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
          "probe_engine_const_receiver", "probe_class_scopes",
          "probe_enum_conversion")


# A probe mutation-checked by a DIFFERENT harness. Named, not assumed: the
# point of the discovery check below is that coverage is asserted somewhere,
# and an unexplained exemption is indistinguishable from an oversight.
COVERED_ELSEWHERE = {
    "probe_enum_corpus": "just enum-probe-mutants (check_enum_probe_nonvacuous.py)",
    "probe_export_binding": "just probe-export-binding-mutants "
                            "(check_export_binding_nonvacuous.py)",
}

# Probes with a `--selftest` and NO mutation coverage ANYWHERE. Each is a real
# gap. They are listed so the gap is REPORTED on every run rather than silently
# exempted — the same treatment B129 gives a corpus with no generator, because
# a known hole that nothing prints is a hole nobody closes.
# Probes with a `--selftest` and NO mutation coverage ANYWHERE. Empty as of
# B154, which closed the last one. Kept rather than deleted: this is the list a
# new probe lands on, and the reporting below is what makes the gap visible
# instead of silently exempt.
UNCOVERED: dict[str, str] = {}


def discovered_probes() -> set[str]:
    """Every `probe_*.py` in scripts/ that offers a `--selftest`.

    PROBES above is a hand-written list, and a hand-written list does not fail
    when it falls behind — it just covers less while still reporting a clean
    sweep. That is how B131's struct audit came to cover 7 of 11 structs while
    passing, and how b067 and b071 sat unregistered in docs/probes/ (B148).
    So the list is CHECKED against the directory rather than trusted.
    """
    found = set()
    for f in sorted((SRC / "scripts").glob("probe_*.py")):
        if "--selftest" in f.read_text(encoding="utf8", errors="replace"):
            found.add(f.stem)
    return found

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
    # B148: a generator that emits an EMPTY corpus measures nothing while
    # exiting 0. The variant table is where that starts.
    ("B148 [probe_class_scopes]: the variant table is emptied, so the probe "
     "would write a corpus of zero rows and report success",
     "probe_class_scopes.py",
     "def variants() -> list[V]:\n    v: list[V] = []",
     "def variants() -> list[V]:\n    return []\n\n"
     "def _unused_variants() -> list[V]:\n    v: list[V] = []",
     "probe_class_scopes"),

    # B148: a generator that marks EVERY row the same verdict looks confident
    # and has measured nothing. For a message-text probe the natural way to do
    # that is to make the discriminator default to a side instead of reporting
    # UNRECOGNISED — which is exactly the failure that would turn an engine
    # rewording into 63 flipped rows.
    ("B148 [probe_class_scopes]: the discriminator defaults to NOT_SCOPE "
     "instead of reporting UNRECOGNISED, so a reworded engine message would "
     "read as every row changing behaviour",
     "probe_class_scopes.py",
     "    return UNRECOGNISED, msg",
     "    return NOT_SCOPE, msg",
     "probe_class_scopes"),

    ("B148 [probe_enum_conversion]: a REJECTED build with no attributed "
     "message is recorded as a measurement, though the corpus's value IS the "
     "message",
     "probe_enum_conversion.py",
     '            return AMBIGUOUS, "REJECTED with no attributed message; cannot say why"',
     '            return REJECT, ""',
     "probe_enum_conversion"),

    # NOTE ON POLARITY. The first version of this mutation neutered the GUARD
    # (`True or "3" in committed`) so it always passed, and the harness
    # correctly reported it MISSED: a check that cannot fail cannot turn a
    # selftest red. To prove a guard RUNS you must break the thing it WATCHES.
    # Both mutations below drop something a regeneration can never recover,
    # which is exactly the disaster the guard exists to prevent.
    ("B148 [probe_enum_conversion]: the retained INVALID row 3 is dropped on "
     "load, so a regeneration would silently delete the record that this "
     "corpus was once wrong",
     "probe_enum_conversion.py",
     '        out[r["row"]] = r\n    return out',
     '        out[r["row"]] = r\n    out.pop("3", None)\n    return out',
     "probe_enum_conversion"),

    ("B148 [probe_enum_conversion]: the frozen lsp_today column is blanked on "
     "load, so a regeneration would erase this repo's own pre-fix history — "
     "the evidence B071 was opened on, and unrecoverable from any game",
     "probe_enum_conversion.py",
     '        out[r["row"]] = r\n    return out',
     '        r["lsp_today"] = ""\n        out[r["row"]] = r\n    return out',
     "probe_enum_conversion"),

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
        # A probe's selftest may read committed corpora, not just its own
        # source: probe_enum_conversion checks that b071's frozen `lsp_today`
        # column and its retained INVALID row are still present, because it
        # regenerates only the engine columns. Copying scripts/ alone made its
        # baseline fail in here for a missing-file reason that had nothing to
        # do with any mutation.
        shutil.copytree(SRC / "docs" / "probes", work / "docs" / "probes")

        # REGISTRATION ENFORCEMENT, before any baseline: a probe with a
        # selftest that nobody listed here is a probe whose guards are never
        # mutation-checked, and this harness would report a clean sweep
        # without ever having looked at it.
        missing = discovered_probes() - set(PROBES) - set(COVERED_ELSEWHERE) - set(UNCOVERED)
        if missing:
            print(
                f"UNREGISTERED PROBE(S) {sorted(missing)}: they offer a --selftest "
                f"but are absent from PROBES, so their guards are never "
                f"mutation-checked. Add them.",
                file=sys.stderr,
            )
            return 2
        stale = (set(PROBES) | set(COVERED_ELSEWHERE) | set(UNCOVERED)) - discovered_probes()
        if stale:
            print(f"PROBES names {sorted(stale)}, which no longer exist or lost "
                  f"their --selftest", file=sys.stderr)
            return 2

        if UNCOVERED:
            print("PROBES WITH A SELFTEST AND NO MUTATION COVERAGE ANYWHERE:")
            for name, why in sorted(UNCOVERED.items()):
                print(f"  {name}: {why}")
            print("  Reported every run. A selftest nothing can prove is not evidence.")
            print()

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
