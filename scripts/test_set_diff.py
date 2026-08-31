#!/usr/bin/env python3
"""B163: a test-SET diff, because a reconciling total cannot see an even swap.

THE DEFECT THIS EXISTS FOR (B158, lane-enum). A test insertion landed between
`#[test]` and an existing test. One test was silently disabled and one was
double-registered. The suite total reconciled EXACTLY -- +1 and -1 cancel -- so
the delta arithmetic every lane relies on produced a clean green over a disabled
test. Counting cannot distinguish a set from its cardinality, and the cheapest
even swap (one disabled while one is added) is also the likeliest, because it is
what a misplaced insertion produces by default.

The exactness is the hazard, not the miss. A total that came out NEAR the
expected number would have been investigated; coming out precisely right retired
the question. This defect gets WORSE as arithmetic discipline improves.

WHY `--list` AND NOT THE RUN LOG. lane-enum's first ad-hoc set diff parsed the
run log and reported a test added in a file it had never touched: a stderr
warning from a parallel test interleaved into an `... ok` line and defeated its
`^test` anchor. The polarity is what makes that dangerous -- it produced a
phantom ADDITION, but landing on one of the lane's own lines it would have
reported a REMOVAL, the alarm a lane would have believed and acted on. Each
binary's `--list` is not interleaved with test-body output and needs no
`--test-threads=1`, so this removes the exposure at the source instead of
hardening a regex against it.

WHY THE BINARY LIST COMES FROM CARGO'S JSON. `target/debug/deps/` holds several
artifacts matching a crate name -- stale harnesses, rlibs, non-test executables.
While this ticket was being measured, a `deps/` probe picked
`openplanet_lsp-d408a682e854e302`, listed 0 tests, and came one step from the
conclusion "the lib harness lists nothing"; the real harness was
`-418c7f45a6ea779a` with 2176. A set diff over ZERO binaries reports zero
removals and reconciles perfectly, so a glob would have given the instrument
built to backstop a vacuous check a vacuous first step of its own. Binary paths
therefore come from `cargo test --no-run --message-format=json`, via
`test_inventory.enumerate_artifacts` -- the same proven enumeration the
inventory gate already uses, NOT a second implementation of it.

NAMES ARE QUALIFIED BY BINARY, AND THAT IS MEASURED, NOT TIDINESS. Integration
tests are listed bare, so bare names collide ACROSS binaries: this tree has
`anti_vacuity_the_harness_reports_on_this_shape` in four separate binaries. In a
flat bare-name set, deleting it from three of the four is invisible -- a
removal blind spot in the removal detector. Qualified `<binary>::<test>`, this
tree has zero collisions.

TWO DISABLE MECHANISMS, ONE FILE. A test can be disabled two ways with two
different signatures:

  * the attribute is swallowed (the B158 incident) -> the name DISAPPEARS
    from `--list`
  * `#[ignore]` is added -> the name STAYS in `--list` and appears in
    `--list --ignored`

So a name-set alone would catch B158 and miss `#[ignore]` entirely. Both sets
are recorded, in one file, by marking ignored names with a trailing
`[ignored]`. A test going active -> ignored is then a changed LINE rather than
an invisible no-op, which renders it in the git diff next to the commit that
did it.

KNOWN BLIND SPOT, STATED HERE ON PURPOSE: doctests. `--list` on a test binary
does not enumerate them, so this tree's 4 doctests are outside this check
entirely. It reconciles: 3090 listed names + 4 doctests == 3082 passed + 12
ignored. A check whose gap is undocumented gets trusted past its range, which
is exactly how the suite total came to be trusted as a set check.

WHY THE BASELINE IS COMMITTED rather than built from the merge base. Two
reasons, and the second is a hard constraint:

  * A committed baseline makes a removed test a DELETED LINE IN THE GIT DIFF,
    inside the change a reviewer is already reading. A live merge-base build
    produces the same finding into a gate log nobody reads twice. That also
    disposes of the obvious objection -- "a lane can re-record and hide a
    removal" -- better than a refusal guard does: re-recording does not conceal
    a removal, it RENDERS it, and the bypass leaves the clearest possible trace.
  * A live merge-base build needs a SECOND target dir per lane (reusing the
    lane's own thrashes the incremental cache between two source trees, which is
    why each worktree already gets its own). Measured on x-alpha 2026-09-01:
    45G free of 458G, 90% full, with ~82GB already in six per-lane target dirs
    against B106's 10GiB floor. At ~14GB each, four concurrent lanes add ~56GB
    against 45GB free. That is the B106 incident verbatim -- gates exiting 0
    while builds failed with ENOSPC. It would have bought a set diff by putting
    a false-green route back into every other gate on the box.

FILE FORMAT IS LOAD-BEARING. Every lane that adds a test touches this file, so
with four concurrent lanes it is a guaranteed conflict surface. It is a pure
sorted list, one name per line, with NO counts, totals, or metadata header: such
a list conflicts additively and resolves by union. A trailing total would turn
every concurrent addition into a manual adjudication, and that is how this gets
reverted in a week for making merges painful. The denominators live in this
tool's OUTPUT instead, where they cost no merge surface.

This is NOT `clippy_delta.py` copied wholesale. It reuses the shape -- committed,
recorded on master, compared at gate time -- and deliberately drops two things.
Clippy carries toolchain and command in-file because lint FINDINGS vary by
toolchain; test NAMES do not. They are not purely source-derived either (this
tree has 10 `#[cfg(unix)]` tests), but the real axis is PLATFORM/FEATURES, not
rustc version, and it is latent while every gate runs Linux. Guarding it the
clippy way would cost an in-file header line that EVERY re-record touches, i.e.
the guaranteed conflict this format exists to avoid.

WHAT IS ADDED THAT CLIPPY HAS NO NEED FOR: an emptiness refusal. A pure sorted
list has a hole clippy's dict does not -- an empty or truncated baseline yields
zero expected names, so every test reads as ADDED, REMOVALS COME OUT ZERO, and
the diff reconciles perfectly. That is this ticket's own defect arriving in the
step that feels most like plumbing. Every population step here therefore prints
its denominator and refuses its own zero: binaries enumerated, names listed,
baseline read.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_inventory  # noqa: E402  (path set above; sibling script, not a package)

DEFAULT_BASELINE = REPO_ROOT / "docs" / "test-baseline.txt"
VERIFIED_MARKER = "TEST-SET-VERIFIED"
RECORD_RECIPE = "just rtest-baseline-record"

# libtest's `--list` emits `<name>: test` (benchmarks say `: bench`).
# `test_inventory.LISTED_TEST` only has to COUNT these lines, so it is the
# suffix alone; this one has to capture the name. Two regexes over one format
# can drift, so the selftest requires them to agree line for line.
LISTED_NAME = re.compile(r"^(?P<name>.+): test$")

IGNORED_MARK = " [ignored]"

# An empty or truncated baseline makes every test read as ADDED and reports
# ZERO removals -- a perfect reconciliation over a destroyed expectation. This
# floor is deliberately far below the real figure (3090 at the time of writing)
# and exists to catch truncation, not to track the suite's size. The suite has
# never been near it; a baseline below it is a broken file, not a small suite.
MIN_PLAUSIBLE_BASELINE = 1000


# --------------------------------------------------------------------------
# Reading this tree
# --------------------------------------------------------------------------


def list_names(exe: str, ignored: bool = False) -> list[str] | None:
    """`<exe> --list [--ignored]` -> the test names it reports, or None."""
    proc = subprocess.run(
        [exe, "--list", *(["--ignored"] if ignored else [])],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    return [
        m.group("name")
        for line in proc.stdout.splitlines()
        if (m := LISTED_NAME.match(line))
    ]


def collect(artifacts: list[dict]) -> tuple[dict[str, bool], list[str], dict[str, int]]:
    """Qualified name -> is-ignored, for every test binary cargo named.

    Returns (names, problems, per_binary_counts). Names are `<binary>::<test>`
    because bare integration-test names collide across binaries (measured: four
    binaries share one name in this tree), and a collision in a flat set is a
    removal this tool could not see.
    """
    names: dict[str, bool] = {}
    problems: list[str] = []
    per_binary: dict[str, int] = {}

    harnesses = [a for a in artifacts if a["profile"]["test"]]
    if not harnesses:
        problems.append(
            "cargo named ZERO test binaries, so this diff would compare an "
            "empty tree against the baseline and report every test REMOVED "
            "(or, against an empty baseline, nothing at all)"
        )
        return names, problems, per_binary

    for msg in harnesses:
        binary = msg["target"]["name"]
        exe = msg["executable"]
        active = list_names(exe)
        if active is None:
            problems.append(f"`{binary}`: `--list` could not be read from {exe}")
            continue
        ignored = list_names(exe, ignored=True)
        if ignored is None:
            problems.append(
                f"`{binary}`: `--list --ignored` could not be read from {exe} — "
                f"without it a test disabled by `#[ignore]` is invisible here"
            )
            continue
        per_binary[binary] = len(active)
        # A binary listing zero tests is NOT refused here, and that is measured
        # rather than assumed: `openplanet-lsp` is the bin's own test target
        # and legitimately holds none (listed 0, declared 0). Refusing it made
        # this tool over-condemn on its very first real run. Whether a
        # PARTICULAR binary should hold tests is a listed-vs-declared question,
        # which `test_inventory` already answers per binary against the sources
        # and gates on -- and `--record` requires that gate to pass. What is
        # refused here is the aggregate zero, below, which is the shape that
        # makes this diff vacuous.
        ignored_set = set(ignored)
        stray = ignored_set - set(active)
        if stray:
            problems.append(
                f"`{binary}`: {len(stray)} name(s) in `--list --ignored` are "
                f"absent from `--list`, so the two sets do not nest as this "
                f"tool assumes: {sorted(stray)[:3]}"
            )
        for name in active:
            if " " in name:
                problems.append(
                    f"`{binary}`: test name {name!r} contains a space, which "
                    f"would be parsed as the `[ignored]` marker"
                )
                continue
            names[f"{binary}::{name}"] = name in ignored_set

    if not names and not problems:
        problems.append(
            f"{len(per_binary)} binaries were enumerated and between them they "
            f"listed ZERO tests. Every name in the baseline would read as "
            f"REMOVED, or -- against an empty baseline -- the diff would "
            f"reconcile perfectly over nothing at all"
        )
    return names, problems, per_binary


# --------------------------------------------------------------------------
# The baseline file
# --------------------------------------------------------------------------


def render_baseline(names: dict[str, bool]) -> str:
    """Sorted, one name per line, no counts and no metadata header.

    The format is the merge story: a pure sorted list conflicts additively and
    resolves by union, so four concurrent lanes each adding a test do not
    adjudicate anything. Any total in this file would make every one of those
    additions a manual conflict.

    The order is Python's `sorted`, i.e. CODEPOINT order, which is what
    `LC_ALL=C sort` produces and NOT what a bare `sort` produces under a UTF-8
    locale (that one folds underscores and would reorder this file). The
    codepoint order is the deliberate choice: a locale-dependent order would
    make the file's contents depend on the recorder's machine settings, so two
    lanes on differently-configured shells would produce spurious whole-file
    diffs and destroy the additive-merge property this format exists for. If
    you are checking it by hand, use `LC_ALL=C sort -c`.
    """
    return "".join(
        f"{name}{IGNORED_MARK if names[name] else ''}\n" for name in sorted(names)
    )


def parse_baseline(text: str) -> tuple[dict[str, bool], list[str]]:
    names: dict[str, bool] = {}
    problems: list[str] = []
    previous: str | None = None
    for lineno, raw in enumerate(text.splitlines(), 1):
        if not raw:
            problems.append(
                f"baseline line {lineno}: blank lines are not part of the pure "
                f"one-name-per-line format"
            )
            continue
        if raw.lstrip().startswith("#"):
            problems.append(
                f"baseline line {lineno}: metadata/comments are not allowed; "
                f"they create a shared line every recorder would edit"
            )
            continue
        if raw != raw.rstrip():
            problems.append(f"baseline line {lineno}: trailing whitespace")
            continue
        line = raw
        ignored = line.endswith(IGNORED_MARK)
        name = line[: -len(IGNORED_MARK)] if ignored else line
        if " " in name:
            problems.append(f"baseline line {lineno}: unparseable entry {raw!r}")
            continue
        if name in names:
            problems.append(f"baseline line {lineno}: duplicate entry {name!r}")
            continue
        if previous is not None and name < previous:
            problems.append(
                f"baseline line {lineno}: {name!r} is out of codepoint order "
                f"after {previous!r}; use `LC_ALL=C sort`"
            )
        names[name] = ignored
        previous = name
    return names, problems


def read_baseline(path: Path) -> tuple[dict[str, bool], list[str]]:
    if not path.exists():
        return {}, [
            f"no baseline at {_rel(path)}. This check compares the tests in "
            f"this tree against a committed list; without one it has no "
            f"expectation and would report zero removals no matter what was "
            f"deleted. Record it on master with `{RECORD_RECIPE}`."
        ]
    names, problems = parse_baseline(path.read_text())
    if not names:
        problems.append(
            f"the baseline at {_rel(path)} holds ZERO names. Every test in "
            f"this tree would read as ADDED and REMOVALS WOULD COME OUT ZERO — "
            f"a perfect-looking reconciliation over a destroyed expectation, "
            f"which is the exact defect this check exists to catch."
        )
    elif len(names) < MIN_PLAUSIBLE_BASELINE:
        problems.append(
            f"the baseline at {_rel(path)} holds only {len(names)} names, "
            f"below the {MIN_PLAUSIBLE_BASELINE} floor. This suite has held "
            f"thousands since the file was created, so this is a truncated or "
            f"half-written file, not a small suite. A short baseline hides "
            f"removals by reporting them as never having been there."
        )
    return names, problems


def _rel(path: Path) -> str:
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


# --------------------------------------------------------------------------
# The comparison
# --------------------------------------------------------------------------


def evaluate(
    baseline: dict[str, bool], current: dict[str, bool]
) -> tuple[list[str], list[str], list[str], list[str]]:
    """-> (removed, added, newly_ignored, no_longer_ignored), each sorted."""
    removed = sorted(set(baseline) - set(current))
    added = sorted(set(current) - set(baseline))
    both = set(baseline) & set(current)
    newly_ignored = sorted(n for n in both if current[n] and not baseline[n])
    no_longer_ignored = sorted(n for n in both if baseline[n] and not current[n])
    return removed, added, newly_ignored, no_longer_ignored


def _bullets(names: list[str], limit: int = 25) -> str:
    shown = "".join(f"      {n}\n" for n in names[:limit])
    if len(names) > limit:
        shown += f"      ... and {len(names) - limit} more\n"
    return shown


def report(
    baseline: dict[str, bool],
    current: dict[str, bool],
    per_binary: dict[str, int],
    problems: list[str],
    baseline_path: Path,
) -> tuple[int, str]:
    out: list[str] = []
    out.append(f"test set diff vs {_rel(baseline_path)}")
    # Every population step prints its denominator. The one that actually bit
    # while this ticket was measured was the FIRST, which is also the one least
    # likely to be checked, because it reads as plumbing rather than logic.
    empty = sorted(b for b, n in per_binary.items() if n == 0)
    holding = len(per_binary) - len(empty)
    detail = f", {len(empty)} holding none ({', '.join(empty)})" if empty else ""
    out.append(
        f"  binaries:  {len(per_binary)} enumerated from cargo's JSON, "
        f"{holding} holding tests{detail}"
    )
    out.append(
        f"  baseline:  {len(baseline)} names "
        f"({sum(baseline.values())} ignored)"
    )
    out.append(
        f"  this tree: {len(current)} names "
        f"({sum(current.values())} ignored)"
    )
    out.append(
        "  not covered: doctests — `--list` does not enumerate them; they are "
        "outside this check"
    )

    if problems:
        out.append("")
        for p in problems:
            out.append(f"REFUSING TO COMPARE: {p}")
        out.append(
            "\nFAIL: this check cannot be trusted in this state, and a zero "
            "from it\nwould be indistinguishable from a real one."
        )
        return 1, "\n".join(out)

    removed, added, newly_ignored, no_longer_ignored = evaluate(baseline, current)

    if not (removed or added or newly_ignored or no_longer_ignored):
        out.append("")
        out.append(
            f"{VERIFIED_MARKER}: the test set matches the baseline exactly "
            f"({len(current)} names across {len(per_binary)} binaries)"
        )
        return 0, "\n".join(out)

    out.append("")
    if removed:
        out.append(f"==> {len(removed)} test(s) REMOVED since the baseline:")
        out.append(_bullets(removed).rstrip("\n"))
    if newly_ignored:
        out.append(f"==> {len(newly_ignored)} test(s) newly `#[ignore]`d:")
        out.append(_bullets(newly_ignored).rstrip("\n"))
    if added:
        out.append(f"==> {len(added)} test(s) added:")
        out.append(_bullets(added).rstrip("\n"))
    if no_longer_ignored:
        out.append(f"==> {len(no_longer_ignored)} test(s) no longer `#[ignore]`d:")
        out.append(_bullets(no_longer_ignored).rstrip("\n"))

    out.append("")
    if removed or newly_ignored:
        out.append(
            "A REMOVAL YOU DID NOT INTEND IS THE ALARM. A test can vanish from\n"
            "this list without failing anything: an insertion landing between\n"
            "`#[test]` and its function swallows the attribute, and the suite\n"
            "TOTAL still reconciles if something was added in the same change.\n"
            "Check the names above are ones you meant to remove or ignore."
        )
    else:
        out.append(
            "Only additions — nothing was removed or newly ignored."
        )
    out.append(
        f"\nIf every difference above is intended, re-record the baseline:\n"
        f"    {RECORD_RECIPE}\n"
        f"That rewrites {_rel(baseline_path)}, so an unintended removal appears\n"
        f"as a DELETED LINE in your diff, next to the change that caused it."
    )
    out.append("\nFAIL: the test set does not match the baseline.")
    return 1, "\n".join(out)


# --------------------------------------------------------------------------
# Entry points
# --------------------------------------------------------------------------


def gather(cargo_args: list[str] | None = None):
    artifacts = test_inventory.enumerate_artifacts(cargo_args or [])
    return collect(artifacts)


def check(cargo_args: list[str] | None = None, baseline_path: Path | None = None):
    baseline_path = baseline_path or DEFAULT_BASELINE
    current, problems, per_binary = gather(cargo_args)
    baseline, base_problems = read_baseline(baseline_path)
    return report(baseline, current, per_binary, problems + base_problems, baseline_path)


def run_selftest() -> int:
    """Prove this check can FAIL, one case per way it could go quietly wrong.

    A set check that has never reported a removal is in exactly the position of
    the suite total it was built to backstop: see docs/verification-hazards.md,
    "A control proves nothing by not failing". So every guard below is driven
    with an input that must make it fire, and the always-alarming failure mode
    is controlled for separately.
    """
    import tempfile

    problems: list[str] = []

    def want(cond, msg):
        if not cond:
            problems.append(msg)

    # ---- the two regexes over one format must not drift -------------------
    # `test_inventory` counts these lines and this module names them. Two
    # readers of one format drift silently, and the drift would show up as a
    # phantom removal.
    sample = [
        "config::tests::foo: test",
        "bare_integration_test: test",
        "some::bench_thing: bench",
        "",
        "3 tests, 1 benchmark",
        "a::b: test",
    ]
    mine = [m.group("name") for line in sample if (m := LISTED_NAME.match(line))]
    theirs = [line for line in sample if test_inventory.LISTED_TEST.search(line)]
    want(
        len(mine) == len(theirs) == 3,
        f"LISTED_NAME and test_inventory.LISTED_TEST disagree on the same "
        f"lines ({len(mine)} vs {len(theirs)}) — one of them is misreading "
        f"`--list`",
    )
    want("some::bench_thing" not in mine, "a benchmark was read as a test")

    # ---- THE B158 SHAPE: an even swap the total cannot see -----------------
    # One test disabled, one added. |baseline| == |current|, so every count in
    # the world reconciles. This is the whole ticket.
    base = {"b::keeps": False, "b::hover_rejects_nadeo_class_storage_key": False}
    cur = {"b::keeps": False, "b::newly_added": False}
    want(len(base) == len(cur), "the even-swap fixture is not actually even")
    removed, added, _, _ = evaluate(base, cur)
    want(
        removed == ["b::hover_rejects_nadeo_class_storage_key"],
        f"the even swap did not report the removed test: {removed}",
    )
    want(added == ["b::newly_added"], f"the even swap misreported additions: {added}")

    # ---- MECHANISM 2: `#[ignore]`, which leaves the name in `--list` -------
    # A name-set diff alone is BLIND to this, which is why both sets exist. A
    # control built only from the B158 shape above would leave this path
    # entirely unproven, and deleting the second-set comparison would keep the
    # suite green. (hazards: "It is not enough for a check to REACH a guard --
    # the input must land elsewhere without it".)
    base = {"b::t": False, "b::other": False}
    cur = {"b::t": True, "b::other": False}
    removed, added, newly_ignored, _ = evaluate(base, cur)
    want(
        not removed and not added,
        "the `#[ignore]` fixture changed the NAME set, so it does not actually "
        "exercise the second-set comparison it was written for",
    )
    want(
        newly_ignored == ["b::t"],
        f"a test disabled by `#[ignore]` was not reported: {newly_ignored}",
    )
    _, _, _, unignored = evaluate(cur, base)
    want(unignored == ["b::t"], "un-ignoring a test was not reported")

    # ---- POSITIVE CONTROL: additions alone must not read as an alarm -------
    # A check that flags everything satisfies every case above and is useless.
    base = {"b::a": False}
    cur = {"b::a": False, "b::b": False, "b::c": False}
    removed, added, newly_ignored, _ = evaluate(base, cur)
    want(
        not removed and not newly_ignored and len(added) == 2,
        f"an ordinary add-only lane did not report clean additions: "
        f"removed={removed} added={added} ignored={newly_ignored}",
    )
    status, text = report(base, cur, {"b": 3}, [], DEFAULT_BASELINE)
    want("REMOVED" not in text, "an add-only diff used the word REMOVED")
    want(
        "Only additions" in text,
        "an add-only diff did not say plainly that nothing was removed",
    )

    # ---- a clean tree is the only thing that may print the marker ----------
    status, text = report({"b::a": False}, {"b::a": False}, {"b": 1}, [], DEFAULT_BASELINE)
    want(status == 0 and VERIFIED_MARKER in text, "an identical set was not verified")
    status, text = report(base, cur, {"b": 3}, [], DEFAULT_BASELINE)
    want(status == 1, "a differing set exited 0")
    want(VERIFIED_MARKER not in text, "a differing set printed the verified marker")

    # ---- ROUTE 3: the empty/truncated baseline ----------------------------
    # The hole a pure sorted list has that clippy's dict does not. With zero
    # expected names every test reads as ADDED and REMOVALS COME OUT ZERO --
    # this ticket's own defect, arriving in the step that feels like plumbing.
    with tempfile.TemporaryDirectory(prefix="b163-") as tmp:
        tmpdir = Path(tmp)

        missing = tmpdir / "nope.txt"
        names, probs = read_baseline(missing)
        want(probs, "a MISSING baseline was accepted")

        empty = tmpdir / "empty.txt"
        empty.write_text("")
        names, probs = read_baseline(empty)
        want(probs, "an EMPTY baseline was accepted")
        # and prove the failure it would have caused is real
        removed, added, _, _ = evaluate(names, {"b::a": False, "b::b": False})
        want(
            not removed and len(added) == 2,
            "the empty-baseline fixture does not actually produce the "
            "zero-removals reconciliation it is guarding against",
        )

        short = tmpdir / "short.txt"
        short.write_text(
            "".join(
                f"{name}\n"
                for name in sorted(
                    f"b::t{i}" for i in range(MIN_PLAUSIBLE_BASELINE - 1)
                )
            )
        )
        names, probs = read_baseline(short)
        want(probs, "a TRUNCATED baseline just under the floor was accepted")
        want(
            len(names) == MIN_PLAUSIBLE_BASELINE - 1,
            "the truncation fixture did not parse the names it wrote",
        )

        ok = tmpdir / "ok.txt"
        ok.write_text(
            "".join(
                f"{name}\n"
                for name in sorted(f"b::t{i}" for i in range(MIN_PLAUSIBLE_BASELINE))
            )
        )
        names, probs = read_baseline(ok)
        want(not probs, f"a plausible baseline was refused: {probs}")

        # round trip, including the ignored marker
        original = {"b::a": False, "b::z": True, "a::m": False}
        rt = tmpdir / "rt.txt"
        rt.write_text(render_baseline(original))
        back, probs = parse_baseline(rt.read_text())
        want(back == original, f"the baseline did not round-trip: {back}")
        want(not probs, f"a rendered baseline did not parse: {probs}")
        lines = rt.read_text().splitlines()
        want(lines == sorted(lines), "the rendered baseline is not sorted")
        want(
            all(not ln[0].isdigit() for ln in lines if ln),
            "the rendered baseline has a count line in it — that is the "
            "metadata this format exists to keep out",
        )

        dup = tmpdir / "dup.txt"
        dup.write_text("b::a\nb::a\n")
        _, probs = parse_baseline(dup.read_text())
        want(probs, "a duplicated baseline entry was accepted")

        for label, malformed in (
            ("metadata header", "# total: 2\nb::a\nb::b\n"),
            ("blank line", "b::a\n\nb::b\n"),
            ("trailing whitespace", "b::a \nb::b\n"),
            ("non-codepoint order", "b::b\nb::a\n"),
        ):
            _, probs = parse_baseline(malformed)
            want(
                probs,
                f"a baseline with {label} was accepted, so the file is no "
                f"longer the pure sorted list its merge behaviour relies on",
            )

    # ---- ROUTE 1: zero binaries enumerated --------------------------------
    # A set diff over zero binaries reports zero removals and reconciles
    # perfectly. This is the step that actually bit while measuring the ticket.
    names, probs, per_binary = collect([])
    want(probs and not names, "ZERO test binaries was accepted as a population")
    status, text = report({"b::a": False}, names, per_binary, probs, DEFAULT_BASELINE)
    want(status == 1, "a zero-binary run did not fail")
    # Keyed on the REPORT HEADER, not the bare word "REMOVED": the refusal
    # prose legitimately explains that a bad population would "report every
    # test REMOVED", so a substring both branches contain proves nothing. That
    # is B162's lesson applied to this file's own assertions.
    want(
        "REFUSING TO COMPARE" in text,
        "a zero-binary run did not say it was refusing",
    )
    want(
        "REMOVED since the baseline" not in text,
        "a zero-binary run reported REMOVALS rather than refusing — it would "
        "hand a lane an alarm about its own broken build",
    )

    # ---- ROUTE 2: a binary that lists nothing, and the sets not nesting ----
    real = list_names
    try:
        def art(name):
            return {"profile": {"test": True}, "target": {"name": name},
                    "executable": f"/nonexistent/{name}"}

        # ONE binary listing nothing is legitimate and must NOT alarm: the
        # bin's own test target holds no unit tests. Refusing it made this
        # tool over-condemn on its first real run.
        globals()["list_names"] = lambda exe, ignored=False: [] if ignored else (
            [] if "empty" in exe else ["a", "b"]
        )
        names, probs, per_bin = collect([art("empty_bin"), art("full_bin")])
        want(
            not probs and len(names) == 2,
            f"a single binary legitimately holding no tests was refused: {probs}",
        )
        want(per_bin.get("empty_bin") == 0, "the empty binary lost its denominator")
        # but ALL of them listing nothing is the vacuous population, and that
        # must refuse -- otherwise the baseline reads as entirely REMOVED, or
        # reconciles perfectly against an empty one.
        globals()["list_names"] = lambda exe, ignored=False: []
        names, probs, _ = collect([art("a_bin"), art("b_bin")])
        want(
            probs and not names,
            "binaries listing ZERO tests BETWEEN THEM were accepted as a "
            "population",
        )

        globals()["list_names"] = (
            lambda exe, ignored=False: ["stray"] if ignored else ["a", "b"]
        )
        _, probs, _ = collect([art("odd_bin")])
        want(probs, "`--list --ignored` naming a test absent from `--list` was accepted")

        globals()["list_names"] = lambda exe, ignored=False: None
        _, probs, _ = collect([art("dead_bin")])
        want(probs, "a binary whose `--list` could not be read was accepted")

        # names are qualified by binary: two binaries sharing a bare name must
        # stay two entries. Measured: this tree really has one such name in
        # four binaries, so an unqualified set would hide three removals.
        globals()["list_names"] = lambda exe, ignored=False: [] if ignored else ["shared"]
        names, probs, _ = collect([art("bin_one"), art("bin_two")])
        want(
            len(names) == 2 and not probs,
            f"a bare name shared by two binaries collapsed to {len(names)} "
            f"entry/entries — removing it from one would be invisible",
        )
    finally:
        globals()["list_names"] = real

    if problems:
        print("FAIL: test_set_diff's own guards did not all fire")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(
        "PASS: test_set_diff — the even swap a total cannot see, both disable\n"
        "mechanisms (attribute swallowed and `#[ignore]`), an add-only lane and\n"
        "a lone empty binary that must NOT alarm, and the three population\n"
        "zeros (no binaries, no tests between them, an empty or truncated\n"
        "baseline) each fail."
    )
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__.split("\n")[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Doctests are NOT covered: `--list` on a test binary does not "
            "enumerate them.\n"
            f"Re-record the committed baseline with `{RECORD_RECIPE}`."
        ),
    )
    ap.add_argument("--baseline", default=None,
                    help="baseline file (default: docs/test-baseline.txt); "
                         "`@target` writes into $CARGO_TARGET_DIR for --record")
    ap.add_argument("--record", action="store_true",
                    help="write the baseline from this tree instead of comparing")
    ap.add_argument("--selftest", action="store_true",
                    help="prove these guards can fail; no cargo, no network")
    args, rest = ap.parse_known_args()

    if args.selftest:
        return run_selftest()

    if args.baseline == "@target":
        target = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
        baseline_path = target / "test-baseline.txt"
    elif args.baseline:
        baseline_path = Path(args.baseline)
    else:
        baseline_path = DEFAULT_BASELINE

    if args.record:
        # The record path has no gate in front of it, unlike the compare path
        # which runs after `test_inventory.check` inside the suite. Recording a
        # truncated population would bake a short expectation into the gate and
        # report zero removals forever, so the same proven per-binary
        # listed-vs-declared reconciliation runs here first rather than being
        # argued around.
        inv_status, inv_report, _ = test_inventory.check(rest)
        if inv_status:
            print(inv_report, file=sys.stderr)
            print(
                "\nrefusing to record: the test inventory does not reconcile "
                "against the sources,\nso the names listed here are not "
                "trustworthy enough to become the expectation.",
                file=sys.stderr,
            )
            return 1
        current, problems, per_binary = gather(rest)
        # Recording is where a vacuous population does the most damage: it
        # would write a short baseline that then reports zero removals forever.
        if problems:
            for p in problems:
                print(f"refusing to record: {p}", file=sys.stderr)
            return 1
        if len(current) < MIN_PLAUSIBLE_BASELINE:
            print(
                f"refusing to record: only {len(current)} tests were listed "
                f"across {len(per_binary)} binaries, below the "
                f"{MIN_PLAUSIBLE_BASELINE} floor. Recording this would bake a "
                f"truncated expectation into the gate.",
                file=sys.stderr,
            )
            return 1
        baseline_path.parent.mkdir(parents=True, exist_ok=True)
        baseline_path.write_text(render_baseline(current))
        print(
            f"recorded {len(current)} test names "
            f"({sum(current.values())} ignored) from {len(per_binary)} "
            f"binaries -> {_rel(baseline_path)}"
        )
        return 0

    status, text = check(rest, baseline_path)
    print(text, file=sys.stderr if status else sys.stdout)
    return status


if __name__ == "__main__":
    sys.exit(main())
