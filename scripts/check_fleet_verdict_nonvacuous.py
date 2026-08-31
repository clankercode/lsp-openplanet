#!/usr/bin/env python3
"""Prove `fleet_dogfood.py --selftest` is non-vacuous (B118 / B123 / B127).

`build_verdict` decides whether a build succeeded and `verdict` decides whether
a plugin shows PARITY. Both are guards against a MISSING signal, which red/green
cannot validate on its own — before the fix the silence they now catch was the
silence they reported as success.

Each mutation restores one half of a real defect. Two of them are OVER-CONDEMN
mutations, which matter as much as the rest: a guard that reports every build
broken passes a suite that only checks that broken builds get caught, and it
would make the dogfood tool useless while looking strictly safer.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

MUTATIONS = [
    # --- B127: the transport reply is not a report on the build -----------
    ("B127: a clean transport reply certifies the build again",
     "    elif raw_errs or load_error or not build_loaded:",
     "    elif load_error:"),
    ("B127: verdict() counts diagnostics before checking the build happened, "
     "so an empty list reads as PARITY-CLEAN",
     '    if status in ("NOT-RUN", "BUILD-ERR"):\n'
     '        return f"lsp={lspn} game=NO-BUILD({status})"',
     '    if False:\n'
     '        return f"lsp={lspn} game=NO-BUILD({status})"'),
    ("B127 over-condemn: every build reported BUILD-ERR",
     "    elif raw_errs or load_error or not build_loaded:\n",
     "    elif True:\n"),
    ("B127 over-condemn: verdict() calls every build a non-build, which would "
     "silence the dogfood tool entirely while looking safe",
     '    if status in ("NOT-RUN", "BUILD-ERR"):',
     '    if status != "ZZZ":'),
    ("B127: a pre-B127 record passes as verified",
     '    unverified = "" if status else " build=?"',
     '    unverified = ""'),

    # --- B118: the three false-green channels, now in build_verdict --------
    ("B118 ch1: build_ran no longer gates the verdict",
     '    if not build_ran:\n        status = "NOT-RUN"',
     '    if False:\n        status = "NOT-RUN"'),
    ("B118 ch2: location-less errors stop reaching a bucket",
     "    raw_errs = raw_build_errors(window, fid)",
     "    raw_errs = []"),
    ("B118 ch2: BUILD-ERR collapsed into REJECTED",
     '        status = "BUILD-ERR"\n    else:\n        status = "CLEAN"',
     '        status = "REJECTED"\n    else:\n        status = "CLEAN"'),
    ("B118 ch3: `Script compilation failed!` no longer counted",
     "    build_failed = any(BUILD_FAILED in l for l in slice_)",
     "    build_failed = False"),
    ("B118 ch3: build_slice stops distinguishing 'never started'",
     "    if start is None:\n        return []",
     "    if start is None:\n        start = 0"),
    # The control that proves the harness is wired at all.
    ("control: own/foreign error attribution stops discriminating",
     '    game_errors = [d for d in errs if d["file"].startswith(mine)]',
     "    game_errors = [d for d in errs]"),
]


def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/fleet_dogfood.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=300,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b127-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        target = work / "scripts" / "fleet_dogfood.py"
        pristine = target.read_text()

        if run(work) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for label, old, new in MUTATIONS:
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
