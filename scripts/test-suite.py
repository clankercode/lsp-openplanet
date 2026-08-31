#!/usr/bin/env python3
"""B078 — the suite gate: run every test and prove the total is complete.

    just test              # the gate
    just test-selftest     # proves the gate can fail

Why this wrapper exists
-----------------------

``cargo test`` is honest: a test binary killed by a signal makes it exit 101,
and it stops the run there. What is NOT honest is the way a total gets read
back out of it. The habit across this batch was::

    cargo test --locked > log 2>&1
    grep '^test result' log | awk '{s+=$4} END {print s}'

A pipeline reports the status of its LAST command, so ``cargo``'s 101 is
discarded and ``awk`` prints a smaller number with no failure anywhere in
sight. Not hypothetical: totals of **2190 and 2218 against a true 2223** were
observed during the B078 investigation, on ``multi_root_tests``. The whole
batch, and the coordinator accepting its numbers, was reporting totals on this
basis for a day.

Two INDEPENDENT mechanisms produce it, and a fix has to close both:

1. **A crashed binary emits no summary.** Its tests vanish from the sum rather
   than appearing as failures, and cargo then stops, so every binary after it
   never runs and never reports either.
2. **A pipeline discards cargo's exit code.** ``cargo test | grep ... > file``
   leaves the shell holding *grep's* status. The crash signal is simply not in
   the variable that gets checked.

Both shrink the total. Neither shows up as a failed test. The error always
points the same way — **fewer tests, still green** — which is the direction
nobody investigates.

Mechanism 2 cannot be fixed from inside a script: any caller may still pipe
this program's output and drop its exit code. So the total is instead made
*absent* rather than wrong. ``SUITE-VERIFIED`` (see ``VERIFIED_MARKER``) is the
only line here that ever carries a total, and it is printed only on a run that
passed every check. A gate that greps for it gets nothing at all from a broken
run instead of a smaller number.

What this checks
----------------

1. **cargo's exit code**, so the signal is never discarded by a pipeline.
2. **Every expected binary reported a summary.** The expected set comes from a
   SEPARATE ``cargo test --no-run --message-format=json`` enumeration, not from
   the run's own output. That distinction is the whole point: a run that stops
   early emits fewer ``Running`` lines *and* fewer summaries, so checking those
   two against each other agrees with itself and proves nothing. The
   independent list is what makes a missing binary visible.
3. **A started binary that produced no summary**, which is the signature of a
   binary killed mid-run (see B078: an external instruction-fetch fault, not a
   defect in this crate).
4. **The inventory** (B085, ``scripts/test_inventory.py``), run first, before
   a single test executes. Everything above is binary-level and therefore
   blind to a test that vanishes INSIDE a present, healthy, summary-producing
   binary, and blind to a stale ``CARGO_BIN_EXE_*`` child process a test
   spawns — which is not a test binary at all. Both were live during the false
   B074 revert; both shrink the total while every check here stays green.
   Running it first is deliberate: a suite measured against stale binaries
   produces a number, and the number is about older code.

Measured, on why point 2 is not the cheaper same-output check. B082's own Fix
section proposes comparing ``Running`` + ``Doc-tests`` lines against
``test result:`` lines, both taken from the run's output. That catches a
crashed binary — but it cannot tell a COMPLETE run from a deliberately partial
one. On ``cargo test --locked --test b044_narrowed_surfaces`` it reports
``expected=1 reported=1 RECONCILED, 4 passed``: internally consistent, and 2348
tests short of the suite. The independent enumeration fails that same run and
names the 24 binaries that never ran. Internal consistency is not completeness.

Only then does it print a total, because a total is trustworthy only alongside
proof that every binary reported one.

Doctests are a section but not a binary: cargo runs them under ``Doc-tests``
with no enumerable artifact, so they are counted into the total and excluded
from the binary reconciliation.

Relationship to ``test-hermetic.py``
------------------------------------
That gate already refuses to call a binary green without a libtest summary
(``ran = bool(summaries)``). It is the model for this one. It cannot replace
it: it runs each binary directly under an isolated HOME and so covers neither
doctests nor the ordinary ``cargo test`` path that developers and lanes
actually gate on.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_inventory  # noqa: E402  (path set above; sibling script, not a package)

# `     Running unittests src/lib.rs (/abs/path/deps/openplanet_lsp-<hash>)`
RUNNING = re.compile(r"^\s+Running\s+(?P<label>.+?)\s+\((?P<path>[^)]+)\)\s*$")
# `   Doc-tests openplanet_lsp`
DOCTESTS = re.compile(r"^\s+Doc-tests\s+(?P<crate>\S+)\s*$")
# The single greppable token that means "this total is complete". A lane may
# grep for this line; it must never appear on a run that failed any check.
VERIFIED_MARKER = "SUITE-VERIFIED"

SUMMARY = re.compile(
    r"^test result: \S+\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored"
)


# `test some::name ... ok` / `... FAILED` / `... ignored`. Counting these is
# how a binary that died MIDWAY is told from one that never started a test.
TEST_LINE = re.compile(r"^test\s+\S+\s+\.\.\.\s")
# cargo's own post-mortem, e.g.
#   process didn't exit successfully: `/path/foo-abc` (signal: 9, SIGKILL: kill)
#   process didn't exit successfully: `/path/foo-abc` (exit status: 101)
PROCESS_STATUS = re.compile(
    r"process didn't exit successfully: `(?P<cmd>[^`]+)`\s*\((?P<status>[^)]*)\)"
)
SIGNAL_IN_STATUS = re.compile(r"signal:\s*(?P<num>\d+)(?:,\s*(?P<name>SIG\w+))?")

# Signals that mean the code did something wrong, versus ones that mean
# something outside the process killed it. The distinction is the whole point
# of the post-mortem: a SIGSEGV on an idle box and a SIGKILL under load average
# 40 are different findings and must not render as the same line.
DEFECT_SIGNALS = {4: "SIGILL", 6: "SIGABRT", 8: "SIGFPE", 11: "SIGSEGV", 31: "SIGSYS"}
EXTERNAL_SIGNALS = {1: "SIGHUP", 2: "SIGINT", 9: "SIGKILL", 15: "SIGTERM"}


class Section:
    """One `Running <binary>` or `Doc-tests <crate>` block of cargo's output."""

    def __init__(self, label: str, path: str | None) -> None:
        self.label = label
        self.path = path
        self.summaries: list[re.Match[str]] = []
        # Everything below exists so a section that produced NO summary can
        # still say why. Without it every such binary renders as one
        # indistinguishable line and a human has to guess between "real crash",
        # "starved under load" and "something else" — and the failure mode of
        # guessing is dismissing a real crash as "just load", which is how B078
        # nearly went.
        self.test_lines = 0  # individual `test ... ok` lines seen
        self.status: str | None = None  # cargo's post-mortem for this binary


# Cargo flags that narrow WHICH test binaries run. A libtest name filter
# (`just test -- some_test`) is not one of these: every binary still runs and
# filters internally, so the expected set is unchanged.
TARGET_SELECTORS = (
    "--lib",
    "--bin",
    "--bins",
    "--test",
    "--tests",
    "--example",
    "--examples",
    "--bench",
    "--benches",
    "--doc",
)


def split_cargo_args(argv: list[str]) -> tuple[list[str], list[str]]:
    """Args before the first bare ``--`` are cargo's; the rest are libtest's."""
    if "--" in argv:
        cut = argv.index("--")
        return argv[:cut], argv[cut:]
    return argv, []


# Shared-machine throttle. `-j2` caps cargo's BUILD jobs; it does NOT touch
# libtest, which defaults to one thread per CPU — 16 here. Four lanes plus a
# merge gate reached load average 37-41 that way. The repo's standing rule is
# two threads for builds AND tests, so the gate applies both itself rather
# than relying on every caller remembering, and says so when it does.
DEFAULT_BUILD_JOBS = "-j2"
DEFAULT_TEST_THREADS = "--test-threads=2"


def apply_throttle(extra: list[str]) -> tuple[list[str], list[str]]:
    """Return ``(args, notes)`` with build-job and test-thread caps applied.

    An explicit `-j`/`--jobs` or `--test-threads` from the caller always wins:
    the point is a safe default, not a ceiling nobody can raise deliberately.
    """
    cargo_args, libtest_args = split_cargo_args(extra)
    notes: list[str] = []
    if not any(a.startswith(("-j", "--jobs")) for a in cargo_args):
        cargo_args = [DEFAULT_BUILD_JOBS, *cargo_args]
        notes.append(DEFAULT_BUILD_JOBS)
    if not any(a.startswith("--test-threads") for a in libtest_args):
        libtest_args = [*(libtest_args or ["--"]), DEFAULT_TEST_THREADS]
        notes.append(DEFAULT_TEST_THREADS)
    return cargo_args + libtest_args, notes


def names_a_target(cargo_args: list[str]) -> bool:
    """True when the caller deliberately narrowed the set of binaries."""
    return any(
        arg == sel or arg.startswith(f"{sel}=")
        for arg in cargo_args
        for sel in TARGET_SELECTORS
    )


def enumerate_expected_binaries(cargo_args: list[str] | None = None) -> dict[str, str]:
    """Build the test binaries; return ``{executable path: target name}``.

    Deliberately a SECOND cargo invocation rather than a parse of the gate
    run's own output. See the module docstring: an expectation derived from
    the thing it is checking cannot detect a truncated run.

    ``cargo_args`` are forwarded so a deliberately narrowed run
    (``--test foo``) enumerates exactly the binaries it selected. Without
    that, every unselected binary reads as "never ran" and the gate cries
    truncation at a caller who asked for one target.
    """
    proc = subprocess.run(
        [
            "cargo", "test", "--locked", "--no-run", "--message-format=json",
            *([] if any(a.startswith(("-j", "--jobs")) for a in (cargo_args or [])) else [DEFAULT_BUILD_JOBS]),
            *(cargo_args or []),
        ],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"error: `cargo test --no-run` failed (exit {proc.returncode})")

    found: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        executable = msg.get("executable")
        if not executable or not msg.get("profile", {}).get("test"):
            continue
        # Workspace targets only — never a dependency's own test harness.
        manifest = Path(msg.get("manifest_path", "")).resolve()
        if REPO_ROOT not in manifest.parents:
            continue
        found[executable] = msg.get("target", {}).get("name", "?")

    if not found:
        sys.exit("error: no test binaries found in cargo's JSON output")
    return found


def run_and_capture(
    args: list[str], echo: bool = True, watch: "LoadWatch | None" = None
) -> tuple[int, list[str]]:
    """Run cargo, streaming its output through, and keep every line.

    Samples machine load as it goes when given a ``LoadWatch``: a spike that
    starved a test binary has usually decayed by the time the run ends, so a
    single reading at the end cannot see the thing it is there to explain.
    """
    proc = subprocess.Popen(
        args,
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    lines: list[str] = []
    assert proc.stdout is not None
    for line in proc.stdout:
        line = line.rstrip("\n")
        lines.append(line)
        if watch is not None:
            watch.sample()
        if echo:
            print(line, flush=True)
    exit_code = proc.wait()
    if watch is not None:
        watch.sample(force=True)
    return exit_code, lines


def split_sections(lines: list[str]) -> list[Section]:
    """Group cargo's output into `Running` / `Doc-tests` blocks.

    Also gathers, per section, the two things a post-mortem needs: how many
    individual test lines it managed to print before it stopped, and cargo's
    own report of how the process ended. Cargo prints the latter AFTER the
    section, so it is attributed to the binary it names rather than to
    whichever section happened to be current.
    """
    sections: list[Section] = []
    by_path: dict[str, Section] = {}
    for line in lines:
        running = RUNNING.match(line)
        if running:
            section = Section(running["label"], running["path"])
            sections.append(section)
            by_path[running["path"]] = section
            continue
        doctests = DOCTESTS.match(line)
        if doctests:
            sections.append(Section(f"Doc-tests {doctests['crate']}", None))
            continue
        summary = SUMMARY.match(line)
        if summary and sections:
            sections[-1].summaries.append(summary)
            continue
        if TEST_LINE.match(line) and sections:
            sections[-1].test_lines += 1
            continue
        status = PROCESS_STATUS.search(line)
        if status:
            # Attribute by the path cargo names. Falling back to "the current
            # section" would pin a post-mortem on the wrong binary whenever
            # cargo prints it after moving on.
            target = by_path.get(status["cmd"])
            if target is None:
                target = next(
                    (sec for sec in sections if sec.path and sec.path in status["cmd"]),
                    sections[-1] if sections else None,
                )
            if target is not None:
                target.status = status["status"]
    return sections


def explain_status(status: str | None) -> tuple[str, str | None]:
    """`(one-line reading, competing-explanation hint)` for cargo's post-mortem.

    Deliberately does NOT pick a winner between "real defect" and
    "environmental". It names which one the evidence points at and what would
    settle it, because a gate that guesses teaches people to believe the guess.
    """
    if status is None:
        return ("cargo reported no exit status for it", None)
    signal = SIGNAL_IN_STATUS.search(status)
    if not signal:
        return (f"exited with {status}", None)
    num = int(signal["num"])
    name = signal["name"] or DEFECT_SIGNALS.get(num) or EXTERNAL_SIGNALS.get(num) or f"signal {num}"
    if num in DEFECT_SIGNALS:
        return (
            f"killed by {name} (signal {num}) — a fault inside the process",
            "this is the shape of a real defect, not of machine load; do not "
            "dismiss it as contention without evidence",
        )
    if num in EXTERNAL_SIGNALS:
        return (
            f"killed by {name} (signal {num}) — something outside the process "
            f"ended it",
            "an OOM kill, a session teardown or the scheduler; check the load "
            "line below and `dmesg`/`journalctl -k` for an OOM record. It is "
            "NOT evidence that the code is fine",
        )
    return (f"killed by signal {num}", None)


def read_loadavg() -> tuple[float, float, float] | None:
    try:
        parts = Path("/proc/loadavg").read_text().split()
        return (float(parts[0]), float(parts[1]), float(parts[2]))
    except (OSError, ValueError, IndexError):
        return None


class LoadWatch:
    """Samples /proc/loadavg while the suite runs, at most every few seconds.

    A verdict that says "a binary died" without saying what the machine was
    doing forces the reader to guess. Sampling only at the end would miss a
    spike that has since decayed, which is exactly the case worth catching on a
    box several lanes are building in.
    """

    INTERVAL = 5.0

    def __init__(self) -> None:
        self.cpus = os.cpu_count() or 1
        self.samples: list[float] = []
        self._last = 0.0
        self.sample()

    def sample(self, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - self._last < self.INTERVAL:
            return
        self._last = now
        current = read_loadavg()
        if current:
            self.samples.append(current[0])

    def describe(self) -> str:
        if not self.samples:
            return "load: unavailable"
        peak, first, last = max(self.samples), self.samples[0], self.samples[-1]
        note = ""
        if peak > 2 * self.cpus:
            note = (
                f"  <-- {peak / self.cpus:.1f}x the core count; a binary killed "
                f"by an external signal here may well have been starved"
            )
        return (
            f"load: {first:.1f} at start, peak {peak:.1f}, {last:.1f} at end, "
            f"on {self.cpus} cores{note}"
        )


def reconcile(
    exit_code: int,
    lines: list[str],
    expected: dict[str, str],
    filtered: bool = False,
    declared: dict[str, int] | None = None,
    load: str | None = None,
) -> tuple[int, str]:
    """The gate's verdict. Pure, so the selftest exercises this exact code.

    ``declared`` maps a binary's path to how many tests its SOURCE declares
    (from the B085 inventory). It turns "this binary produced no summary" into
    "it printed 37 of its 118 tests and then stopped", which is the difference
    between a fact and a diagnosis. ``load`` is the machine's load description
    at the time, passed in rather than read here so this stays pure.

    Returns ``(status, report)`` where status is 0 for a trustworthy green.
    """
    sections = split_sections(lines)
    problems: list[str] = []

    started_without_summary = [s for s in sections if s.path and not s.summaries]
    reported_paths = {s.path for s in sections if s.path and s.summaries}
    missing = {p: n for p, n in expected.items() if p not in reported_paths}

    passed = sum(int(m["passed"]) for s in sections for m in s.summaries)
    failed = sum(int(m["failed"]) for s in sections for m in s.summaries)
    ignored = sum(int(m["ignored"]) for s in sections for m in s.summaries)

    for section in started_without_summary:
        name = Path(section.path or "?").name
        reading, hint = explain_status(section.status)
        progress = f"printed {section.test_lines} test line(s)"
        want = (declared or {}).get(section.path or "")
        if want:
            progress += f" of the {want} its source declares"
        if section.test_lines == 0:
            progress += " — it died before running a single test"
        problems.append(
            f"`{name}` started and produced no libtest summary — killed "
            f"mid-run, so its tests are missing from the total.\n"
            f"      how it ended: {reading}\n"
            f"      progress:     {progress}"
            + (f"\n      note:         {hint}" if hint else "")
        )
    for path, name in sorted(missing.items(), key=lambda kv: kv[1]):
        if any(s.path == path for s in sections):
            continue  # already reported as started-without-summary
        problems.append(f"`{name}` never ran — the run stopped before reaching it")
    if exit_code != 0:
        problems.append(f"cargo exited {exit_code}")
    if failed:
        problems.append(f"{failed} test(s) failed")

    report: list[str] = []
    report.append("")
    if not problems:
        # A deliberately narrowed run is not a broken one. Reporting it as a
        # failure is the same mislabelling the COMPLETE/INCOMPLETE split just
        # fixed one level up: it puts the word FAIL in front of a caller who
        # asked for exactly what they got, and teaches them to read past it.
        #
        # It still withholds the marker, because a subset is not a suite
        # total — and that is the whole guarantee `SUITE-VERIFIED` carries.
        if filtered:
            report.append(
                f"==> FILTERED RUN: {len(reported_paths)}/{len(expected)} selected "
                f"binaries reported, {len(sections)} sections"
            )
            report.append(
                f"    {passed} pass / {failed} fail / {ignored} ignored — a subset, "
                "not a suite total."
            )
            # Deliberately does NOT name the marker: a gate greps for that
            # exact token, and printing it inside an explanation would match.
            report.append(
                "    a verified suite total is withheld — run `just test` with no "
                "target filter for a number you can report."
            )
            return 0, "\n".join(report)
        report.append(
            f"==> {len(reported_paths)}/{len(expected)} binaries reported, "
            f"{len(sections)} sections"
        )
        # VERIFIED_MARKER is the only line this script ever prints that carries
        # a total, and it is printed only on a run that passed every check.
        # That is what makes the gate survive the second mechanism: a lane
        # whose pipeline discards the exit code still gets NOTHING from a
        # broken run rather than a smaller number, because the line it greps
        # for is absent. Do not print a total anywhere else.
        report.append(
            f"{VERIFIED_MARKER}: {passed} passed; {failed} failed; {ignored} ignored; "
            f"{len(reported_paths)}/{len(expected)} binaries reported"
        )
        return 0, "\n".join(report)

    # Two different bad outcomes, and calling them both "incomplete" would be
    # wrong: a suite where every binary reported and some tests FAILED is
    # complete and red, which is the ordinary case a lane sees while working.
    # Only a run missing binaries is a fragment.
    #
    # Neither branch prints the "N passed" shape a summing gate matches, and
    # neither prints VERIFIED_MARKER. See VERIFIED_MARKER above.
    if started_without_summary or missing:
        report.append(
            f"==> INCOMPLETE RUN: {len(reported_paths)} of {len(expected)} binaries "
            f"reported. The counts below are a fragment, not a total."
        )
        report.append(f"    fragment: {passed} pass / {failed} fail / {ignored} ignored")
    else:
        report.append(
            f"==> COMPLETE RUN, RED: all {len(expected)} binaries reported, "
            f"{len(sections)} sections"
        )
        report.append(f"    {passed} pass / {failed} fail / {ignored} ignored")
    report.append("")
    report.append(
        "FAIL: this run did not produce a trustworthy total."
        if (started_without_summary or missing)
        else "FAIL: the run was complete, but tests failed."
    )
    for problem in problems:
        report.append(f"  - {problem}")
    report.append("")
    if load:
        report.append(f"  {load}")
        report.append("")
    report.append(
        "A binary that dies contributes no `test result:` line, and cargo stops\n"
        "at the first failure, so everything after it never runs either. Both\n"
        "shrink the total without producing a single failed test. Do not report\n"
        "the number above. See B078.\n"
        "\n"
        "This gate does NOT retry. An intermittent that a retry turns green is\n"
        "an intermittent nobody investigates, which is worse than a red run: it\n"
        "converts a defect into silence. Re-run it yourself, deliberately, and\n"
        "compare the post-mortem above across runs (B085)."
    )
    return 1, "\n".join(report)


def run_gate(extra: list[str]) -> int:
    cargo_args, _libtest_args = split_cargo_args(extra)
    filtered = names_a_target(cargo_args)

    # B085, first: the binaries about to run must contain the tests their
    # sources declare, and nothing they measure may be older than the tree.
    # A suite run against a stale binary still produces a total.
    inventory_status, inventory_report, declared = test_inventory.check(cargo_args)
    print(inventory_report, file=sys.stderr if inventory_status else sys.stdout)
    if inventory_status:
        print(
            "\nFAIL: the test inventory is not trustworthy, so the suite was not\n"
            "run. Any total from these binaries would be about other code.",
            file=sys.stderr,
        )
        return inventory_status
    print(flush=True)

    expected = enumerate_expected_binaries(cargo_args)
    scope = "selected" if filtered else "expected"
    throttled, notes = apply_throttle(extra)
    if notes:
        print(f"==> throttled: {' '.join(notes)} (shared machine; override to opt out)")
    print(f"==> {len(expected)} test binaries {scope}\n", flush=True)
    watch = LoadWatch()
    exit_code, lines = run_and_capture(
        ["cargo", "test", "--locked", *throttled], watch=watch
    )
    status, report = reconcile(
        exit_code,
        lines,
        expected,
        filtered=filtered,
        declared=declared,
        load=watch.describe(),
    )
    print(report, file=sys.stderr if status else sys.stdout)
    return status


def run_selftest() -> int:
    """Positive control: the gate must go RED on a genuinely crashed binary.

    Runs the `#[ignore]`d canary in ``tests/zz_crash_canary.rs``, which aborts
    the process, and asserts the reconciliation above rejects the run. Without
    this the gate could only ever say yes — which is the same failure it exists
    to catch, one level up.
    """
    expected = enumerate_expected_binaries()
    canary = {p: n for p, n in expected.items() if n == "zz_crash_canary"}
    if not canary:
        sys.exit("error: test binary `zz_crash_canary` not found — was it deleted?")

    print("==> running the crash canary under the real gate logic\n", flush=True)
    exit_code, lines = run_and_capture(
        ["cargo", "test", "--locked", "--test", "zz_crash_canary", "--", "--ignored"],
        echo=False,
    )
    status, report = reconcile(exit_code, lines, canary)
    print(report)

    problems: list[str] = []
    if VERIFIED_MARKER in report:
        problems.append(
            f"a failed run printed `{VERIFIED_MARKER}` — the marker no longer means "
            "'complete', so a gate grepping for it would accept a truncated run"
        )
    if exit_code == 0:
        problems.append(
            "the canary did not crash cargo (exit 0) — it is no longer aborting, "
            "so this control proves nothing"
        )
    if status == 0:
        problems.append(
            "the gate PASSED a crashed binary — it cannot fail, and every total "
            "it has ever blessed is unverified"
        )
    if any(SUMMARY.match(line) for line in lines):
        problems.append(
            "the crashed binary emitted a libtest summary — the canary is not "
            "aborting where this gate assumes it does"
        )

    # The other half of the damage, checked against the same real output: when
    # a binary dies cargo stops, so binaries AFTER it never run and never
    # report. Reconciling that captured run against an expected set containing
    # a binary cargo never reached must name it. The extra entry is synthetic
    # because staging it for real would mean running an unrelated `#[ignore]`d
    # binary, several of which need a live game (B068).
    # A COMPLETE run whose tests merely failed must not be called incomplete —
    # that is the ordinary red suite a lane sees while working, and mislabelling
    # it teaches people to ignore the word. It must still withhold the marker.
    # Synthetic input, because producing it for real would mean committing a
    # failing test.
    # A deliberately filtered run must succeed WITHOUT the marker. Both
    # halves matter: failing it is the friction this branch removes, and
    # emitting the marker would let a subset be reported as a suite total.
    one = "/deps/one-0000000000000000"
    filtered_ok = [
        f"     Running tests/one.rs ({one})",
        "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; "
        "0 filtered out; finished in 0.01s",
    ]
    filt_status, filt_report = reconcile(0, filtered_ok, {one: "one"}, filtered=True)
    if filt_status != 0:
        problems.append("a clean filtered run was reported as a failure")
    if VERIFIED_MARKER in filt_report:
        problems.append(
            f"a filtered run printed `{VERIFIED_MARKER}` — a subset would be "
            "reportable as a suite total"
        )
    if not names_a_target(["--test", "foo"]) or names_a_target(["-j2"]):
        problems.append("target-selector detection is wrong")
    if split_cargo_args(["-j2", "--", "--test-threads=2"]) != (
        ["-j2"],
        ["--", "--test-threads=2"],
    ):
        problems.append("cargo/libtest argument split is wrong")

    example = "/deps/example-0000000000000000"
    complete_red = [
        f"     Running tests/example.rs ({example})",
        "test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; "
        "0 filtered out; finished in 0.01s",
    ]
    red_status, red_report = reconcile(101, complete_red, {example: "example"})
    if red_status == 0:
        problems.append("a run with failing tests was accepted as green")
    if "INCOMPLETE" in red_report:
        problems.append(
            "a complete run whose tests failed was labelled INCOMPLETE — the word "
            "no longer distinguishes a truncated run from a red one"
        )
    if VERIFIED_MARKER in red_report:
        problems.append(f"a run with failing tests printed `{VERIFIED_MARKER}`")

    never_reached = dict(canary)
    never_reached["/nonexistent/deps/never_reached-0000000000000000"] = "never_reached"
    stop_status, stop_report = reconcile(exit_code, lines, never_reached)
    if stop_status == 0 or "never ran" not in stop_report:
        problems.append(
            "a binary the run never reached was not reported as missing — the "
            "stop-early half of the check is not working"
        )

    # B085 post-mortem: an INCOMPLETE RUN verdict is worthless if it renders a
    # real crash and a starved process as the same line — the reader has to
    # guess, and the failure mode of guessing is dismissing a crash as "just
    # load". These drive the real `reconcile` with synthetic cargo output.
    seg = "/deps/segv-0000000000000000"
    segv_lines = [
        f"     Running tests/segv.rs ({seg})",
        "test alpha ... ok",
        "test beta ... ok",
        f"error: test failed, to rerun pass `--test segv`",
        f"Caused by:",
        f"  process didn't exit successfully: `{seg}` (signal: 11, SIGSEGV: invalid memory reference)",
    ]
    _st, segv_report = reconcile(
        101, segv_lines, {seg: "segv"}, declared={seg: 118}, load="load: 0.4 at start, peak 0.6, 0.5 at end, on 16 cores"
    )
    kill_lines = [
        f"     Running tests/segv.rs ({seg})",
        f"  process didn't exit successfully: `{seg}` (signal: 9, SIGKILL: kill)",
    ]
    _st, kill_report = reconcile(
        101, kill_lines, {seg: "segv"}, declared={seg: 118},
        load="load: 38.0 at start, peak 41.2, 39.0 at end, on 16 cores",
    )
    if "SIGSEGV" not in segv_report or "signal 11" not in segv_report:
        problems.append("a SIGSEGV death did not name the signal in the verdict")
    if "SIGKILL" not in kill_report:
        problems.append("a SIGKILL death did not name the signal in the verdict")
    if segv_report == kill_report:
        problems.append(
            "a SIGSEGV on an idle box and a SIGKILL under load render "
            "IDENTICALLY — the verdict cannot distinguish a real crash from "
            "contention, which is the ambiguity this post-mortem exists to remove"
        )
    if "2 test line(s) of the 118" not in segv_report:
        problems.append(
            "partial progress was not reported — 'produced no summary' without "
            "'got 2 of 118' is a fact with no diagnosis attached"
        )
    if "died before running a single test" not in kill_report:
        problems.append("a binary that ran no tests at all was not distinguished")
    if "41.2" not in kill_report or "16 cores" not in kill_report:
        problems.append("the machine load was not reported alongside the death")
    # The load READING has to come from the real describe(), not from a string
    # this selftest wrote. Asserting on a fabricated `load=` argument would be
    # checking the fixture, which is how a control ends up proving nothing.
    busy, idle = LoadWatch(), LoadWatch()
    busy.cpus = idle.cpus = 16
    busy.samples = [38.0, 41.2, 39.0]
    idle.samples = [0.4, 0.6, 0.5]
    if "x the core count" not in busy.describe():
        problems.append(
            "a load of 41 on 16 cores was not flagged as a plausible starvation "
            "cause, so the reader gets the number without the reading"
        )
    if "x the core count" in idle.describe():
        problems.append(
            "an idle box was described as loaded — the starvation hint fires "
            "regardless of load and therefore says nothing"
        )
    if "peak 41.2" not in busy.describe() or "16 cores" not in busy.describe():
        problems.append("the load description lost its peak or its core count")
    empty = LoadWatch()
    empty.samples = []
    if "unavailable" not in empty.describe():
        problems.append(
            "a run with no load samples claimed a load anyway — an unmeasured "
            "value must read as unmeasured"
        )
    if "does NOT retry" not in segv_report:
        problems.append(
            "the verdict no longer states that it does not retry; a gate that "
            "retries until green converts an intermittent into silence"
        )
    # Attribution: cargo prints its post-mortem AFTER the section, and pinning
    # it on 'the current section' would blame the wrong binary.
    one, two = "/deps/one-0000000000000000", "/deps/two-0000000000000000"
    misattrib = [
        f"     Running tests/one.rs ({one})",
        f"     Running tests/two.rs ({two})",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s",
        f"  process didn't exit successfully: `{one}` (signal: 11, SIGSEGV: x)",
    ]
    _st, attrib_report = reconcile(101, misattrib, {one: "one", two: "two"})
    if "SIGSEGV" not in attrib_report or "one-" not in attrib_report:
        problems.append(
            "a post-mortem naming binary `one` was not attributed to it — "
            "cargo prints it after the section, so 'the current section' is "
            "the wrong binary"
        )
    if not apply_throttle([])[1] or "-j2" not in apply_throttle([])[0]:
        problems.append("the shared-machine throttle is not applied by default")
    if "--test-threads=2" not in apply_throttle([])[0]:
        problems.append("libtest is not throttled by default — `-j2` does not cap it")
    if "--test-threads=8" not in apply_throttle(["--", "--test-threads=8"])[0]:
        problems.append("an explicit --test-threads was overridden by the default")
    explicit_jobs = apply_throttle(["-j4"])[0]
    if DEFAULT_BUILD_JOBS in explicit_jobs or "-j4" not in explicit_jobs:
        problems.append(
            f"an explicit -j4 was not respected: got {explicit_jobs}"
        )
    if apply_throttle(["-j4"])[1] != [DEFAULT_TEST_THREADS]:
        problems.append("the throttle notes do not name only what was injected")

    # B085 phase: a failed inventory must stop the gate BEFORE any test runs,
    # and must never print the marker. The inventory is stubbed rather than a
    # real binary being staled, because staling one means a rebuild; the
    # inventory's own ability to detect real staleness is proved separately by
    # `just test-inventory-selftest` and `just test-inventory-mutants`.
    marker = "SYNTHETIC-INVENTORY-FAILURE"
    real_check = test_inventory.check
    captured_out, captured_err = io.StringIO(), io.StringIO()
    test_inventory.check = lambda *_a, **_k: (1, f"FAIL: {marker}", {})
    try:
        with contextlib.redirect_stdout(captured_out), contextlib.redirect_stderr(
            captured_err
        ):
            gate_status = run_gate([])
    finally:
        test_inventory.check = real_check
    captured = captured_out.getvalue() + captured_err.getvalue()
    if marker not in captured:
        problems.append(
            "the stubbed inventory never ran — this control proves nothing, so "
            "the gate's inventory phase is untested here"
        )
    if gate_status == 0:
        problems.append(
            "a failed inventory did not fail the gate — a suite measured "
            "against stale or incomplete binaries would still be reported"
        )
    if VERIFIED_MARKER in captured:
        problems.append(
            f"a failed inventory still printed `{VERIFIED_MARKER}`"
        )
    if "test result:" in captured:
        problems.append(
            "the gate ran tests after the inventory failed — it must stop first"
        )

    if problems:
        print("\nFAIL: suite-gate self-test", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        print("\nFull canary output follows.\n", file=sys.stderr)
        print("\n".join(lines), file=sys.stderr)
        return 1

    print(
        "\nPASS: the gate turns red on a crashed binary, and does so because the\n"
        "binary produced no summary — not merely because cargo exited non-zero.\n"
        "A failed inventory (B085) stops it before a single test runs."
    )
    return 0


USAGE = """usage: test-suite.py [--selftest] [cargo args...]

  --selftest   run the crash canary and check the gate rejects the run
  anything else is forwarded VERBATIM to `cargo test --locked`, e.g.

      python3 scripts/test-suite.py -- --test-threads=2
"""


def main() -> int:
    # Deliberately not argparse: it consumes the first bare `--`, so
    # `just test -- --test-threads=2` reached cargo as `--test-threads=2` and
    # cargo rejected it. Forwarding argv verbatim is the only way to keep the
    # libtest separator intact.
    argv = sys.argv[1:]
    if "-h" in argv or "--help" in argv:
        print(USAGE)
        return 0
    if "--selftest" in argv:
        if argv != ["--selftest"]:
            sys.exit("error: --selftest takes no other arguments")
        return run_selftest()
    return run_gate(argv)


if __name__ == "__main__":
    sys.exit(main())
