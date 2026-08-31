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

    # --- B147: the $HOME spelling, a THIRD time --------------------------
    # Measured in production: an artefact that demonstrably existed was
    # reported missing, because shlex.quote single-quotes and no shell expands
    # $HOME inside single quotes. The selftest runs the script through a real
    # bash, so restoring the shipped bug must be caught.
    ("B147: the probe re-quotes $HOME, restoring the shipped defect where an "
     "artefact that EXISTS is reported missing",
     '        f\'p="$HOME"/{shlex.quote(rel)}\\n\'',
     "        f'p={shlex.quote(chr(36) + chr(72) + chr(79) + chr(77) + chr(69) + chr(47) + rel)}\\n'"),
    ("B147 OVER-CONDEMN: the probe stops quoting the subpath, so a path with "
     "a space or a `;` is split or executed on the remote",
     '        f\'p="$HOME"/{shlex.quote(rel)}\\n\'',
     '        f\'p="$HOME"/{rel}\\n\''),
    ("B147: the probe tests a path other than the one it printed",
     '        f\'test -e "$p"\\n\'',
     "        f'test -e \"$HOME\"\\n'"),
    ("B147: the probe stops printing the path it resolved, so the not-found "
     "message falls back to the unexpanded template",
     '        f\'printf {shlex.quote(PROBE_PATH_MARK + "%s")}"\\\\n" "$p"\\n\'',
     "        f'true\\n'"),
    ("B147: parse_probe_path invents a path instead of falling back",
     "    return fallback",
     '    return "/invented/path"'),
    ("B147: parse_probe_path ignores the marker and returns any last line, so "
     "login-shell noise is mistaken for the artefact path",
     "        if line.startswith(PROBE_PATH_MARK):\n"
     "            return line[len(PROBE_PATH_MARK):].strip()",
     "        if line.strip():\n            return line.strip()"),

    # --- B147: a failed sync-back must be visible and non-zero -------------
    ("B147: the sync-back failure statuses collapse to 0, so a caller cannot "
     "tell a failed fetch from a successful one (the incident's own shape)",
     "SYNC_BACK_FAILED = 3      # asked for an artefact, did not get one\n"
     "SYNC_BACK_UNCHANGED = 4   # got one, and it is byte-identical to the old one",
     "SYNC_BACK_FAILED = 0\nSYNC_BACK_UNCHANGED = 0"),
    ("B147: 'not fetched' and 'fetched but unchanged' share a status, so a "
     "comparison script cannot branch on them",
     "SYNC_BACK_UNCHANGED = 4   # got one, and it is byte-identical to the old one",
     "SYNC_BACK_UNCHANGED = 3"),
    ("B147: the not-fetched message stops naming the stale local artefact",
     '    what = (f"The file already at that path is the PREVIOUS one "',
     '    what = (f"The file already at that path is the fine one "'),
    ("B147 OVER-CONDEMN: the not-fetched message claims a stale previous "
     "artefact even when there was no file there at all",
     "            if stale else \"Nothing is at that path.\")",
     "            if True else \"Nothing is at that path.\")"),
    ("B147: --sync-back-must-change is dropped from the local-only flag list, "
     "so it is handed to the remote shell as a command",
     '    "--sync-back-must-change",',
     '    "--sync-back-zzz",'),
    ("B147: --allow-unresolved is dropped from the local-only flag list",
     '    "--allow-unresolved",\n    # Found by',
     '    "--allow-unresolved-zzz",\n    # Found by'),
    ("B147: --disk-floor is dropped from the local-only flag list — the third "
     "omission, and the one the completeness guard found rather than a person",
     '    "--disk-floor",\n)',
     '    "--disk-floor-zzz",\n)'),

    # --- B147: the list must not be able to MISS a flag --------------------
    # Three omissions by three different changes is the LIST's problem, not any
    # author's. These mutate the guard that makes an omission impossible.
    ("B147: the parser is no longer walked, so a flag in neither tuple is "
     "silently uncovered again — this is the guard that found --disk-floor",
     "    unclassified = sorted(set(parser_flags) - classified)",
     "    unclassified = []"),
    ("B147: a classification naming a flag that no longer exists is accepted, "
     "so a RENAME leaves the real new spelling uncovered",
     "    phantom = sorted(classified - set(parser_flags))",
     "    phantom = []"),
    ("B147: a flag classified BOTH ways is accepted, so the two tuples stop "
     "saying anything",
     "    overlap = sorted(set(local_only) & set(shared))",
     "    overlap = []"),
    ("B147 OVER-CONDEMN: a flag the remote command shares is moved to the "
     "local-only list, which breaks `run -- cargo test -- --test-threads=4`",
     '    "--test-threads",        # libtest\'s own, passed after a second `--`',
     '    "--zzz-not-test-threads",'),
    ("B147: misplaced_flags stops refusing anything, so every local-only flag "
     "reaches the remote shell",
     "        if any(x == flag or x.startswith(f\"{flag}=\") for flag in LOCAL_ONLY_FLAGS)",
     "        if False"),
    ("B147 OVER-CONDEMN: misplaced_flags refuses everything, so no remote "
     "command can be run at all",
     "        if any(x == flag or x.startswith(f\"{flag}=\") for flag in LOCAL_ONLY_FLAGS)",
     "        if True"),

    # --- B147: the hash, which is what makes a comparison real -------------
    ("B147: file_sha256 fabricates a hash for a file that is not there, so "
     "'no previous artefact' becomes indistinguishable from 'identical'",
     "    if not path.is_file():\n        return None\n    h = hashlib.sha256()",
     '    if not path.is_file():\n        return "0" * 64\n    h = hashlib.sha256()'),
    ("B147: file_sha256 hashes only a prefix, so two builds differing late "
     "(which is most of them) look identical",
     "        for chunk in iter(lambda: fh.read(1 << 20), b\"\"):\n            h.update(chunk)\n    return h.hexdigest()",
     "        h.update(fh.read(64))\n    return h.hexdigest()"),
    ("B147: file_sha256 returns a constant, so every artefact is 'unchanged'",
     "    return h.hexdigest()",
     '    return "c" * 64'),
    ("B147: the unchanged-artefact message stops naming the hash",
     '        f"    sha256={digest}\\n\\n"',
     '        f"\\n\\n"'),

    # --- B167: disk order and safe orphan reclaim --------------------------
    ("B167: the disk-floor preflight is skipped, so mkdir/sync run on a full "
     "host",
     "    if allow_low_disk:\n        return",
     "    if True:\n        return"),
    ("B167 OVER-CONDEMN: --allow-low-disk no longer bypasses the floor",
     "    if allow_low_disk:\n        return",
     "    if False:\n        return"),
    ("B167: --no-sync still creates and rsyncs the remote tree",
     "    if no_sync:\n        return\n    rc, out = mkdir_fn",
     "    if False:\n        return\n    rc, out = mkdir_fn"),
    ("B167: a reclaim slug that does not match the former local root is "
     "accepted",
     "    if requested_slug != expected:",
     "    if False:"),
    ("B167: a still-registered live worktree is reclaimable",
     "    if canonical_former in canonical_registered:",
     "    if False:"),
    ("B167: an alternate requested spelling bypasses worktree registration",
     "    canonical_former = former_root.resolve(strict=False)",
     "    canonical_former = former_root"),
    ("B167: an alternate registered spelling bypasses worktree registration",
     "        path.resolve(strict=False) for path in registered",
     "        path for path in registered"),
    ("B167: an extant unregistered local path is reclaimable",
     "    if os.path.lexists(former_root):",
     "    if False:"),
    ("B167: the exact remote parent/name validation is removed",
     "    if (not pure.is_absolute() or pure.name != slug\n"
     "            or pure.parent.name != REMOTE_ROOT):",
     "    if False:"),
    ("B167: preview and final reclaim bypass canonical/symlink path safety",
     "reclaim_path_safe || {",
     "true || {"),
    ("B167: reclaim deletes by default instead of dry-run",
     "    if not execute:\n        return \"dry-run\"",
     "    if False:\n        return \"dry-run\""),
    ("B167: post-quarantine reclaim ignores an incomplete process scan",
     'if [ "$unknown" -ne 0 ]; then\n'
     '    restore_quarantine || exit $?\n'
     "    printf 'refused: post-quarantine process scan was incomplete\\n'\n"
     "    exit 5\n"
     "fi",
     'if false; then\n'
     '    restore_quarantine || exit $?\n'
     "    printf 'refused: post-quarantine process scan was incomplete\\n'\n"
     "    exit 5\n"
     "fi"),
    ("B167: post-quarantine failed fd reads are treated as absence",
     '            if [ $? -ne 0 ]; then\n'
     '                [ -e "$proc" ] && {\n'
     "                    printf 'scan-unknown: %s\\n' \"$link\"; unknown=1;\n"
     '                }\n'
     '                continue\n'
     '            fi\n'
     '            case "$resolved" in\n'
     '                "$target"|"$target"/*|"$quarantine_target"|'
     '"$quarantine_target"/*)\n'
     "                    printf 'in-use: %s -> %s\\n' \"$link\" "
     '"$resolved"; busy=1 ;;',
     '            if false; then\n'
     '                [ -e "$proc" ] && {\n'
     "                    printf 'scan-unknown: %s\\n' \"$link\"; unknown=1;\n"
     '                }\n'
     '                continue\n'
     '            fi\n'
     '            case "$resolved" in\n'
     '                "$target"|"$target"/*|"$quarantine_target"|'
     '"$quarantine_target"/*)\n'
     "                    printf 'in-use: %s -> %s\\n' \"$link\" "
     '"$resolved"; busy=1 ;;'),
    ("B167: atomic reclaim reports success without deleting the target",
     "        remove_dir_at(root_fd, quarantine, expected_target)",
     "        pass  # deletion removed"),
    ("B167: reclaim does not atomically quarantine the scanned target",
     "        os.rename(\n"
     "            slug, quarantine, src_dir_fd=root_fd, dst_dir_fd=root_fd,\n"
     "        )",
     "        os.mkdir(quarantine, dir_fd=root_fd)"),
    ("B167: reclaim ignores use acquired between initial scan and quarantine",
     'if [ "$busy" -ne 0 ]; then\n'
     '    restore_quarantine || exit $?\n'
     "    printf 'refused: target entered use during reclaim\\n'\n"
     "    exit 4\n"
     "fi",
     'if false; then\n'
     '    restore_quarantine || exit $?\n'
     "    printf 'refused: target entered use during reclaim\\n'\n"
     "    exit 4\n"
     "fi"),
    ("B167: fd-relative deletion follows a swapped cache-root symlink",
     "cache_root, slug, expected_root, expected_target = sys.argv[1:]\n"
     "flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC",
     "cache_root, slug, expected_root, expected_target = sys.argv[1:]\n"
     "flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC"),
    ("B167: deletion ignores replacement of the scanned cache-root directory",
     "        if identity(os.fstat(root_fd)) != expected_root:\n"
     "            raise RuntimeError(\"cache-root identity changed after scan\")\n"
     "        target = os.stat(slug, dir_fd=root_fd, follow_symlinks=False)",
     "        if False:\n"
     "            raise RuntimeError(\"cache-root identity changed after scan\")\n"
     "        target = os.stat(slug, dir_fd=root_fd, follow_symlinks=False)"),
    ("B167: deletion ignores replacement of the scanned slug directory",
     "        if not stat.S_ISDIR(target.st_mode) or identity(target) != expected_target:\n"
     "            raise RuntimeError(\"target identity changed after scan\")",
     "        if False:\n"
     "            raise RuntimeError(\"target identity changed after scan\")"),
    ("B167: execute skips locked local ownership revalidation",
     "        locked_problems = reclaim_request_problems(\n"
     "            former_root, requested_slug, worktree_paths_fn(root)\n"
     "        )",
     "        locked_problems = []"),
    ("B167: execute drops the atomic former-path deletion lease",
     "        with lease_context_fn(former_root):",
     "        with nullcontext():"),
    ("B167: the former-path lease can open an already occupied pathname",
     "    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC",
     "    flags = os.O_WRONLY | os.O_CREAT | os.O_CLOEXEC"),
    ("B167: normal builds stop holding the shared remote slug lock",
     "    with RemoteLock(slug), preflighted_remote_host_lock(\n"
     "        a.host, slug, disk_check_before_remote_mutation,\n"
     "    ):\n"
     "        rc, cores",
     "    with RemoteLock(slug):\n        rc, cores"),
    ("B167: the remote host lock mkdir runs before the disk preflight",
     "    disk_check()\n    with lock_factory(host, slug):\n        yield",
     "    with lock_factory(host, slug):\n        disk_check()\n        yield"),
    ("B167: current-tree clean can race a build or reclaim",
     "        with RemoteLock(slug), RemoteHostLock(a.host, slug):\n"
     "            return run_ssh",
     "        with RemoteLock(slug):\n            return run_ssh"),
    ("B167: orphan reclaim stops holding the lock normal builds share",
     "            with RemoteLock(lock_slug), RemoteHostLock(lock_host, lock_slug):",
     "            with RemoteLock(lock_slug):"),
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
