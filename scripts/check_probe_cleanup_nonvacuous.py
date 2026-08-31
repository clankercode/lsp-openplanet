#!/usr/bin/env python3
"""Prove `issue_repro_game.py --selftest` is non-vacuous (B077).

A test suite that still passes once the protection is removed tests nothing.
Each mutation below deletes or neuters exactly ONE guard in
`scripts/issue_repro_game.py`, in a throwaway copy of `scripts/`; the selftest
must go red for every one of them. `just probe-cleanup-mutants`.

Mutations are applied to a COPY, never to the checkout, so an interrupted run
cannot leave a neutered guard behind -- the same property the script under test
is about.

An anchor that no longer matches, or that matches more than once, is reported
as a MISS rather than skipped: a mutation that silently failed to apply would
otherwise be scored as a pass and this harness would itself become the vacuous
thing it exists to detect.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

# (label, old, new)
MUTATIONS = [
    ("__enter__ cleanup on partial stage",
     "            if self.lease is not None and remove_tree(self.dest, lease=self.lease):\n"
     "                _STAGED.pop(self.dest, None)\n            raise",
     "            raise"),
    ("signal handlers",
     "    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):",
     "    for sig in ():"),
    ("atexit hook",
     "    atexit.register(_cleanup_staged)",
     "    pass  # atexit removed"),
    ("second probe dir prefix",
     'PROBE_DIR_PREFIXES = (PROBE_PREFIX, "lsp-enum-corpus-probe")',
     'PROBE_DIR_PREFIXES = (PROBE_PREFIX,)'),
    ("early registration in __enter__",
     "            _STAGED[self.dest] = self.lease",
     "            pass  # registration removed"),
    ("contamination guard in probe()",
     "    strays = probe_dirs_present(root, allow={fid})",
     "    strays = []"),
    ("empty-only sweeping (any dir looks empty)",
     "        return d.is_dir() and not d.is_symlink() and not any(d.iterdir())",
     "        return True"),
    ("live-pid protection",
     "        if pid is not None and pid != os.getpid() and pid_alive(pid):",
     "        if False:"),
    ("removal verification in remove_tree",
     "    return not _path_present(d)",
     "    return True"),
    ("non-empty-dir protection in sweep",
     "        if not is_empty_dir(d):",
     "        if False:"),
    # NOTE: this mutation makes the guard PERMIT rather than deleting it.
    # Deleting the call would let the un-injected probe reach the REAL game
    # during mutation testing -- the exact defect under test, committed by the
    # harness that checks for it. `fleet.rb_alive` stays replaced; it just
    # stops refusing. Same red, zero traffic.
    ("selftest game-access revocation (guard permits instead of refusing)",
     "    def refuse(*_a, **_k):\n        raise AssertionError(",
     "    def refuse(*_a, **_k):\n        return False\n        raise AssertionError("),
    ("emergency-teardown failure report",
     '    if stuck:\n        print(',
     '    if False:\n        print('),

    # --- B118: the three false-green channels ---------------------------
    # Each of these RESTORES one half of the original defect. The selftest
    # must go red for every one, or the pair-cases added for B118 are
    # decoration. They are listed as 4-tuples because two of them live in
    # fleet_dogfood.py.
    ("B118 ch1: probe defaults back to the TRAC-filtering window",
     "issue_repro_game.py",
     "    read_log_window = read_log_window_fn or fleet.read_log_window_raw",
     "    read_log_window = read_log_window_fn or fleet.read_log_window"),
    ("B118 ch1: build_ran no longer gates the verdict",
     "issue_repro_game.py",
     '                if not rec["build_ran"]:\n'
     '                    rec["build_status"] = "NOT-RUN"\n'
     '                elif rec["game_errors"]',
     '                if False:\n'
     '                    rec["build_status"] = "NOT-RUN"\n'
     '                elif rec["game_errors"]'),
    ("B118 ch1: build_slice returns lines even when the build never started",
     "fleet_dogfood.py",
     "    if start is None:\n        return []",
     "    if start is None:\n        start = 0"),
    ("B118 ch2: raw_build_errors stops excluding LOCATED diagnostics "
     "(collapses the two error kinds)",
     "fleet_dogfood.py",
     "        and not _LOCATED_DIAG.search(l)",
     "        and True"),
    ("B118 ch2: location-less errors stop reaching a bucket at all",
     "issue_repro_game.py",
     '                rec["raw_build_errors"] = fleet.raw_build_errors(window, fid)',
     '                rec["raw_build_errors"] = []'),
    ("B118 ch2: BUILD-ERR collapsed into REJECTED",
     "issue_repro_game.py",
     '                elif rec["raw_build_errors"] or load.get("error"):\n'
     '                    rec["build_status"] = "BUILD-ERR"',
     '                elif rec["raw_build_errors"] or load.get("error"):\n'
     '                    rec["build_status"] = "REJECTED"'),
    ("B118 ch3: `Script compilation failed!` no longer counted, so a failed "
     "compile that still logs `Loaded plugin` reads as CLEAN",
     "issue_repro_game.py",
     '                rec["build_failed"] = any(fleet.BUILD_FAILED in l for l in slice_)',
     '                rec["build_failed"] = False'),
    ("B118 ch3: `Loaded plugin` treated as SUFFICIENT for a clean build",
     "issue_repro_game.py",
     '                elif not rec["build_loaded"]:\n'
     '                    rec["build_status"] = "BUILD-ERR"\n'
     '                else:\n'
     '                    rec["build_status"] = "CLEAN"',
     '                else:\n'
     '                    rec["build_status"] = "CLEAN"'),
    # The CONTROL that proves the harness is wired at all -- this attribution
    # predates B118 and already worked; if breaking it does NOT go red, the
    # three cases above are measuring nothing either.
    ("B118 control: own/foreign error attribution stops discriminating",
     "issue_repro_game.py",
     '                    d for d in errs if d["file"].startswith(f"{fid}/")',
     '                    d for d in errs'),
]

def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/issue_repro_game.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=300,
    ).returncode

def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b077-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        targets = {
            "issue_repro_game.py": work / "scripts" / "issue_repro_game.py",
            "fleet_dogfood.py": work / "scripts" / "fleet_dogfood.py",
        }
        pristines = {k: v.read_text() for k, v in targets.items()}

        if run(work) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for mut in MUTATIONS:
            # B118 mutations live in fleet_dogfood.py, so each entry names its
            # file. A 3-tuple keeps meaning "issue_repro_game.py".
            label, old, new = mut[0], mut[-2], mut[-1]
            fname = mut[1] if len(mut) == 4 else "issue_repro_game.py"
            target, pristine = targets[fname], pristines[fname]
            if old not in pristine:
                print(f"  ANCHOR MISSING: {label} [{fname}]", file=sys.stderr)
                missed += 1
                continue
            # Anchor must be unique, or the mutation is not the one described.
            if pristine.count(old) != 1:
                print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}", file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            rc = run(work)
            if rc != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}  <-- selftest is vacuous here", file=sys.stderr)
                missed += 1
            target.write_text(pristine)

        print(f"\n{caught}/{len(MUTATIONS)} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1

if __name__ == "__main__":
    sys.exit(main())
