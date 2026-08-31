#!/usr/bin/env python3
"""Prove `remote-build.py selftest` is non-vacuous (B106 / B132).

The B132 guard refuses to gate a tree with an unresolved merge, rebase or
cherry-pick, because the gate rsyncs FILES and cannot see the git index — so
conflict markers reach the compiler as syntax and come back as errors in your
code.

Half these mutations are OVER-CONDEMN shapes, and they matter at least as much
as the rest. Gating uncommitted work is the normal lane workflow, so a guard
that refuses a dirty tree — or that attributes the main checkout's merge to
every worktree — would break all five lanes at once. It would also pass any
suite that only checks that bad trees get refused. B118 found four of nine
guards vacuous exactly this way.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

MUTATIONS = [
    # --- the guard must actually be WIRED into the entry point -------------
    ("B132: the guard is removed from main(), so a conflicted tree reaches "
     "the remote (the ticket's own mutation)",
     "        refusal = unresolved_verdict(\n"
     "            operation_in_progress(git_dir_for(root)), unmerged_paths(root)\n"
     "        )\n"
     "        if refusal:\n"
     "            die(refusal)",
     "        pass  # guard removed"),

    # --- the decision itself -----------------------------------------------
    ("B132: unresolved_verdict never refuses",
     "    if not operation:\n        return None",
     "    if True:\n        return None"),
    ("B132 OVER-CONDEMN: unresolved_verdict refuses everything, including a "
     "clean tree and the normal dirty-lane workflow",
     "    if not operation:\n        return None",
     "    if False:\n        return None"),
    ("B132: the refusal stops naming the unmerged files",
     "    if unmerged:\n        shown = unmerged[:10]",
     "    if False:\n        shown = unmerged[:10]"),
    ("B132: unmerged_paths returns nothing, so the refusal cannot name files",
     '             "diff", "--name-only", "--diff-filter=U"],',
     '             "diff", "--name-only", "--diff-filter=ZZZ"],'),

    # --- which operations count --------------------------------------------
    ("B132: only merge is detected, so a rebase or cherry-pick is gated",
     '\n    ("rebase-merge", "rebase"),',
     '\n    ("zzz-not-a-sentinel", "rebase"),'),
    ("B132: cherry-pick is dropped from the sentinel list",
     '\n    ("CHERRY_PICK_HEAD", "cherry-pick"),',
     '\n    ("ZZZ_NOT_CHERRY_PICK", "cherry-pick"),'),
    ("B132 OVER-CONDEMN: bisect treated as an unresolved operation, which "
     "refuses a workflow that cannot produce conflict markers",
     '\n    ("REVERT_HEAD", "revert"),\n)',
     '\n    ("REVERT_HEAD", "revert"),\n    ("BISECT_LOG", "bisect"),\n)'),
    # B169: the defect that shipped. `REBASE_HEAD` is written during a rebase
    # and NOT removed when it succeeds, so as a sentinel it refuses the clean
    # post-rebase tree that every lane has -- and rebase-then-re-gate is the
    # mandated workflow, so this over-condemns exactly the lane doing the right
    # thing. It is the same shape as the bisect mutation above, and it is here
    # rather than only in remote-build's selftest because this file is where a
    # future "the sentinel list looks incomplete" edit gets its answer.
    ("B169 OVER-CONDEMN: REBASE_HEAD treated as an unresolved-rebase sentinel, "
     "which refuses a tree whose rebase already SUCCEEDED",
     '\n    ("REVERT_HEAD", "revert"),\n)',
     '\n    ("REVERT_HEAD", "revert"),\n    ("REBASE_HEAD", "rebase"),\n)'),
    ("B132 OVER-CONDEMN: operation_in_progress always reports a merge",
     "    if gitdir is None:\n        return None",
     '    if gitdir is None:\n        return "merge"\n    return "merge"'),

    # --- B140: --sync-back resolves under the target dir -------------------
    ("B140: a source path is no longer refused, so rsync answers with "
     "`Broken pipe (32)` instead of naming the mistake",
     "    head = PurePosix(subpath).parts[0]\n    if head in SOURCE_TREE_DIRS:",
     "    head = PurePosix(subpath).parts[0]\n    if False:"),
    ("B140: an absolute --sync-back is accepted, which relocates the "
     "destination out of target/ and can write over the source tree",
     "    if PurePosix(subpath).is_absolute():",
     "    if False:"),
    ("B140: `..` traversal is accepted",
     '    if ".." in PurePosix(subpath).parts:',
     "    if False:"),
    ("B140 OVER-CONDEMN: every --sync-back path refused, which makes the flag "
     "unusable for the artefact paths it exists to fetch",
     "    if subpath is None:\n        return None",
     '    if subpath is None:\n        return None\n    return "refused"'),
    ("B140: the not-found message stops naming the path rsync could not fetch",
     '        f"--sync-back {subpath!r} does not exist on {host}:\\n"\n'
     '        f"    {remote_path}\\n\\n"',
     '        f"sync-back failed\\n"'),

    # --- B139: ssh's own status must not read as a verdict -----------------
    ("B139: exit 255 loses its note, so ssh's own failure status reads as a "
     "result from the remote command",
     "    if rc == 255:",
     "    if False:"),
    ("B139 OVER-CONDEMN: every exit status gets the ssh note, which makes a "
     "note nobody reads",
     "    if rc == 255:",
     "    if True:"),

    # --- worktree scope: the property that keeps the guard from being an
    # outage. Resolving the gitdir by hand attributes the main checkout's
    # merge to every lane.
    ("B132 OVER-CONDEMN: gitdir resolved as root/.git, so the coordinator's "
     "merge stops every lane worktree at once",
     '            ["git", "-C", str(root), "rev-parse", "--absolute-git-dir"],',
     '            ["git", "-C", str(root), "rev-parse", "--git-common-dir"],'),
]


def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/remote-build.py", "selftest"],
        cwd=root, capture_output=True, text=True, timeout=600,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b132-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        target = work / "scripts" / "remote-build.py"
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
