#!/usr/bin/env python3
"""Prove `probe_enum_corpus.py --selftest` is non-vacuous (B068 / B123).

Same discipline as `check_probe_cleanup_nonvacuous.py`, applied to the other
probe: each mutation restores one half of a defect this script's guards exist
to prevent, in a throwaway copy of `scripts/`, and the selftest must go red for
every one.

Why this file exists at all: B118 added nine guards whose pair-tests all passed
on the first run, and mutation testing then found FOUR of them vacuous --
every one because an earlier check in the chain had already decided that case,
so the guard under test never executed. A passing pair is a claim that a guard
is correct; it is not evidence that the guard RAN. Nothing about that lesson
was specific to B118, so the B123 guards get the same treatment.

An anchor that no longer matches, or matches more than once, is a MISS rather
than a skip: a mutation that silently failed to apply would be scored as a pass
and this harness would become the vacuous thing it detects.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

# (label, file, old, new)
MUTATIONS = [
    # --- B068: the sentinel is what makes silence mean anything ----------
    ("B068: silence certified as ACCEPTED with no sentinel",
     "probe_enum_corpus.py",
     "    analysis_reached_the_end = sentinel_line in by_line",
     "    analysis_reached_the_end = True"),
    ("B068: every name reported ACCEPTED regardless",
     "probe_enum_corpus.py",
     '            out[n] = "" if analysis_reached_the_end else None',
     '            out[n] = ""'),
    ("B068 over-condemn: every name reported UNDETERMINED regardless "
     "(a guard that fires on BOTH halves is as broken as one that fires on "
     "neither)",
     "probe_enum_corpus.py",
     '            out[n] = "" if analysis_reached_the_end else None',
     '            out[n] = None'),
    ("B068: a foreign plugin's error may certify our sentinel",
     "probe_enum_corpus.py",
     '        if d["sev"] == "ERR" and d.get("file") == source_file:',
     '        if d["sev"] == "ERR":'),

    # --- B097: a failed load must stop the batch --------------------------
    ("B097/B127: a failed or unhealthy build no longer stops the batch",
     "probe_enum_corpus.py",
     '        if verdict["build_status"] in ("NOT-RUN", "BUILD-ERR"):',
     '        if False:'),
    ("B127: run_batch goes back to trusting the transport reply, which is "
     "empty for a build whose dependency could not be resolved",
     "probe_enum_corpus.py",
     '        if verdict["build_status"] in ("NOT-RUN", "BUILD-ERR"):',
     '        if loaded.get("error"):'),
    ("B127 over-condemn: REJECTED treated as fatal, which would abort every "
     "real run (this probe's sentinel is a DELIBERATE error, so REJECTED is "
     "the normal outcome)",
     "probe_enum_corpus.py",
     '        if verdict["build_status"] in ("NOT-RUN", "BUILD-ERR"):',
     '        if verdict["build_status"] != "CLEAN":'),

    # --- B123: the raw reader, and the build-health guard it enables ------
    ("B123: run_batch reverts to the severity-filtered reader, so the build "
     "bracket is invisible and the health check cannot fire",
     "probe_enum_corpus.py",
     "        window = fleet.read_log_window_raw(mark, PROBE_ID)",
     "        window = fleet.read_log_window(mark, PROBE_ID)"),
    ("B123: location-less build errors stop being collected at all",
     "fleet_dogfood.py",
     "    raw_errs = raw_build_errors(window, fid)",
     "    raw_errs = []"),
    ("B123: build_slice stops distinguishing 'never started'",
     "fleet_dogfood.py",
     "    if start is None:\n        return []",
     "    if start is None:\n        start = 0"),
    ("B123: raw_build_errors stops excluding located diagnostics, so an "
     "ordinary REJECT would abort the corpus run",
     "fleet_dogfood.py",
     "        and not _LOCATED_DIAG.search(l)",
     "        and True"),
]


def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/probe_enum_corpus.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=600,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b123-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        targets = {
            "probe_enum_corpus.py": work / "scripts" / "probe_enum_corpus.py",
            "fleet_dogfood.py": work / "scripts" / "fleet_dogfood.py",
        }
        pristines = {k: v.read_text() for k, v in targets.items()}

        if run(work) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for label, fname, old, new in MUTATIONS:
            target, pristine = targets[fname], pristines[fname]
            if old not in pristine:
                print(f"  ANCHOR MISSING: {label} [{fname}]", file=sys.stderr)
                missed += 1
                continue
            if pristine.count(old) != 1:
                print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}",
                      file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            rc = run(work)
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
