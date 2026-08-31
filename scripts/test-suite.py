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
sight. That is not hypothetical: lane-b060-b062 reported 2190/2218 against a
true 2223 after a SIGSEGV in ``multi_root_tests``, and totals were accepted
from five lanes on that basis for a day.

Two things go wrong at once when a binary dies, and a total hides both:

* the crashed binary contributes **no** ``test result:`` line, so its tests
  vanish from the sum rather than appearing as failures; and
* cargo stops at the first failing binary, so every binary after it never runs
  and never reports either.

Both shrink the total. Neither shows up as a failed test.

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

import json
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# `     Running unittests src/lib.rs (/abs/path/deps/openplanet_lsp-<hash>)`
RUNNING = re.compile(r"^\s+Running\s+(?P<label>.+?)\s+\((?P<path>[^)]+)\)\s*$")
# `   Doc-tests openplanet_lsp`
DOCTESTS = re.compile(r"^\s+Doc-tests\s+(?P<crate>\S+)\s*$")
SUMMARY = re.compile(
    r"^test result: \S+\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored"
)


class Section:
    """One `Running <binary>` or `Doc-tests <crate>` block of cargo's output."""

    def __init__(self, label: str, path: str | None) -> None:
        self.label = label
        self.path = path
        self.summaries: list[re.Match[str]] = []


def enumerate_expected_binaries() -> dict[str, str]:
    """Build the test binaries; return ``{executable path: target name}``.

    Deliberately a SECOND cargo invocation rather than a parse of the gate
    run's own output. See the module docstring: an expectation derived from
    the thing it is checking cannot detect a truncated run.
    """
    proc = subprocess.run(
        ["cargo", "test", "--locked", "--no-run", "--message-format=json"],
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


def run_and_capture(args: list[str], echo: bool = True) -> tuple[int, list[str]]:
    """Run cargo, streaming its output through, and keep every line."""
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
        if echo:
            print(line, flush=True)
    return proc.wait(), lines


def split_sections(lines: list[str]) -> list[Section]:
    """Group cargo's output into `Running` / `Doc-tests` blocks."""
    sections: list[Section] = []
    for line in lines:
        running = RUNNING.match(line)
        if running:
            sections.append(Section(running["label"], running["path"]))
            continue
        doctests = DOCTESTS.match(line)
        if doctests:
            sections.append(Section(f"Doc-tests {doctests['crate']}", None))
            continue
        summary = SUMMARY.match(line)
        if summary and sections:
            sections[-1].summaries.append(summary)
    return sections


def reconcile(exit_code: int, lines: list[str], expected: dict[str, str]) -> tuple[int, str]:
    """The gate's verdict. Pure, so the selftest exercises this exact code.

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
        problems.append(
            f"`{Path(section.path or '?').name}` started and produced no libtest "
            f"summary — killed mid-run, so its tests are missing from the total"
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
    report.append(
        f"==> {len(reported_paths)}/{len(expected)} binaries reported, "
        f"{len(sections)} sections, {passed} passed, {failed} failed, {ignored} ignored"
    )
    if not problems:
        report.append(
            f"PASS: {passed} passed, and every one of the {len(expected)} expected "
            "binaries reported a result."
        )
        return 0, "\n".join(report)

    report.append("")
    report.append("FAIL: this total is NOT trustworthy.")
    for problem in problems:
        report.append(f"  - {problem}")
    report.append("")
    report.append(
        "A binary that dies contributes no `test result:` line, and cargo stops\n"
        "at the first failure, so everything after it never runs either. Both\n"
        "shrink the total without producing a single failed test. Do not report\n"
        "the number above. See B078."
    )
    return 1, "\n".join(report)


def run_gate(extra: list[str]) -> int:
    expected = enumerate_expected_binaries()
    print(f"==> {len(expected)} test binaries expected\n", flush=True)
    exit_code, lines = run_and_capture(["cargo", "test", "--locked", *extra])
    status, report = reconcile(exit_code, lines, expected)
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
    never_reached = dict(canary)
    never_reached["/nonexistent/deps/never_reached-0000000000000000"] = "never_reached"
    stop_status, stop_report = reconcile(exit_code, lines, never_reached)
    if stop_status == 0 or "never ran" not in stop_report:
        problems.append(
            "a binary the run never reached was not reported as missing — the "
            "stop-early half of the check is not working"
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
        "binary produced no summary — not merely because cargo exited non-zero."
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
