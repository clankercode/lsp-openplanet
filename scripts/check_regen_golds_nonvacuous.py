#!/usr/bin/env python3
"""B129: prove `regen-golds --selftest` is non-vacuous, one guard at a time.

A passing selftest is not evidence that its guards ran. B118 found 4 of 9 new
guards vacuous this way AFTER all their pair-tests passed, so this harness
demands PER-GUARD KILL EVIDENCE: it neuters each guard individually in a
throwaway copy of the runner and requires the selftest to go red for that
specific guard. A mutation that leaves the selftest green is a guard that was
never doing anything.

TWO OF THESE MUTATIONS RUN THE OTHER WAY (over-condemn). A runner that reports
every artifact ERROR, or every row count as drift, passes any suite that only
checks that real problems get caught — it "catches" everything, including all
the healthy artifacts. `regen-golds` is especially exposed to this because its
normal audit output is entirely UNCHECKED: a report with no failures in it and
a report in which nothing was measured look identical unless something asserts
the difference. So a mutant that condemns the whole repo must also FAIL.

Every mutant is applied to a copy under `scripts/`, never to the real file, and
the copy is removed on every exit path (B077: a stranded fixture is worse than
no fixture).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
TARGET = HERE / "regen_golds.py"
MUTANT = HERE / "_regen_golds_mutant.py"

# (name, what the guard is for, old, new, the selftest check it must kill)
MUTATIONS: list[tuple[str, str, str, str, str]] = [
    (
        "stray-corpus-detection",
        "a new corpus must not be able to dodge the manifest by not being added to it",
        "    return found\n",
        "    return []\n",
        "stray corpus is detected",
    ),
    (
        "not-run-is-never-clean",
        "not having regenerated an artifact is not the game agreeing with it (B118/B127)",
        'return Result(row, UNCHECKED, "not-run", "audit only; pass --regen to measure")',
        'return Result(row, CLEAN, "not-run", "audit only; pass --regen to measure")',
        "audit never CLEAN",
    ),
    (
        "unknown-version-refusal",
        "an artifact with no recorded version cannot be diffed against a live game",
        '    if row.engine_version == "UNKNOWN":',
        "    if False:",
        "UNKNOWN version refuses",
    ),
    (
        "version-moved-refusal",
        "three engine generations are cited in this repo; a version change is not drift",
        "    if live != row.engine_version:",
        "    if False:",
        "version mismatch refuses",
    ),
    (
        "no-live-version-refusal",
        "refusing to diff against an ASSUMED version is the point of the column",
        "    if live is None:",
        "    if False:",
        "absent live version refuses",
    ),
    (
        "orphan-has-its-own-reason",
        "b067/b071 have no generator at all; folding that into not-run hides the finding",
        'row, UNCHECKED, "no-generator",',
        'row, UNCHECKED, "not-run",',
        "orphan reason",
    ),
    (
        "primary-has-its-own-reason",
        "PRIMARY in-source ground truth needs a human, which not-run does not convey",
        'row, UNCHECKED, "manual-re-verify",',
        'row, UNCHECKED, "not-run",',
        "primary reason",
    ),
    (
        "empty-manifest-raises",
        "a manifest that parses to nothing would make every artifact silently pass",
        "    if not lines:",
        "    if False:",
        "empty manifest raises",
    ),
    (
        "manifest-column-check",
        "the runner reads the manifest positionally, so a column change must be loud",
        "    if header != COLUMNS:",
        "    if False:",
        "wrong columns raise",
    ),
    (
        "row-count-staleness",
        "the one staleness check that needs no game; silently disabling it is invisible",
        '    """Staleness check that needs no game: does `rows` match the file?"""',
        '    """Staleness check that needs no game: does `rows` match the file?"""\n    return None',
        "row-count mismatch is detected",
    ),
    (
        "version-reader-picks-latest",
        "the log accumulates sessions; reading the first reports a stale generation as live",
        "    return found[-1][0] if found else None",
        "    return found[0][0] if found else None",
        "last version line wins",
    ),
    (
        "version-reader-invents-nothing",
        "a version guessed from an unreadable log defeats the entire refusal mechanism",
        "    if not log.exists():\n        return None\n    try:",
        '    if not log.exists():\n        return "1.29.14"\n    try:',
        "a missing log returns None",
    ),
    (
        "drift-is-detected",
        "the whole point of --regen: a changed artifact must not come back CLEAN",
        "    if after == before:",
        "    if True:",
        "a generator changing the file is DRIFT",
    ),
    (
        "failing-generator-is-error",
        "a probe that crashed has measured nothing; calling that CLEAN is B127's defect",
        "    if proc.returncode != 0:",
        "    if False:",
        "a failing generator is ERROR",
    ),
    (
        "silent-no-output-is-error",
        "a generator that exits 0 and writes nothing would otherwise read as agreement",
        '        return Result(row, ERROR, "no-output", "generator succeeded but wrote no file")',
        '        return Result(row, CLEAN, "no-output", "generator succeeded but wrote no file")',
        "a generator writing no file is ERROR",
    ),
    (
        "drift-carries-dependents",
        "the fan-out edge: B094's one measurement is restated in four files, and a "
        "regen updates none of them",
        '        dependents_unchecked=[d for d in row.dependents.split(";") if d != "-"],',
        "        dependents_unchecked=[],",
        "DRIFT carries its dependents",
    ),
    (
        "volatility-vocabulary-is-closed",
        "an unvalidated label is a free-text field, and B151's whole mechanism is the label",
        "        if row.volatility not in KNOWN_VOLATILITY:",
        "        if False:",
        "bad volatility raises",
    ),
    (
        "retention-detects-an-archived-artifact",
        "if nothing is ever found archived, the policy silently demands archives that exist",
        "            if hit:\n                out.append(d.name)",
        "            if False:\n                out.append(d.name)",
        "an archived artifact is detected",
    ),
    (
        "retention-detects-an-UNARCHIVED-artifact",
        "B151's finding was three SURFACE corpora nothing archived; a check that "
        "never says so would have reported the policy as implemented",
        "    return sorted(set(out))",
        '    return sorted(set(out)) or ["1.29.5"]',
        "an unarchived artifact is detected",
    ),
    (
        "semantics-artifacts-are-exempt",
        "if SEMANTICS also demanded archives, the label would decide nothing and "
        "every language corpus would grow a version store nobody diffs",
        "        if r.volatility != SURFACE or r.kind == \"VERSIONS\":",
        "        if r.kind == \"VERSIONS\":",
        "a SEMANTICS artifact needs no archive",
    ),
    (
        "both-version-stores-are-read",
        "one store holds type dumps and the other holds corpora; reading only one "
        "made the report claim b052 was retained because typedb was",
        'VERSION_STORES = ("tests/fixtures/typedb-versions", "docs/probes/versions")',
        'VERSION_STORES = ("tests/fixtures/typedb-versions",)',
        "both stores are read",
    ),
    # ---- OVER-CONDEMN ----
    (
        "OVER-CONDEMN every-artifact-errors",
        "a runner that condemns every artifact passes a suite that only checks for catches",
        '    CLEAN: not having looked is not agreement.\n    \"\"\"',
        '    CLEAN: not having looked is not agreement.\n    \"\"\"\n    return Result(row, ERROR, "condemned", "over-condemn mutant")',
        "healthy corpus is not condemned",
    ),
    (
        "OVER-CONDEMN nothing-is-ever-archived",
        "a retention check that condemns every SURFACE artifact is as useless as one "
        "that clears them all, and looks more diligent",
        "def archived_versions_of(row: Row) -> list[str]:",
        "def archived_versions_of(row: Row) -> list[str]:\n    return []",
        "an archived artifact is detected",
    ),
    (
        "OVER-CONDEMN every-row-count-drifts",
        "reporting drift on every artifact is as useless as reporting none, and looks busier",
        "    if claimed != actual:",
        "    if True:",
        "row count",
    ),
]


def run_selftest(script: Path) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(script), "--selftest"],
        capture_output=True, text=True, cwd=HERE.parent,
    )
    return proc.returncode, proc.stdout + proc.stderr


def main() -> int:
    source = TARGET.read_text(encoding="utf8")

    # PRE-FLIGHT (B139): the unmutated runner must be GREEN, or every mutation
    # below "kills" a selftest that was already red and this harness reports a
    # clean sweep having proved nothing.
    rc, out = run_selftest(TARGET)
    if rc != 0:
        print("PRE-FLIGHT FAILED: the unmutated selftest is already red.")
        print("Every mutation would appear to kill it, proving nothing.")
        print(out)
        return 2
    print(f"pre-flight: unmutated selftest is GREEN ({len(MUTATIONS)} mutations to apply)")
    print()

    survived: list[str] = []
    wrong_check: list[str] = []
    setup_fail: list[str] = []
    passed = 0

    for name, why, old, new, expect in MUTATIONS:
        count = source.count(old)
        if count != 1:
            print(f"  SETUP-FAIL {name}: anchor appears {count} times, expected exactly 1")
            setup_fail.append(name)
            continue
        try:
            MUTANT.write_text(source.replace(old, new), encoding="utf8")
            rc, out = run_selftest(MUTANT)
            if rc == 0:
                print(f"  SURVIVED   {name}")
                print(f"             guard exists for: {why}")
                survived.append(name)
            elif expect not in out:
                # It went red, but not for the reason claimed. A mutation that
                # kills the selftest via an unrelated check is not evidence
                # that THIS guard runs.
                print(f"  WRONG-KILL {name}: red, but {expect!r} was not the failing check")
                wrong_check.append(name)
            else:
                print(f"  killed     {name}  -> {expect!r}")
                passed += 1
        finally:
            MUTANT.unlink(missing_ok=True)

    # B077 belt-and-braces: no mutant may outlive this process.
    assert not MUTANT.exists(), "mutant copy survived the run"

    print()
    print(f"{passed}/{len(MUTATIONS)} mutations killed the selftest for the intended reason")
    if survived:
        print(f"VACUOUS GUARDS ({len(survived)}): {survived}")
    if wrong_check:
        print(f"KILLED FOR THE WRONG REASON ({len(wrong_check)}): {wrong_check}")
    if setup_fail:
        print(f"NEVER APPLIED — anchor not unique ({len(setup_fail)}): {setup_fail}")
        print("  These proved NOTHING. An anchor that matches more than once mutates")
        print("  several places at once, so the selftest goes red for a reason that has")
        print("  nothing to do with the guard under test.")
    if survived or wrong_check or setup_fail:
        return 1
    if passed != len(MUTATIONS):
        print("FAILED: mutation count does not match; some mutation did not run at all")
        return 1
    print("regen-golds selftest is NON-VACUOUS: every guard has kill evidence.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
