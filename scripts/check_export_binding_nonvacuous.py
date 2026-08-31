#!/usr/bin/env python3
"""B154: prove `probe-export-binding-selftest` is non-vacuous, one guard at a time.

`probe_export_binding` had a `--selftest` and no mutation harness anywhere --
found by B148's discovery check once it PARSED `scripts/` instead of trusting a
hardcoded list of three probes. Its selftest is the one this repo cites as the
shape a probe oracle should have, which makes "nothing proves it can fail" the
least comfortable gap in the batch.

THE ASSUMPTION THIS FILE STARTS FROM IS THAT THE SELFTEST IS VACUOUS UNTIL A
MUTATION RUN SAYS OTHERWISE. Two of six new mutations survived first time on
B151, on checks that read as obviously sufficient.

Each mutation neuters ONE guard in a throwaway copy and requires the selftest to
go red FOR THAT GUARD. Three kinds are represented deliberately:

* **Guard mutations** -- disable a branch in `classify`.
* **Subject mutations** -- break the thing a guard WATCHES rather than the
  guard. B148 learned this the hard way: neutering a check so it always passes
  cannot turn a selftest red, so that mutation proves nothing. The variant-table
  checks are all tested this way.
* **Production-path mutations** -- change something the selftest never calls.
  B118's rule: a selftest that INJECTS every input cannot see which path
  production takes. `probe_export_binding`'s selftest hands `classify` synthetic
  windows, so nothing it does can observe which reader `run()` actually uses, or
  whether the generated consumer really calls the import. These are expected to
  SURVIVE on the first run; that is the finding, not a failure of this harness.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
PROBE = "probe_export_binding.py"

# (label, why the guard exists, old, new, the selftest line it must kill)
MUTATIONS: list[tuple[str, str, str, str, str]] = [
    (
        "bound-and-unbound-is-AMBIGUOUS",
        "a window holding both a return and an unbound error is contended, and "
        "calling it BOUND reports another plugin's noise as our measurement",
        "    if bound and unbound:",
        "    if False:",
        "returned AND unbound in the same window",
    ),
    (
        "not-started-is-AMBIGUOUS",
        "a consumer that never ran measured nothing; reading it as UNBOUND scores "
        "a probe that never happened as evidence about the engine",
        "    if not started:",
        "    if False:",
        "an unbound error with no start token is not a verdict",
    ),
    (
        "load-error-is-AMBIGUOUS",
        "RemoteBuild refusing the load means no measurement happened at all",
        '    if load.get("error"):',
        "    if False:",
        "a refused load outranks a window that would otherwise be BOUND",
    ),
    (
        "errors-are-attributed-to-our-fixture",
        "Openplanet.log is SHARED: another session's compile error in the same "
        "window would otherwise be read as this probe's answer",
        '        if d["sev"] == "ERR" and d["file"].startswith(f"{fid}/")',
        '        if d["sev"] == "ERR"',
        "another plugin's compile error in the window",
    ),
    # ---- SUBJECT mutations: break what the guard watches, not the guard ----
    (
        "SUBJECT controls-expect-opposite-verdicts",
        "B086's defect exactly: two variants that fail identically measure "
        "nothing while looking like they measured something",
        '        "NEGATIVE CONTROL: a stub naming a function that does not exist.",\n        "UNBOUND",',
        '        "NEGATIVE CONTROL: a stub naming a function that does not exist.",\n        "BOUND",',
        "the positive and negative controls expect the SAME verdict",
    ),
    (
        "SUBJECT duplicate-variant-ids",
        "rows are keyed by id, so a duplicate silently overwrites a measurement",
        '        "control-no-body",\n        "Echo_missing",',
        '        "exact-const-ref-in",\n        "Echo_missing",',
        "duplicate variant ids",
    ),
    (
        "SUBJECT duplicate-function-names",
        "two variants would measure ONE function, and the exporter would not compile",
        '        "control-no-body",\n        "Echo_missing",',
        '        "control-no-body",\n        "Echo_exact",',
        "duplicate function names",
    ),
    # ---- PRODUCTION-PATH mutations: things the selftest never calls ----
    (
        "PRODUCTION raw-window-reader",
        "B118: the FILTERED reader drops every `[ TRAC]` line, and this probe's "
        "entire evidence is a plugin's print() plus a runtime exception. The "
        "filter cost three probes their answer before it was hoisted",
        "    return fleet.read_log_window_raw(mark)",
        '    return fleet.read_log_window(mark, "")',
        "not the RAW reader",
    ),
    (
        "PRODUCTION consumer-actually-calls-the-import",
        "B086's whole point is that binding is a RUNTIME property, so the "
        "consumer must CALL the import. A stub that merely compiles proves "
        "nothing: an unmatched stub compiles fine on both sides",
        '        f"    {call}\\n"',
        '        f\'    string res = "never called";\\n\'',
        "does not CALL the import",
    ),
    # ---- OVER-CONDEMN ----
    (
        "OVER-CONDEMN everything-is-AMBIGUOUS",
        "an oracle that refuses to answer passes any suite that only checks that "
        "non-answers are caught -- it 'catches' every real measurement too",
        "def classify(variant: Variant, window: str, fid: str, load: dict) -> Result:\n    diags = [",
        'def classify(variant: Variant, window: str, fid: str, load: dict) -> Result:\n'
        '    return Result(variant, "AMBIGUOUS", "over-condemn", [], window)\n    diags = [',
        "a call that returned",
    ),
    (
        "OVER-CONDEMN everything-is-BOUND",
        "the opposite pole: an oracle that always says BOUND would report every "
        "stub shape as binding and close the ticket with a wrong answer",
        "def classify(variant: Variant, window: str, fid: str, load: dict) -> Result:\n    diags = [",
        'def classify(variant: Variant, window: str, fid: str, load: dict) -> Result:\n'
        '    return Result(variant, "BOUND", "over-condemn", [], window)\n    diags = [',
        "an unbound call",
    ),
]


def run_selftest(work: Path) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(work / "scripts" / PROBE), "--selftest"],
        capture_output=True, text=True, cwd=work,
    )
    return proc.returncode, proc.stdout + proc.stderr


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b154-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        shutil.copytree(SRC / "docs" / "probes", work / "docs" / "probes")

        target = work / "scripts" / PROBE
        pristine = target.read_text(encoding="utf8")

        # PRE-FLIGHT (B139). If the unmutated selftest is already red, every
        # mutation below "kills" something that was already dead.
        rc, out = run_selftest(work)
        if rc != 0:
            print("PRE-FLIGHT FAILED: the unmutated selftest is already red.")
            print(out)
            return 2
        print("pre-flight: unmutated selftest is GREEN")
        print()

        killed = survived = wrong = setup = 0
        survivors: list[tuple[str, str]] = []

        for label, why, old, new, expect in MUTATIONS:
            count = pristine.count(old)
            if count != 1:
                print(f"  SETUP-FAIL  {label}: anchor appears {count}x, expected 1")
                setup += 1
                continue
            try:
                target.write_text(pristine.replace(old, new), encoding="utf8")
                rc, out = run_selftest(work)
                if rc == 0:
                    print(f"  SURVIVED    {label}")
                    survived += 1
                    survivors.append((label, why))
                elif expect not in out:
                    print(f"  WRONG-KILL  {label}: red, but {expect!r} was not the failing check")
                    wrong += 1
                else:
                    print(f"  killed      {label}")
                    killed += 1
            finally:
                target.write_text(pristine, encoding="utf8")

        print()
        print(f"{killed}/{len(MUTATIONS)} mutations killed the selftest for the intended reason")
        if survivors:
            print()
            print(f"VACUOUS GUARDS ({len(survivors)}) — the selftest cannot see these:")
            for label, why in survivors:
                print(f"  {label}")
                print(f"    guard exists for: {why}")
        if wrong:
            print(f"KILLED FOR THE WRONG REASON: {wrong}")
        if setup:
            print(f"NEVER APPLIED — anchor not unique: {setup}. These proved NOTHING.")
        return 0 if (survived == wrong == setup == 0) else 1


if __name__ == "__main__":
    sys.exit(main())
