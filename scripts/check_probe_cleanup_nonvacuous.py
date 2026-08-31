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
        target = work / "scripts" / "issue_repro_game.py"
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
