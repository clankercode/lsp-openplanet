#!/usr/bin/env python3
"""Prove the three probe `--selftest`s are non-vacuous (B141).

`probe_const_receiver`, `probe_member_visibility` and
`probe_engine_const_receiver` gained a `--selftest` so a lane can check their
variant tables without the game. A selftest nobody has seen FAIL is a claim,
so each mutation here reintroduces a defect the table checks exist to catch and
requires that probe's selftest to go red.

The shared logic (`fleet.probe_variant_problems`) is unit-tested and mutated in
`check_fleet_verdict_nonvacuous.py`. What is proven HERE is the integration:
that each probe actually applies it to its REAL table. Those are different
claims, and B118 is the reason for separating them — a suite that only
exercises paths it hands in cannot see the path production takes.

Dropping every control is expressed by wrapping `variants()` rather than
editing rows, because two of the three files have no unique control row to
anchor on and a mutation that silently fails to apply would be scored as a
pass.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

PROBES = ("probe_const_receiver", "probe_member_visibility",
          "probe_engine_const_receiver")

# Drop every control row: the table can then no longer require two rows to
# disagree, which is the B086 "oracle cannot discriminate" defect.
DROP_CONTROLS = (
    "def variants() -> list[V]:",
    "def variants() -> list[V]:\n"
    "    return [_v for _v in _variants_unmutated() if _v.expect == 'UNKNOWN']\n"
    "\n"
    "def _variants_unmutated() -> list[V]:",
)

# (label, file, old, new, which selftest must go red)
MUTATIONS = [
    *[(f"B141/B086 [{p}]: every control row dropped, so no two rows are "
       f"required to disagree", f"{p}.py", *DROP_CONTROLS, p) for p in PROBES],

    # Blanked via the same wrapper trick: `stands_for=` appears 13 times in
    # that file, and an ambiguous anchor is reported rather than applied.
    ("B141/B096 [probe_member_visibility]: rows stop saying what shipped code "
     "they stand in for, so a fixture's claim about real plugins is unchecked",
     "probe_member_visibility.py",
     "def variants() -> list[V]:",
     "def variants() -> list[V]:\n"
     "    _vs = _variants_unmutated()\n"
     "    for _v in _vs:\n"
     "        _v.stands_for = ''\n"
     "    return _vs\n"
     "\n"
     "def _variants_unmutated() -> list[V]:",
     "probe_member_visibility"),

    ("B141/B139 [probe_member_visibility]: `classify` becomes a local copy "
     "again, outside the mutation coverage it appears to have",
     "probe_member_visibility.py",
     "from probe_const_receiver import classify  # noqa: E402",
     "from probe_const_receiver import classify as _c  # noqa: E402\n"
     "def classify(rec):\n"
     "    return _c(rec)",
     "probe_member_visibility"),

    # OVER-CONDEMN: a checker that condemns every table passes any suite that
    # only asks whether bad tables get caught, and would make all three
    # selftests permanently red.
    ("B141 OVER-CONDEMN: probe_variant_problems rejects every table",
     "fleet_dogfood.py",
     "    problems: list[str] = []\n    if not variants:",
     '    problems: list[str] = ["always"]\n    if not variants:',
     "probe_const_receiver"),
]


def run(root: Path, probe: str) -> int:
    return subprocess.run(
        [sys.executable, f"scripts/{probe}.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=300,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b141-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")

        for probe in PROBES:
            if run(work, probe) != 0:
                print(f"BASELINE FAILED for {probe} — mutations prove nothing",
                      file=sys.stderr)
                return 2
        print(f"baseline: PASS ({len(PROBES)} probe selftests)")

        caught = missed = 0
        for label, fname, old, new, probe in MUTATIONS:
            target = work / "scripts" / fname
            pristine = target.read_text()
            if old not in pristine:
                print(f"  ANCHOR MISSING: {label}", file=sys.stderr)
                missed += 1
                continue
            if pristine.count(old) != 1:
                print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}",
                      file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            rc = run(work, probe)
            if rc != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}  <-- selftest is vacuous here",
                      file=sys.stderr)
                missed += 1
            target.write_text(pristine)

        print(f"\n{caught}/{len(MUTATIONS)} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
