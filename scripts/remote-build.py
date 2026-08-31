#!/usr/bin/env python3
"""Run builds and tests on a remote build box instead of this machine.

Why this exists: a full `just test` here saturates the local CPU and makes the
desktop lag, and with several agent lanes working at once it is much worse.
x-alpha has 48 cores and 123 GB against this box's 16, so offloading is both
kinder and faster.

    just rtest                 # full suite on the remote
    just rtest -- --lib        # extra args pass through
    just rcheck                # cargo check
    just rclippy               # cargo clippy
    just rrun cargo build --release
    just rdoctor               # verify the remote is usable

The remote tree is a MIRROR, not a checkout: this script rsyncs the working
tree up and the remote never pulls. So uncommitted work is included, which is
the point -- you can test a dirty tree. Nothing is ever synced back over your
source unless you ask with --sync-back.

Each worktree gets its own remote directory and its own CARGO_TARGET_DIR, keyed
by a hash of the local path, so parallel lanes never share a target dir and
incremental rebuilds stay warm per lane.

Flags go BEFORE the action, always:

    remote-build.py --sync-back debug/openplanet-lsp run -- cargo build

A flag placed after the action is not a flag, it is part of the remote command.

FOOTGUN, because it fails GREEN. Arguments after `--` go to the libtest
harness, not to cargo, so `just rtest -- --test b089_funcdef_call_site` is read
as a test-NAME filter and matches nothing: the run reports `0 passed` and the
gate prints a perfectly cheerful SUITE-VERIFIED line. A green zero is not a
pass. To select a binary you want cargo's flag, before the `--`:

    just rrun cargo test --test b089_funcdef_call_site
    just rtest -- some_test_name        # name substring: works as expected

B106 pre-flight: the remote's free space is checked BEFORE anything is synced
or built, and a host under the floor (10 GiB, or $LSP_REMOTE_DISK_FLOOR_GB) is
refused by name. A full build box does not fail as "no room" -- rsync, cargo
and rustc each report it in their own dimension, and the one an operator
usually reads is cargo's `could not compile ... due to 1 previous error`, which
blames the code. `--allow-low-disk` skips the check.

`.git` is NOT synced by default. Nothing in the test suite reads repository git
state, and copying it while a lane is mid-commit could send a torn index. Pass
--with-git if you add something that needs it.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath as PurePosix

# `.get(name, default)` returns "" for a SET-BUT-EMPTY variable, which is a
# routine shell accident (`LSP_REMOTE_HOST= just ...`, an unset-looking export,
# a CI variable with no value). An empty host is not a host: ssh would be given
# `:path` and the failure names the wrong dimension. Treat empty as unset.
DEFAULT_HOST = os.environ.get("LSP_REMOTE_HOST") or "x-alpha"
REMOTE_ROOT = ".remote-builds"

# B106. x-alpha reached 0 bytes free during a batch and every lane building
# there got `error: could not compile ... due to 1 previous error` -- a message
# about the CODE, for a failure of the MACHINE. A box below this floor is not a
# build box. Overridable because 10 GiB is a judgement, not a fact -- and note
# what it buys: one lane's target dir on this tree MEASURED 7.5 GiB, so 10 GiB
# is room for the lane you are about to start and nothing else.
DEFAULT_DISK_FLOOR_GB = float(os.environ.get("LSP_REMOTE_DISK_FLOOR_GB", "10"))

# Named in the low-disk message so it points somewhere the caller is not
# already. Suggesting the host that just failed is worse than suggesting
# nothing: it reads as a tool that did not notice where it was.
KNOWN_HOSTS = ("x-alpha", "x-tuf", "x-19")

# Flags of THIS script that no cargo, just or shell command also accepts, so
# one seen inside the remote command is unambiguously a misplaced flag rather
# than part of the command. `-v`, `--jobs` and `--test-threads` are deliberately
# NOT here: `just rrun cargo build -v` is legitimate and a check that rejected
# it would be wrong more often than right.
LOCAL_ONLY_FLAGS = (
    "--sync-back",
    # B147. `--sync-back-must-change` is NOT covered by the `--sync-back`
    # entry: the check is `==` or `flag=`, so a longer flag sharing a prefix
    # falls straight through it and is handed to the remote shell, which
    # answers `--sync-back-must-change: command not found`. Found while adding
    # the flag; `--allow-unresolved` had the same gap since B132, from the
    # ordinary cause -- the list is a second place to remember.
    "--sync-back-must-change",
    "--no-sync",
    "--with-git",
    "--host",
    "--allow-low-disk",
    "--allow-unresolved",
    "--reclaim-root",
    "--remote-slug",
    "--execute-reclaim",
    # Found by the completeness guard below, not by a person: uncovered since
    # B106 added it. THREE omissions in one hand-maintained list, by three
    # different changes, is the list's problem and not any author's.
    "--disk-floor",
)

# Options of THIS script that are deliberately NOT refused after the action,
# because the remote command plausibly spells them the same way. Refusing these
# would break the thing the tool exists to do -- `run -- cargo test --
# --test-threads=4` is a normal invocation, and `--jobs`/`-v` are cargo's own.
#
# Every option must appear in exactly ONE of these two tuples, and the selftest
# enforces it. The point is not that this classification is obviously right; it
# is that adding a flag now forces the question instead of defaulting to
# silence, which is how both of the gaps above happened.
SHARED_SPELLING_FLAGS = (
    "-h", "--help",          # every program has one
    "--jobs",                # cargo -j
    "--test-threads",        # libtest's own, passed after a second `--`
    "-v", "--verbose",       # cargo's own
)

# Never sync these. `target` and the per-lane build caches dwarf the source
# tree; `.worktrees` would recursively copy every other lane's checkout.
EXCLUDES = [
    "/target/",
    "/.worktrees/",
    "/.venv/",
    "__pycache__/",
    "*.pyc",
    "/node_modules/",
    "/.cache/",
    "*.swp",
]


# B147. Distinct statuses so a caller can tell the two sync-back failures
# apart without parsing prose. `2` stays the generic refusal; these are the
# ones a before/after comparison script needs to branch on.
SYNC_BACK_FAILED = 3      # asked for an artefact, did not get one
SYNC_BACK_UNCHANGED = 4   # got one, and it is byte-identical to the old one


def flag_classification_problems(parser_flags, local_only, shared) -> list[str]:
    """Every option of this script must be classified, exactly once.

    B147. `LOCAL_ONLY_FLAGS` is a hand-maintained second copy of a subset of
    the parser's option strings, and a list cannot notice what is not in it.
    THREE flags were missing from it, added by three different changes:
    `--allow-unresolved` since B132, `--disk-floor` since B106, and
    `--sync-back-must-change` from the hour in which the first omission was
    being fixed. That is the list's problem, not any author's.

    Pure, and separated from the parser on purpose: a check that only ever
    looks at a correct classification cannot be shown to work, because its
    answer is the empty list either way. Given synthetic bad input it has to
    produce something, which is what makes it testable and what a mutation run
    can therefore kill.
    """
    problems = []
    classified = set(local_only) | set(shared)
    unclassified = sorted(set(parser_flags) - classified)
    if unclassified:
        problems.append(
            f"these options are in neither LOCAL_ONLY_FLAGS nor "
            f"SHARED_SPELLING_FLAGS, so placing one after the action would "
            f"hand it to the remote shell as `command not found` and nothing "
            f"would say so: {unclassified}"
        )
    phantom = sorted(classified - set(parser_flags))
    if phantom:
        problems.append(
            f"these are classified but are not options of this parser, so the "
            f"classification is guarding a flag that no longer exists -- a "
            f"rename leaves the REAL new spelling uncovered: {phantom}"
        )
    overlap = sorted(set(local_only) & set(shared))
    if overlap:
        problems.append(
            f"these are classified BOTH ways, so the tuples no longer say "
            f"anything: {overlap}"
        )
    return problems


def die(msg: str, code: int = 2) -> "NoReturn":  # type: ignore[valid-type]
    print(f"remote-build: {msg}", file=sys.stderr)
    sys.exit(code)


def local_root() -> Path:
    """The worktree root, so this works from a lane worktree as well as master."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        die("not inside a git repository (could not run `git rev-parse`)")
    return Path(out)


def git_branch(root: Path) -> str:
    """The checked-out branch, for the run banner. Never raises.

    Reported so a caller can see at a glance WHICH TREE is being built. `.git`
    is not synced to the remote, so this is read locally and is purely a label.
    """
    try:
        out = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True, text=True, timeout=5,
        )
        name = out.stdout.strip()
        return name if out.returncode == 0 and name else "?"
    except Exception:
        # A missing git, a detached worktree, anything at all: the banner must
        # never be the reason a build does not run.
        return "?"


# An unresolved git operation, by the sentinel git itself writes.
#
# THE MEMBERSHIP RULE IS "GIT REMOVES IT ON RESOLUTION", NOT "GIT WRITES IT
# DURING THE OPERATION" (B169). Those two are the same for three of the four
# HEAD-style files and DIFFERENT for `REBASE_HEAD`, which is why it used to be
# here and no longer is. Measured, one throwaway repo per operation, each
# conflicted and then resolved:
#
#     op            mid-conflict                  survives resolution
#     merge         MERGE_HEAD                    --
#     cherry-pick   CHERRY_PICK_HEAD              --
#     revert        REVERT_HEAD                   --
#     rebase        REBASE_HEAD, rebase-merge/    REBASE_HEAD
#
# So `REBASE_HEAD` is not evidence of an UNRESOLVED rebase: it survives a
# successful one, and git itself then says `fatal: no rebase in progress`. With
# it in this table the gate refused to run for exactly the lane that had just
# resolved a conflict and was re-gating -- the workflow this repo mandates.
# `rebase-merge/` and `rebase-apply/` are removed on resolution and are what
# git's own "is a rebase in progress" check uses.
#
# `BISECT_LOG` is deliberately ABSENT. A bisect checks out real commits and
# produces no conflict markers, so gating during one is a legitimate thing to
# want; refusing there would be over-condemning, which costs a lane its whole
# workflow to prevent a failure that cannot occur.
GIT_OPERATION_SENTINELS = (
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase"),
    ("MERGE_HEAD", "merge"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
)

# Files git writes DURING an operation but does NOT remove when it completes,
# so their presence says nothing about whether anything is unresolved. Pinned
# as a named exclusion for the same reason `BISECT_LOG` is: the next person to
# "complete" the sentinel list must be told this one was measured and excluded
# on purpose, not overlooked (B169).
NOT_UNRESOLVED_SENTINELS = ("REBASE_HEAD",)


def git_dir_for(root: Path) -> Path | None:
    """The gitdir for THIS tree, which for a worktree is not `root/.git`.

    In a linked worktree `.git` is a FILE pointing elsewhere, and the
    operation sentinels live in `.git/worktrees/<name>/`. That is the correct
    scope and not merely an implementation detail: a merge running in the main
    checkout must NOT stop a lane worktree from gating, and vice versa. Asking
    git is what gets that right; joining `.git` by hand would either miss the
    operation or attribute someone else's to you.
    """
    try:
        out = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--absolute-git-dir"],
            capture_output=True, text=True, timeout=5,
        )
        if out.returncode != 0 or not out.stdout.strip():
            return None
        return Path(out.stdout.strip())
    except Exception:
        return None


def operation_in_progress(gitdir: Path | None) -> str | None:
    """Name the unresolved git operation in this tree, or None.

    Filesystem-only and takes the gitdir as an argument, so the selftest can
    build real sentinels in a temp directory rather than mock git.
    """
    if gitdir is None:
        return None
    for name, label in GIT_OPERATION_SENTINELS:
        if (gitdir / name).exists():
            return label
    return None


def unmerged_paths(root: Path) -> list[str]:
    """Files with conflict markers, per git's index. Never raises.

    `--no-optional-locks` because this is a read on someone else's checkout: a
    plain `git diff` can take `index.lock` when the index is stat-stale, which
    races the very operation we are reporting on.
    """
    try:
        out = subprocess.run(
            ["git", "--no-optional-locks", "-C", str(root),
             "diff", "--name-only", "--diff-filter=U"],
            capture_output=True, text=True, timeout=10,
        )
        if out.returncode != 0:
            return []
        return [l for l in out.stdout.splitlines() if l.strip()]
    except Exception:
        return []


def dirty_count(root: Path) -> int | None:
    """How many paths differ from HEAD. `None` if it could not be determined."""
    try:
        out = subprocess.run(
            ["git", "--no-optional-locks", "-C", str(root),
             "status", "--porcelain"],
            capture_output=True, text=True, timeout=10,
        )
        if out.returncode != 0:
            return None
        return sum(1 for l in out.stdout.splitlines() if l.strip())
    except Exception:
        return None


def unresolved_verdict(operation: str | None, unmerged: list[str]) -> str | None:
    """The refusal message, or None to proceed. Pure, so it is testable.

    REFUSES ON AN UNRESOLVED OPERATION, NEVER ON A DIRTY TREE. Gating
    uncommitted work is the normal workflow here -- every lane does it on every
    run -- so a guard that fired on dirty would be a far worse regression than
    the defect it fixed. `operation is None` means proceed, however dirty.
    """
    if not operation:
        return None
    lines = [
        f"the working tree has an unresolved {operation}, so gating it would "
        f"measure conflict markers rather than your code.",
    ]
    if unmerged:
        shown = unmerged[:10]
        lines.append(f"\n  {len(unmerged)} unmerged file(s):")
        lines.extend(f"    {p}" for p in shown)
        if len(unmerged) > len(shown):
            lines.append(f"    ... and {len(unmerged) - len(shown)} more")
    else:
        # A stopped rebase can have every conflict staged and still be
        # unfinished. Say so rather than printing an empty list, which would
        # read as "nothing is wrong".
        lines.append(
            f"\n  No files are currently unmerged, but the {operation} has not "
            f"been finished, so HEAD is not what you think you are gating."
        )
    lines.append(
        "\nThis gate rsyncs FILES, not commits. It cannot see the git index, so "
        "`<<<<<<<` reaches the compiler as syntax and comes back as an error IN "
        "YOUR CODE -- which is the wrong diagnosis and has cost real time. In a "
        "shared checkout the operation may belong to ANOTHER SESSION."
    )
    # B169: print the invocation that actually works. `--allow-unresolved` is a
    # remote-build FLAG, and flags go BEFORE the action -- so it cannot be
    # passed through `just rtest`, which forwards everything after its own `--`
    # to libtest. Advising "pass --allow-unresolved" without saying that made
    # `just rtest --allow-unresolved` reprint THIS SAME REFUSAL, handing the
    # reader a second instance of the fault instead of a way out.
    lines.append(
        f"\nFinish or abort it (`git {operation} --continue` / "
        f"`git {operation} --abort`), or gate a different worktree."
    )
    lines.append(
        "\nTo gate the tree exactly as it sits, use the flag form -- it must "
        "come BEFORE the action, so it cannot go through `just rtest`:\n"
        "    python3 scripts/remote-build.py --allow-unresolved test"
    )
    return "\n".join(lines)


def slug_for(root: Path) -> str:
    """A stable per-worktree name.

    The basename alone collides -- every lane worktree under .worktrees/ could
    be named the same as another repo's. The path hash makes it unique while
    keeping the readable part readable.
    """
    digest = hashlib.sha256(str(root.resolve()).encode()).hexdigest()[:8]
    return f"{root.name}-{digest}"


class RemoteLock:
    """One build per worktree at a time.

    Two concurrent runs from the same worktree would rsync over each other's
    tree mid-compile, which produces failures that look like real ones. This is
    advisory and local-only: it serialises this machine's invocations, which is
    where the races come from.
    """

    def __init__(self, slug: str) -> None:
        d = Path.home() / ".cache" / "lsp-remote-build"
        d.mkdir(parents=True, exist_ok=True)
        self.path = d / f"{slug}.lock"
        self.fh = None

    def __enter__(self) -> "RemoteLock":
        self.fh = self.path.open("w")
        try:
            fcntl.flock(self.fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(
                f"remote-build: another run holds {self.path.name}; waiting...",
                file=sys.stderr,
            )
            fcntl.flock(self.fh, fcntl.LOCK_EX)
        return self

    def __exit__(self, *_exc) -> None:
        if self.fh:
            fcntl.flock(self.fh, fcntl.LOCK_UN)
            self.fh.close()


REMOTE_LOCK_MARK = "__REMOTE_BUILD_LOCKED__"


class RemoteHostLock:
    """Crash-safe per-slug flock held on the build host.

    The local lock coordinates agents on this machine. This one coordinates
    every cooperating caller that can reach the shared host, and lives outside
    the tree so reclaiming the tree cannot remove the lock protecting it.
    """

    def __init__(self, host: str, slug: str) -> None:
        self.host = host
        self.slug = slug
        self.proc: subprocess.Popen | None = None

    def __enter__(self) -> "RemoteHostLock":
        lock_rel = f".remote-build-locks/{self.slug}.lock"
        hold = f"printf '%s\\n' {shlex.quote(REMOTE_LOCK_MARK)}; cat >/dev/null"
        script = (
            'mkdir -p "$HOME/.remote-build-locks" && '
            f'exec flock -x "$HOME"/{shlex.quote(lock_rel)} '
            f'sh -c {shlex.quote(hold)}'
        )
        # One quoted remote command string avoids ssh re-splitting bash -lc's
        # script into $0/$1 (the same rule documented in run_ssh below).
        cmd = ssh_base(self.host) + [f"bash -lc {shlex.quote(script)}"]
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            text=True, bufsize=1,
        )
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            if line.strip() == REMOTE_LOCK_MARK:
                return self
        rc = self.proc.wait()
        raise RuntimeError(
            f"could not acquire remote build lock for {self.host}:{self.slug} "
            f"(ssh exit {rc})"
        )

    def __exit__(self, *_exc) -> None:
        if not self.proc:
            return
        if self.proc.stdin:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            self.proc.wait(timeout=5)


@contextmanager
def preflighted_remote_host_lock(
    host: str, slug: str, disk_check, *, lock_factory=None,
):
    """Measure disk before the host lock can mkdir, then enter that lock.

    The caller still runs ``preflight_remote_tree`` inside this context. That
    is the second disk measurement, under the lock and immediately before the
    source/target mkdir and rsync.
    """
    lock_factory = lock_factory or RemoteHostLock
    disk_check()
    with lock_factory(host, slug):
        yield


def ssh_base(host: str) -> list[str]:
    return [
        "ssh",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=40",
        host,
    ]


# The script is fed to the remote shell on STDIN rather than as an argument.
# `ssh host bash -lc "$script"` does NOT work: ssh joins its command words with
# spaces and the remote shell re-splits them, so `bash -lc` receives only the
# first word and everything after it becomes $0, $1, ... Silently. `rustc
# --version` came back as rustc's usage text, and a compound command lost
# everything after its first token.
def run_ssh(host: str, script: str, *, quiet: bool = False) -> int:
    """Run a shell snippet on the remote, streaming its output."""
    cmd = ssh_base(host) + ["bash", "-l", "-s"]
    if quiet:
        return subprocess.run(cmd, input=script, text=True,
                              capture_output=True).returncode
    return subprocess.run(cmd, input=script, text=True).returncode


def capture_ssh(host: str, script: str) -> tuple[int, str]:
    cmd = ssh_base(host) + ["bash", "-l", "-s"]
    p = subprocess.run(cmd, input=script, text=True, capture_output=True)
    return p.returncode, (p.stdout + p.stderr).strip()


def remote_paths(slug: str) -> dict[str, str]:
    """Every remote path this script uses, in ONE place, in both spellings.

    ssh runs a shell, so ``$HOME`` expands there. rsync does NOT run a shell on
    its remote path: it passes it literally, so ``$HOME/x`` becomes the absurd
    ``/home/user/$HOME/x``. rsync's remote paths are already relative to the
    login directory, so the bare relative form is the correct one for it.

    That one rule has been got wrong twice, once per direction, by two
    different people. It is now one function and one selftest assertion: no
    value whose key starts with ``rsync_`` may contain ``$HOME``.
    """
    rel_base = f"{REMOTE_ROOT}/{slug}"
    base = f"$HOME/{rel_base}"
    return {
        "rel_base": rel_base,
        "base": base,
        "ssh_src": f"{base}/src",
        "ssh_target": f"{base}/target",
        "rsync_src": f"{rel_base}/src",
        "rsync_target": f"{rel_base}/target",
    }


def exit_status(rc: int) -> tuple[int, str | None]:
    """A shell-style status, plus a note when the remote session was SIGNALLED.

    ``subprocess`` reports a signal death as a NEGATIVE returncode, and
    ``sys.exit(-15)`` is taken modulo 256, so a killed ssh surfaces as 241 --
    nonzero, therefore safe, but naming nothing. Convert to the shell's own
    128+N and say which signal, so a session the network dropped is
    distinguishable from a remote command that chose to exit 143. B106 is about
    statuses that survive; a status that survives but lies about its cause is
    only half of that.
    """
    if rc < 0:
        return 128 - rc, f"the remote session was killed by signal {-rc}"
    if rc == 255:
        # B139. ssh's OWN failure status: host unreachable, auth refused, DNS
        # gone. Reported as a bare `exit 255` it reads as a verdict about the
        # command, which is the batch's recurring shape -- the tool naming
        # what it saw rather than why it could not see. A NOTE and not a
        # refusal: 255 is a status a remote command may legitimately choose,
        # so this is data for the reader, not a decision taken from them.
        return rc, ("255 is ssh's own failure status — the remote command may "
                    "never have started. Check connectivity to the host "
                    "before reading this as a result")
    return rc, None


# Top-level directories of the SOURCE tree. `--sync-back` resolves under the
# remote CARGO_TARGET_DIR, so naming one of these is always a mistake -- and
# the mistake is worth naming, because rsync's own answer to it is
# `Broken pipe (32)`.
SOURCE_TREE_DIRS = ("src", "scripts", "tests", "docs", "benches", "examples",
                    "assets", "fixtures", ".github")


def sync_back_verdict(subpath: str | None) -> str | None:
    """Refuse a `--sync-back` path that cannot mean what the caller intends.

    B140. `--sync-back` is resolved under the remote CARGO_TARGET_DIR at one
    end and the local `target/` at the other, because its purpose is to fetch
    a BUILD ARTEFACT. Hand it a source path and rsync is asked for a file that
    was never going to be there; what comes back is
    `rsync: Broken pipe (32)`, which names the transport and says nothing
    about the mistake. Same family as B106 and B132: an infrastructure-shaped
    error standing in for a condition the tool could have recognised.

    Checked BEFORE the remote command runs. Refusing after a successful
    five-minute build would be correct and useless.

    Absolute paths and `..` are refused for a different reason: `Path("/a/b")
    / "/etc/x"` is `/etc/x`, so an absolute subpath silently relocates the
    destination OUT of `target/`, and the module docstring's promise that
    nothing is ever written over your source would quietly stop being true.
    """
    if subpath is None:
        return None
    if not subpath.strip():
        return ("--sync-back was given an empty path. It takes a path under "
                "the remote target dir, e.g. `--sync-back debug/openplanet-lsp`.")
    if PurePosix(subpath).is_absolute():
        return (
            f"--sync-back {subpath!r} is an absolute path. It is resolved under "
            f"the LOCAL `target/` directory, and joining an absolute path "
            f"discards that prefix entirely -- so this would write to "
            f"{subpath!r} itself, outside the target dir and potentially over "
            f"your source. Pass a path relative to the target dir, e.g. "
            f"`debug/openplanet-lsp`."
        )
    if ".." in PurePosix(subpath).parts:
        return (
            f"--sync-back {subpath!r} contains `..`, which escapes the target "
            f"directory it is resolved under. Pass a path relative to the "
            f"target dir, e.g. `debug/openplanet-lsp`."
        )
    head = PurePosix(subpath).parts[0]
    if head in SOURCE_TREE_DIRS:
        return (
            f"--sync-back {subpath!r} looks like a SOURCE path, but "
            f"--sync-back is resolved under the remote CARGO_TARGET_DIR -- it "
            f"fetches build artefacts, not sources. `{head}/` does not exist "
            f"there, and rsync reports that as `Broken pipe (32)`, which names "
            f"the transport rather than the mistake.\n\n"
            f"If you meant a build artefact, name it relative to the target "
            f"dir (e.g. `debug/openplanet-lsp`). This gate never writes over "
            f"your source tree; syncing is one-way, local -> remote."
        )
    return None


def sync_back_missing_verdict(subpath: str, host: str, remote_path: str) -> str:
    """`--sync-back` named something the remote does not have (B140).

    The static check above catches the shapes that are wrong by construction.
    This is the residual: a plausible target-relative path that simply is not
    there -- a binary that was not built, a stale name, the wrong profile.
    Asking `test -e` first turns rsync's `Broken pipe (32)` into the path that
    was looked for.
    """
    return (
        f"--sync-back {subpath!r} does not exist on {host}:\n"
        f"    {remote_path}\n\n"
        f"The path is resolved under the remote CARGO_TARGET_DIR, so it names "
        f"a build artefact. Check the profile (`debug/` vs `release/`) and "
        f"that the run actually produced it -- a `cargo check` builds no "
        f"binary. Nothing was written locally."
    )


# Marker the remote existence probe prints its RESOLVED path behind, so the
# path can be recovered from a login shell's motd/profile noise. Matched at the
# start of a line and taken from the LAST match, because a `.bashrc` that echoes
# is common and one that echoes this exact prefix is not.
PROBE_PATH_MARK = "remote-build-resolved: "


def sync_back_probe_script(rel_base: str, subpath: str) -> str:
    """A remote script that PRINTS the path it resolved, then tests it.

    B147, and it is the `remote_paths` rule failing in a THIRD direction. The
    previous form was `test -e {shlex.quote(remote_path)}` where remote_path
    began with the literal `$HOME`. `shlex.quote` wraps its argument in SINGLE
    quotes, and no shell expands `$HOME` inside single quotes -- so the probe
    asked whether a path with a literal dollar-sign in it existed. It never
    does. Every `--sync-back` of an artefact that WAS on the remote was
    reported missing, and the refusal then printed the same unexpanded string,
    so the operator was shown a path that is not the path that was checked and
    could not reproduce the failure by hand.

    Both halves of the rule have to hold at once, in one string:

    * `$HOME` must be expanded, so it must NOT be quoted;
    * the caller's subpath must NOT be interpreted, so it MUST be quoted.

    An unquoted `"$HOME"` concatenated with a `shlex.quote`d relative path is
    exactly that, and it is why this cannot be written as one `shlex.quote` of
    the whole thing. The quoting also matters for a second reason: the subpath
    is operator input that reaches a remote shell, and `debug/x; rm -rf ~`
    should be a path that does not exist, not a command.

    The script prints before it tests, so the resolved path is available
    whether or not the artefact is there -- the not-found message needs it
    precisely in the case where the test failed.
    """
    rel = f"{rel_base}/target/{subpath}"
    return (
        f'p="$HOME"/{shlex.quote(rel)}\n'
        f'printf {shlex.quote(PROBE_PATH_MARK + "%s")}"\\n" "$p"\n'
        f'test -e "$p"\n'
    )


def parse_probe_path(out: str, fallback: str) -> str:
    """Recover the resolved path the probe printed, ignoring login noise."""
    for line in reversed(out.splitlines()):
        if line.startswith(PROBE_PATH_MARK):
            return line[len(PROBE_PATH_MARK):].strip()
    return fallback


def file_sha256(path: Path) -> str | None:
    """Content hash of a local file, or None if it is not there.

    None is a THIRD answer and not an error: "there was no previous artefact"
    is a legitimate before-state, and it is the one case where an unchanged
    hash cannot be a stale-artefact bug.
    """
    if not path.is_file():
        return None
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sync_back_not_fetched_verdict(subpath: str, dest: Path, status: int,
                                  stale: str | None) -> str:
    """The remote command FAILED, so `--sync-back` was not attempted (B147).

    The old code spelled this `if status == 0 and a.sync_back:` and then said
    nothing at all. The exit status was the COMMAND's, which is correct and is
    not the point: the operator asked for an artefact, did not get one, and
    still has the previous one sitting at the destination with a plausible
    mtime. Every later step reads a file that exists and is wrong.

    This is the stale-artefact incident's actual shape. A caller who checks the
    status is fine either way; a caller who looks at the FILE needs to be told
    the file is not what they asked for.
    """
    what = (f"The file already at that path is the PREVIOUS one "
            f"(sha256={stale[:16]}...). It was not replaced."
            if stale else "Nothing is at that path.")
    return (
        f"--sync-back {subpath!r} was NOT fetched: the remote command failed "
        f"(exit {status}), so there is no new artefact to fetch.\n"
        f"    {dest}\n"
        f"{what}\n\n"
        f"Anything downstream that reads that path is reading STALE state. "
        f"This exits with the command's own status ({status}), not a separate "
        f"one, because the command's failure is the thing to fix."
    )


def sync_back_unchanged_verdict(subpath: str, dest: Path, digest: str,
                                host: str) -> str:
    """`--sync-back-must-change` fetched a byte-identical artefact (B147).

    The measured incident: a before/after fleet comparison reported IDENTICAL
    while both halves ran the SAME binary, because the sync between them had
    not taken effect. The comparison was perfectly honest -- two identical
    inputs give an identical answer. Nothing told it the inputs were SUPPOSED
    to differ.

    That is the pipe-eats-the-status family one layer out: not a lost STATUS
    but a lost EFFECT, with the downstream step reporting success on stale
    state. A no-op sync and a genuine no-change are indistinguishable from the
    artefact alone, so the caller who knows the difference must say so -- which
    is what this flag is. Silence is the wrong default here for the same reason
    it is everywhere else in this file, so the hash is PRINTED on every fetch
    whether or not the flag is passed.
    """
    return (
        f"--sync-back-must-change: the artefact fetched from {host} is "
        f"BYTE-IDENTICAL to the one that was already there.\n"
        f"    {dest}\n"
        f"    sha256={digest}\n\n"
        f"You asserted this build would produce a different artefact, and it "
        f"did not. Either the remote did not rebuild (check that the source "
        f"sync ran and that the command actually builds), or the change has no "
        f"effect on this profile.\n\n"
        f"This is refused rather than warned because the next step is a "
        f"comparison, and comparing a binary against itself reports IDENTICAL "
        f"-- an honest answer to a question you did not mean to ask."
    )


def _other_host(host: str) -> str:
    """A build box that is NOT the one that just failed."""
    return next((h for h in KNOWN_HOSTS if h != host), "<another-box>")


def disk_floor_verdict(
    free_bytes: int | None, floor_gb: float, host: str, path: str
) -> str | None:
    """``None`` when the remote has room; otherwise the message to die with.

    An unreadable ``df`` returns a message too. "I could not measure it" and
    "there is room" are different states; rendering both as ``None`` is the
    silence-equals-success mistake this whole file exists to avoid.
    """
    if free_bytes is None:
        return (
            f"could not read free space on {host}:{path}. That is NOT the same "
            f"as having room. Pass --allow-low-disk to build there anyway."
        )
    if free_bytes >= floor_gb * (1024 ** 3):
        return None
    return (
        f"{host} has {free_bytes / 1024 ** 3:.1f} GiB free on the filesystem "
        f"holding {path}, under the {floor_gb:g} GiB floor.\n"
        f"  A build there dies with `No space left on device`, and cargo "
        f"reports that as `could not compile ... due to 1 previous error` -- a "
        f"message about your code (B106).\n"
        f"  Free some: `just rclean` drops THIS worktree's remote tree; other "
        f"lanes' trees are under ~/{REMOTE_ROOT}/ on {host}.\n"
        f"  Or use another box:  LSP_REMOTE_HOST={_other_host(host)} "
        f"just <recipe>\n"
        f"  Or override:         --allow-low-disk, "
        f"or LSP_REMOTE_DISK_FLOOR_GB=<n>."
    )


def misplaced_flags(inner: list[str]) -> list[str]:
    """Flags of this script that ended up inside the remote command."""
    return [
        x
        for x in inner
        if any(x == flag or x.startswith(f"{flag}=") for flag in LOCAL_ONLY_FLAGS)
    ]


def parse_df_avail(output: str) -> int | None:
    """Available bytes from one ``df -Pk`` data line, or None if unreadable."""
    for line in reversed(output.strip().splitlines()):
        fields = line.split()
        # POSIX -P: Filesystem 1024-blocks Used Available Capacity Mounted-on
        if len(fields) >= 4 and fields[3].isdigit() and fields[1].isdigit():
            return int(fields[3]) * 1024
    return None


def remote_free_bytes(host: str, rel_base: str) -> int | None:
    """Free bytes on the remote filesystem that will hold this lane's TARGET.

    The target dir is what fills -- one measured 7.5 GiB against a 23 MiB
    source tree -- and it may be a different filesystem from the source, so
    measure that path specifically rather than the tree root.

    Measures the deepest EXISTING ancestor of it: neither the target nor the
    tree exists on a first run, and a check that skips itself then is absent
    exactly when the directory is about to be created.
    """
    script = (
        'cd "$HOME" || exit 1; d=' + shlex.quote(f"{rel_base}/target") + '; '
        'while [ ! -d "$d" ]; do d=$(dirname "$d"); done; '
        'df -Pk "$d" 2>/dev/null'
    )
    rc, out = capture_ssh(host, script)
    if rc != 0:
        return None
    return parse_df_avail(out)


def sync_up(host: str, root: Path, remote_src: str, with_git: bool, verbose: bool) -> None:
    args = ["rsync", "-az", "--delete", "--delete-excluded"]
    for pat in EXCLUDES:
        args += ["--exclude", pat]
    if not with_git:
        args += ["--exclude", "/.git", "--exclude", "/.git/"]
    if verbose:
        args.append("--info=stats1")
    # Deliberately NOT --quiet. It suppresses rsync's own diagnostics, and the
    # first real failure here ("mkdir /home/user/$HOME/... failed") was invisible
    # until it was removed. A sync is fast and silent on success anyway.
    # Trailing slash on the source: copy the CONTENTS of root into remote_src.
    args += [f"{root}/", f"{host}:{remote_src}/"]
    t0 = time.monotonic()
    rc = subprocess.run(args).returncode
    if rc != 0:
        die(f"rsync failed (exit {rc})", rc)
    if verbose:
        print(f"remote-build: synced in {time.monotonic() - t0:.1f}s", file=sys.stderr)


def preflight_remote_disk(
    *, host: str, rel_base: str, remote_target: str, floor_gb: float,
    allow_low_disk: bool, free_bytes_fn=None,
) -> None:
    """Refuse low/unknown disk without making any remote mutation."""
    if allow_low_disk:
        return
    free_bytes_fn = free_bytes_fn or remote_free_bytes
    verdict = disk_floor_verdict(
        free_bytes_fn(host, rel_base), floor_gb, host, remote_target,
    )
    if verdict:
        die(verdict)


def preflight_remote_tree(
    *, host: str, rel_base: str, remote_src: str, remote_target: str,
    rsync_dest: str, root: Path, floor_gb: float, allow_low_disk: bool,
    no_sync: bool, with_git: bool, verbose: bool,
    free_bytes_fn=None, mkdir_fn=None, sync_fn=None,
) -> None:
    """Check disk first, then (and only then) create and sync the tree."""
    mkdir_fn = mkdir_fn or capture_ssh
    sync_fn = sync_fn or sync_up
    preflight_remote_disk(
        host=host, rel_base=rel_base, remote_target=remote_target,
        floor_gb=floor_gb, allow_low_disk=allow_low_disk,
        free_bytes_fn=free_bytes_fn,
    )

    if no_sync:
        return
    rc, out = mkdir_fn(host, f"mkdir -p {remote_src} {remote_target}")
    if rc != 0:
        die(f"could not create the remote tree on {host} "
            f"(mkdir exited {rc}):\n  {out}", rc)
    sync_fn(host, root, rsync_dest, with_git, verbose)


def git_worktree_paths(root: Path) -> set[Path]:
    """Registered worktrees for this repository, from git's own ledger."""
    out = subprocess.run(
        ["git", "-C", str(root), "worktree", "list", "--porcelain"],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        die("could not enumerate local git worktrees before remote inventory")
    return {
        Path(line.removeprefix("worktree "))
        for line in out.stdout.splitlines()
        if line.startswith("worktree ")
    }


def reclaim_request_problems(former_root: Path, requested_slug: str,
                             registered: set[Path]) -> list[str]:
    """Pure validation for the two-factor former-root + slug reclaim key."""
    problems = []
    if not former_root.is_absolute():
        problems.append("--reclaim-root must be an absolute former worktree path")
        return problems
    if not re.fullmatch(r"[A-Za-z0-9._-]+-[0-9a-f]{8}", requested_slug or ""):
        problems.append(
            f"--remote-slug {requested_slug!r} is not a remote-build slug"
        )
        return problems
    canonical_former = former_root.resolve(strict=False)
    canonical_registered = {
        path.resolve(strict=False) for path in registered
    }
    expected = slug_for(former_root)
    if requested_slug != expected:
        problems.append(
            f"--remote-slug is {requested_slug!r}, but --reclaim-root "
            f"{former_root} deterministically maps to {expected!r}. Refusing "
            f"to guess which remote tree was intended."
        )
    if canonical_former in canonical_registered:
        problems.append(
            f"{former_root} is still a registered git worktree; use `just "
            f"rclean` from that worktree instead"
        )
    if os.path.lexists(former_root):
        problems.append(
            f"{former_root} still exists locally. Reclaim is only for a "
            f"worktree that has already been removed."
        )
    return problems


def remote_inventory_script() -> str:
    """List exact remote tree names and sizes; mtime is deliberately absent."""
    return r'''root="$HOME/.remote-builds"
[ -d "$root" ] || exit 0
for d in "$root"/*; do
    [ -d "$d" ] && [ ! -L "$d" ] || continue
    kib=$(du -sk -- "$d" 2>/dev/null | awk '{print $1}') || continue
    printf '%s\t%s\n' "$kib" "${d##*/}"
done | sort -nr
'''


def parse_remote_inventory(output: str) -> list[tuple[str, int]]:
    rows = []
    for line in output.splitlines():
        fields = line.split("\t", 1)
        if len(fields) != 2 or not fields[0].isdigit():
            continue
        rows.append((fields[1], int(fields[0])))
    return rows


def cmd_inventory(host: str, root: Path) -> int:
    registered = git_worktree_paths(root)
    local = {slug_for(path): path for path in registered}
    rc, out = capture_ssh(host, remote_inventory_script())
    if rc != 0:
        print(out, file=sys.stderr)
        return rc
    print(f"remote-build inventory: {host}:~/{REMOTE_ROOT}")
    print("STATE             GiB  REMOTE SLUG -> LOCAL WORKTREE")
    remote_names = set()
    for slug, kib in parse_remote_inventory(out):
        remote_names.add(slug)
        # B167 measured hundreds of empty r-*/wt-* test directories. Keep
        # them visible to matching logic but out of the operator's inventory;
        # they are not the multi-GiB target trees this command exists to map.
        if kib < 1024 and slug not in local:
            continue
        if slug in local:
            state, owner = "REGISTERED", str(local[slug])
        else:
            state = "NO-LOCAL-MATCH"
            owner = "not proof of orphanhood; confirm with the coordinator"
        print(f"{state:16} {kib / 1024 ** 2:5.1f}  {slug} -> {owner}")
    for slug, path in sorted(local.items()):
        if slug not in remote_names:
            print(f"REMOTE-ABSENT      0.0  {slug} -> {path}")
    print("(unmatched remote trees under 1 MiB omitted; no mtime-based stale "
          "inference is made)")
    return 0


RECLAIM_PATH_MARK = "__REMOTE_BUILD_RECLAIM_PATH__="


def validated_remote_reclaim_path(path: str, slug: str) -> str | None:
    pure = PurePosix(path)
    if (not pure.is_absolute() or pure.name != slug
            or pure.parent.name != REMOTE_ROOT):
        return None
    return path


def remote_reclaim_path_guard_script() -> str:
    """Shell guard shared by preview and the final locked deletion."""
    return r'''reclaim_path_safe() {
    test -d "$cache_root" && test ! -L "$cache_root" || return 1
    test -d "$target" && test ! -L "$target" || return 1
    test "$target" = "$cache_root/$slug" || return 1
    home_real=$(realpath -e -- "$home_root") || return 1
    cache_real=$(realpath -e -- "$cache_root") || return 1
    target_real=$(realpath -e -- "$target") || return 1
    test "$cache_real" = "$home_real/.remote-builds" || return 1
    test "$target_real" = "$cache_real/$slug" || return 1
}
reclaim_path_safe || {
    printf 'refused: unsafe cache root or canonical target: %s\n' "$target"
    exit 8
}
'''


def remote_reclaim_probe_script(slug: str) -> str:
    """Validate and print one canonical, non-symlinked cache target."""
    return (
        'home_root="$HOME"\n'
        f'cache_root="$home_root"/{shlex.quote(REMOTE_ROOT)}\n'
        f'slug={shlex.quote(slug)}\n'
        'target="$cache_root/$slug"\n'
        + remote_reclaim_path_guard_script()
        + f'printf {shlex.quote(RECLAIM_PATH_MARK + "%s")}"\\n" "$target"\n'
    )


def resolve_remote_reclaim_path(host: str, slug: str) -> tuple[str | None, str]:
    script = remote_reclaim_probe_script(slug)
    rc, out = capture_ssh(host, script)
    path = parse_probe_path(out.replace(RECLAIM_PATH_MARK, PROBE_PATH_MARK), "")
    if rc != 0:
        return None, out
    if validated_remote_reclaim_path(path, slug) is None:
        return None, f"remote resolved an unsafe reclaim path: {path!r}"
    return path, out


def fd_relative_reclaim_script(before_delete_script: str | None = None) -> str:
    """Quarantine, rescan, then delete through an O_NOFOLLOW cache-root FD."""
    hook = f"{before_delete_script}\n" if before_delete_script else ""
    return hook + r'''quarantine_name=$(python3 - "$cache_root" "$slug" \
    "$cache_root_identity" "$target_identity" <<'PY'
import os
import secrets
import stat
import sys

cache_root, slug, expected_root, expected_target = sys.argv[1:]
flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC

def identity(info: os.stat_result) -> str:
    return f"{info.st_dev}:{info.st_ino}"

try:
    root_fd = os.open(cache_root, flags)
    try:
        if identity(os.fstat(root_fd)) != expected_root:
            raise RuntimeError("cache-root identity changed after scan")
        target = os.stat(slug, dir_fd=root_fd, follow_symlinks=False)
        if not stat.S_ISDIR(target.st_mode) or identity(target) != expected_target:
            raise RuntimeError("target identity changed after scan")
        for _ in range(32):
            quarantine = f".reclaim-quarantine-{slug}-{secrets.token_hex(16)}"
            try:
                os.stat(quarantine, dir_fd=root_fd, follow_symlinks=False)
            except FileNotFoundError:
                break
        else:
            raise RuntimeError("could not allocate a private quarantine name")
        os.rename(
            slug, quarantine, src_dir_fd=root_fd, dst_dir_fd=root_fd,
        )
        moved = os.stat(quarantine, dir_fd=root_fd, follow_symlinks=False)
        if not stat.S_ISDIR(moved.st_mode) or identity(moved) != expected_target:
            raise RuntimeError("quarantined target identity changed")
        try:
            os.stat(slug, dir_fd=root_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise RuntimeError("original target still exists after quarantine")
        print(quarantine)
    finally:
        os.close(root_fd)
except Exception as error:
    print(f"refused: target quarantine failed: {error}", file=sys.stderr)
    raise SystemExit(8)
PY
)
quarantine_rc=$?
if [ "$quarantine_rc" -ne 0 ]; then
    printf 'refused: target quarantine failed\n'
    exit "$quarantine_rc"
fi
case "$quarantine_name" in
    .reclaim-quarantine-"$slug"-[0-9a-f]*) ;;
    *) printf 'refused: invalid quarantine name: %s\n' "$quarantine_name"; exit 8 ;;
esac
quarantine_target="$cache_root/$quarantine_name"

# Renaming closes the public pathname to new entrants. Rescan both spellings:
# existing cwd/fd references follow the inode to its quarantine name, while a
# synthetic or concurrently disappearing /proc link can still show the old
# spelling and must conservatively block deletion.
busy=0
unknown=0
for proc in "$proc_root"/[0-9]*; do
    owner=$(stat -c %u -- "$proc" 2>/dev/null)
    if [ $? -ne 0 ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s owner\n' "$proc"; unknown=1; }
        continue
    fi
    [ "$owner" = "$uid" ] || continue

    if [ -L "$proc/cwd" ]; then
        resolved=$(readlink -f -- "$proc/cwd" 2>/dev/null)
        if [ $? -ne 0 ]; then
            [ -e "$proc" ] && { printf 'scan-unknown: %s/cwd\n' "$proc"; unknown=1; }
        else
            case "$resolved" in
                "$target"|"$target"/*|"$quarantine_target"|"$quarantine_target"/*)
                    printf 'in-use: %s/cwd -> %s\n' "$proc" "$resolved"; busy=1 ;;
            esac
        fi
    fi

    if [ ! -r "$proc/fd" ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s/fd\n' "$proc"; unknown=1; }
    else
        for link in "$proc"/fd/*; do
            [ -e "$link" ] || [ -L "$link" ] || continue
            resolved=$(readlink -f -- "$link" 2>/dev/null)
            if [ $? -ne 0 ]; then
                [ -e "$proc" ] && {
                    printf 'scan-unknown: %s\n' "$link"; unknown=1;
                }
                continue
            fi
            case "$resolved" in
                "$target"|"$target"/*|"$quarantine_target"|"$quarantine_target"/*)
                    printf 'in-use: %s -> %s\n' "$link" "$resolved"; busy=1 ;;
            esac
        done
    fi

    grep -Fq -- "$target/" "$proc/maps" 2>/dev/null
    old_maps_rc=$?
    grep -Fq -- "$quarantine_target/" "$proc/maps" 2>/dev/null
    quarantine_maps_rc=$?
    if [ "$old_maps_rc" -eq 0 ] || [ "$quarantine_maps_rc" -eq 0 ]; then
        printf 'in-use: %s/maps\n' "$proc"; busy=1
    elif { [ "$old_maps_rc" -ne 1 ] || [ "$quarantine_maps_rc" -ne 1 ]; } \
            && [ -e "$proc" ]; then
        printf 'scan-unknown: %s/maps\n' "$proc"; unknown=1
    fi

    cmdline=$(tr '\0' ' ' < "$proc/cmdline" 2>/dev/null)
    if [ $? -ne 0 ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s/cmdline\n' "$proc"; unknown=1; }
    else
        case "$cmdline" in
            *"$target"*|*"$quarantine_target"*)
                printf 'in-use: %s/cmdline: %s\n' "$proc" "$cmdline"; busy=1 ;;
        esac
    fi
done

restore_quarantine() {
    python3 - "$cache_root" "$slug" "$quarantine_name" \
        "$cache_root_identity" "$target_identity" <<'PY'
import os
import stat
import sys

cache_root, slug, quarantine, expected_root, expected_target = sys.argv[1:]
flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC

def identity(info: os.stat_result) -> str:
    return f"{info.st_dev}:{info.st_ino}"

try:
    root_fd = os.open(cache_root, flags)
    try:
        if identity(os.fstat(root_fd)) != expected_root:
            raise RuntimeError("cache-root identity changed before restore")
        moved = os.stat(quarantine, dir_fd=root_fd, follow_symlinks=False)
        if not stat.S_ISDIR(moved.st_mode) or identity(moved) != expected_target:
            raise RuntimeError("quarantined target identity changed before restore")
        try:
            os.stat(slug, dir_fd=root_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise RuntimeError("original target pathname was reoccupied")
        os.rename(
            quarantine, slug, src_dir_fd=root_fd, dst_dir_fd=root_fd,
        )
    finally:
        os.close(root_fd)
except Exception as error:
    print(f"refused: could not restore quarantined target: {error}")
    raise SystemExit(8)
PY
}

if [ "$unknown" -ne 0 ]; then
    restore_quarantine || exit $?
    printf 'refused: post-quarantine process scan was incomplete\n'
    exit 5
fi
if [ "$busy" -ne 0 ]; then
    restore_quarantine || exit $?
    printf 'refused: target entered use during reclaim\n'
    exit 4
fi

python3 - "$cache_root" "$slug" "$quarantine_name" \
    "$cache_root_identity" "$target_identity" <<'PY'
import os
import stat
import sys

cache_root, slug, quarantine, expected_root, expected_target = sys.argv[1:]
flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC

def identity(info: os.stat_result) -> str:
    return f"{info.st_dev}:{info.st_ino}"

def remove_dir_at(
    parent_fd: int, name: str, expected_identity: str | None = None,
) -> None:
    child_fd = os.open(name, flags, dir_fd=parent_fd)
    opened = os.fstat(child_fd)
    try:
        if expected_identity is not None and identity(opened) != expected_identity:
            raise RuntimeError(f"directory identity changed after scan: {name}")
        with os.scandir(child_fd) as entries:
            for entry in list(entries):
                if entry.is_dir(follow_symlinks=False):
                    remove_dir_at(child_fd, entry.name)
                else:
                    os.unlink(entry.name, dir_fd=child_fd)
    finally:
        os.close(child_fd)
    current = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (
        opened.st_dev, opened.st_ino,
    ):
        raise RuntimeError(f"directory identity changed before removal: {name}")
    os.rmdir(name, dir_fd=parent_fd)

try:
    root_fd = os.open(cache_root, flags)
    try:
        if identity(os.fstat(root_fd)) != expected_root:
            raise RuntimeError("cache-root identity changed after scan")
        remove_dir_at(root_fd, quarantine, expected_target)
        try:
            os.stat(quarantine, dir_fd=root_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise RuntimeError("quarantine still exists after fd-relative removal")
        try:
            os.stat(slug, dir_fd=root_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise RuntimeError("original target was reoccupied during deletion")
    finally:
        os.close(root_fd)
except Exception as error:
    print(f"refused: fd-relative deletion failed: {error}")
    raise SystemExit(8)
PY
delete_rc=$?
[ "$delete_rc" -eq 0 ] || exit "$delete_rc"
'''


def remote_reclaim_script(
    target: str, proc_root: str = "/proc", home_root: str | None = None,
    before_delete_script: str | None = None,
) -> str:
    """One remote critical section: revalidate, scan, then delete exactly once."""
    home_value = '"$HOME"' if home_root is None else shlex.quote(home_root)
    return ("home_root=" + home_value + r'''
cache_root="$home_root/.remote-builds"
target=''' + shlex.quote(target) + "\nproc_root="
            + shlex.quote(proc_root) + r'''
uid=$(id -u) || exit 6
slug=${target##*/}
''' + remote_reclaim_path_guard_script() + r'''cache_root_identity=$(stat -c '%d:%i' -- "$cache_root") || {
    printf 'refused: cannot pin cache-root identity: %s\n' "$cache_root"
    exit 8
}
target_identity=$(stat -c '%d:%i' -- "$target") || {
    printf 'refused: cannot pin target identity: %s\n' "$target"
    exit 8
}
busy=0
unknown=0
for proc in "$proc_root"/[0-9]*; do
    owner=$(stat -c %u -- "$proc" 2>/dev/null)
    if [ $? -ne 0 ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s owner\n' "$proc"; unknown=1; }
        continue
    fi
    [ "$owner" = "$uid" ] || continue

    if [ -L "$proc/cwd" ]; then
        resolved=$(readlink -f -- "$proc/cwd" 2>/dev/null)
        if [ $? -ne 0 ]; then
            [ -e "$proc" ] && { printf 'scan-unknown: %s/cwd\n' "$proc"; unknown=1; }
        else
            case "$resolved" in "$target"|"$target"/*)
                printf 'in-use: %s/cwd -> %s\n' "$proc" "$resolved"; busy=1 ;;
            esac
        fi
    fi

    if [ ! -r "$proc/fd" ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s/fd\n' "$proc"; unknown=1; }
    else
        for link in "$proc"/fd/*; do
            [ -e "$link" ] || [ -L "$link" ] || continue
            resolved=$(readlink -f -- "$link" 2>/dev/null)
            if [ $? -ne 0 ]; then
                [ -e "$proc" ] && {
                    printf 'scan-unknown: %s\n' "$link"; unknown=1;
                }
                continue
            fi
            case "$resolved" in "$target"|"$target"/*)
                printf 'in-use: %s -> %s\n' "$link" "$resolved"; busy=1 ;;
            esac
        done
    fi

    grep -Fq -- "$target/" "$proc/maps" 2>/dev/null
    grep_rc=$?
    if [ "$grep_rc" -eq 0 ]; then
        printf 'in-use: %s/maps\n' "$proc"; busy=1
    elif [ "$grep_rc" -ne 1 ] && [ -e "$proc" ]; then
        printf 'scan-unknown: %s/maps\n' "$proc"; unknown=1
    fi

    cmdline=$(tr '\0' ' ' < "$proc/cmdline" 2>/dev/null)
    if [ $? -ne 0 ]; then
        [ -e "$proc" ] && { printf 'scan-unknown: %s/cmdline\n' "$proc"; unknown=1; }
    else
        case "$cmdline" in *"$target"*)
            printf 'in-use: %s/cmdline: %s\n' "$proc" "$cmdline"; busy=1 ;;
        esac
    fi
done
[ "$unknown" -eq 0 ] || { printf 'refused: process scan was incomplete\n'; exit 5; }
[ "$busy" -eq 0 ] || { printf 'refused: target is in use\n'; exit 4; }
''' + fd_relative_reclaim_script(before_delete_script) + r'''
printf 'removed: %s\n' "$target"
''')


def reclaim_decision(execute: bool, busy: bool | None) -> str:
    if not execute:
        return "dry-run"
    if busy is not False:
        return "refuse-busy-or-unknown"
    return "delete"


@contextmanager
def reclaim_path_lease(former_root: Path):
    """Atomically occupy the absent former path across remote deletion."""
    parent = former_root.parent
    if not parent.is_dir():
        die(
            f"cannot safely lease former worktree path {former_root}: parent "
            f"directory {parent} does not exist"
        )
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(former_root, flags, 0o600)
    except FileExistsError:
        die(
            f"remote reclaim refused: {former_root} appeared before its "
            "deletion lease could be acquired"
        )
    except OSError as error:
        die(f"cannot lease former worktree path {former_root}: {error}")
    opened = os.fstat(fd)
    try:
        os.write(fd, b"remote-build reclaim lease\n")
        os.fsync(fd)
        yield
    finally:
        try:
            current = former_root.lstat()
        except FileNotFoundError:
            current = None
        if current is not None and (
            stat.S_ISREG(current.st_mode)
            and (current.st_dev, current.st_ino) == (opened.st_dev, opened.st_ino)
        ):
            former_root.unlink()
        elif current is not None:
            print(
                f"WARNING: reclaim lease path was replaced; preserving the "
                f"replacement at {former_root}", file=sys.stderr,
            )
        os.close(fd)


def cmd_reclaim(
    host: str, root: Path, former_root_arg: str | None,
    requested_slug: str | None, execute: bool, *, worktree_paths_fn=None,
    lock_context_fn=None, lease_context_fn=None, resolve_fn=None, run_fn=None,
) -> int:
    worktree_paths_fn = worktree_paths_fn or git_worktree_paths
    resolve_fn = resolve_fn or resolve_remote_reclaim_path
    run_fn = run_fn or run_ssh
    lease_context_fn = lease_context_fn or reclaim_path_lease

    if lock_context_fn is None:
        @contextmanager
        def lock_context_fn(lock_host: str, lock_slug: str):
            with RemoteLock(lock_slug), RemoteHostLock(lock_host, lock_slug):
                yield

    if not former_root_arg or not requested_slug:
        die("reclaim requires BOTH --reclaim-root /absolute/former/worktree "
            "and --remote-slug <name-hash>")
    former_root = Path(former_root_arg)
    problems = reclaim_request_problems(
        former_root, requested_slug, worktree_paths_fn(root)
    )
    if problems:
        die("remote reclaim refused:\n  - " + "\n  - ".join(problems))
    with lock_context_fn(host, requested_slug):
        if reclaim_decision(execute, None) == "dry-run":
            target, detail = resolve_fn(host, requested_slug)
            if target is None:
                die(
                    f"remote reclaim target is absent or unsafe on {host}:\n"
                    f"{detail}"
                )
            print(f"remote-build reclaim target: {host}:{target}")
            print("DRY-RUN: no files removed. Re-run with --execute-reclaim after "
                  "the coordinator confirms this exact target.")
            return 0
        # The former path may be recreated or registered after the preview.
        # Re-read local ownership under the shared locks before any execute,
        # then atomically occupy that exact pathname until deletion finishes.
        locked_problems = reclaim_request_problems(
            former_root, requested_slug, worktree_paths_fn(root)
        )
        if locked_problems:
            die(
                "remote reclaim refused after locking:\n  - "
                + "\n  - ".join(locked_problems)
            )
        with lease_context_fn(former_root):
            target, detail = resolve_fn(host, requested_slug)
            if target is None:
                die(
                    f"remote reclaim target is absent or unsafe on {host}:\n"
                    f"{detail}"
                )
            print(f"remote-build reclaim target: {host}:{target}")
            print(f"remote-build: removing confirmed orphan {host}:{target}")
            # The lock is the same one held by normal build/sync/shell/clean
            # paths, and the scan plus deletion are one SSH script under that
            # lock. No cooperating build can enter between final scan and rm.
            return run_fn(host, remote_reclaim_script(target))


def doctor(host: str, root: Path) -> int:
    """Check the remote can actually do the job, and say what is missing."""
    slug = slug_for(root)
    print(
        f"remote-build doctor\n"
        f"  host      {host}\n"
        f"  local     {root}\n"
        f"  slug      {slug}\n"
        f"  remote    ~/{REMOTE_ROOT}/{slug}/src\n"
        f"  target    ~/{REMOTE_ROOT}/{slug}/target"
    )

    rc, out = capture_ssh(host, "echo ok")
    if rc != 0:
        print(f"\n  FAIL  ssh unreachable: {out}")
        return 1
    print("\n  ok    ssh")

    checks = {
        "rsync": "command -v rsync",
        "flock": "command -v flock",
        "realpath": "command -v realpath",
        "cargo": "command -v cargo",
        "python3": "command -v python3",
        "just": "command -v just",
    }
    failed = []
    for name, probe in checks.items():
        rc, _ = capture_ssh(host, probe)
        if rc == 0:
            print(f"  ok    {name}")
        else:
            print(f"  FAIL  {name} not found on {host}")
            failed.append(name)

    rc, remote_rustc = capture_ssh(host, "rustc --version")
    local_rustc = subprocess.run(
        ["rustc", "--version"], capture_output=True, text=True
    ).stdout.strip()
    print(f"  --    rustc local  {local_rustc}")
    print(f"  --    rustc remote {remote_rustc}")
    if rc == 0 and remote_rustc != local_rustc:
        # Not fatal, but say it plainly: a version skew can change which
        # warnings appear, which matters for the clippy baseline (B083).
        print("  WARN  rustc versions differ. Builds are fine, but a clippy")
        print("        baseline recorded on one will not match the other.")

    free = remote_free_bytes(host, f"{REMOTE_ROOT}/{slug}")
    verdict = disk_floor_verdict(free, DEFAULT_DISK_FLOOR_GB, host, f"~/{REMOTE_ROOT}")
    if free is None:
        print("  WARN  free space could not be read (B106 pre-flight will refuse)")
    elif verdict:
        print(f"  FAIL  disk {free / 1024 ** 3:.1f}GB free, under the "
              f"{DEFAULT_DISK_FLOOR_GB:g}GB floor — builds here will hit ENOSPC")
        failed.append("disk space")
    else:
        print(f"  ok    disk {free / 1024 ** 3:.1f}GB free")

    rc, cores = capture_ssh(host, "nproc")
    rc2, mem = capture_ssh(host, "free -g | awk '/^Mem:/{print $2}'")
    if rc == 0:
        print(f"  --    remote cores {cores}, ram {mem}GB")

    if failed:
        print(f"\n  {len(failed)} problem(s): {', '.join(failed)}")
        if "cargo" in failed:
            print("  Install a toolchain there, or use --host x-alpha.")
        return 1
    print("\n  remote is usable")
    return 0


def run_selftest() -> int:
    """Prove this script's own guards can fail, and are not passing by vacuity.

    Everything here is pure: no ssh, no rsync, no remote. The guards that
    matter in this file are all decisions made from strings and numbers before
    anything is executed, which is exactly the part that has been wrong three
    times (twice on `$HOME`, once on flag order).
    """
    problems: list[str] = []
    gib = 1024 ** 3

    # The $HOME rule, in both directions. rsync passes its remote path
    # literally; ssh runs a shell. Getting this backwards produced
    # `/home/user/$HOME/...` twice, once per direction.
    paths = remote_paths("demo-0000abcd")
    for key, value in sorted(paths.items()):
        if key.startswith("rsync_") and "$HOME" in value:
            problems.append(
                f"{key} = {value!r} contains $HOME — rsync does not run a "
                f"shell, so it would try to chdir into /home/user/$HOME/..."
            )
    # Positive control: the ssh spellings MUST have it, or the assertion above
    # is satisfied by a function that simply never emits $HOME anywhere.
    for key in ("ssh_src", "ssh_target"):
        if "$HOME" not in paths[key]:
            problems.append(
                f"{key} has no $HOME — the rsync assertion above would then "
                f"pass for a function that cannot produce the bug at all"
            )
    if paths["ssh_target"].endswith(paths["rsync_target"]) is False:
        problems.append("the two spellings no longer name the same directory")

    # B106 disk floor: three states, three renderings.
    if disk_floor_verdict(40 * gib, 10, "h", "/p") is not None:
        problems.append("40 GiB free was refused against a 10 GiB floor")
    low = disk_floor_verdict(int(1.5 * gib), 10, "x-alpha", "/p")
    if low is None:
        problems.append("1.5 GiB free passed a 10 GiB floor")
    elif "x-alpha" not in low or "LSP_REMOTE_HOST" not in low:
        problems.append(
            "the low-disk message does not name the host or an escape route — "
            "a refusal with no way forward reads as a broken tool"
        )
    if disk_floor_verdict(None, 10, "h", "/p") is None:
        problems.append(
            "an unreadable `df` rendered as room — 'could not measure' and "
            "'there is space' must not be the same silence"
        )
    for host in KNOWN_HOSTS:
        message = disk_floor_verdict(1 * gib, 10, host, "/p")
        assert message is not None
        suggested = _other_host(host)
        if suggested == host or f"LSP_REMOTE_HOST={host} " in message:
            problems.append(
                f"the low-disk message for {host} suggests moving to {host} — "
                f"it points the caller at the box that just failed"
            )
    if _other_host("not-a-known-box") not in KNOWN_HOSTS:
        problems.append("an unknown host got no alternative suggested")
    exact = disk_floor_verdict(10 * gib, 10, "h", "/p")
    if exact is not None:
        problems.append("exactly the floor was refused; the floor is a minimum")

    # B167: the main build boundary must measure disk before RemoteHostLock's
    # first-use mkdir, then measure again under the lock before tree mutation.
    # This composes the outer lock boundary with preflight_remote_tree instead
    # of proving either helper in isolation.
    class FakeRemoteHostLock:
        def __init__(self, _host: str, _slug: str) -> None:
            pass

        def __enter__(self):
            events.append("lock")
            return self

        def __exit__(self, *_exc) -> None:
            events.append("unlock")

    events: list[str] = []
    with preflighted_remote_host_lock(
        "h", "slug", lambda: events.append("disk-before-lock"),
        lock_factory=FakeRemoteHostLock,
    ):
        preflight_remote_tree(
            host="h", rel_base="r", remote_src="s", remote_target="t",
            rsync_dest="d", root=Path("/tmp/root"), floor_gb=10,
            allow_low_disk=False, no_sync=False, with_git=False, verbose=False,
            free_bytes_fn=lambda _h, _r: events.append("disk-inside-lock") or 20 * gib,
            mkdir_fn=lambda _h, _s: events.append("mkdir") or (0, ""),
            sync_fn=lambda *_a: events.append("sync"),
        )
        events.append("build")
    expected_events = [
        "disk-before-lock", "lock", "disk-inside-lock", "mkdir", "sync",
        "build", "unlock",
    ]
    if events != expected_events:
        problems.append(
            f"main remote mutation order was {events}, expected {expected_events}"
        )

    events.clear()
    try:
        with preflighted_remote_host_lock(
            "h", "slug",
            lambda: events.append("disk-before-lock") or die("low disk"),
            lock_factory=FakeRemoteHostLock,
        ):
            events.append("body")
    except SystemExit:
        pass
    if events != ["disk-before-lock"]:
        problems.append(
            f"low disk reached the host-lock mkdir boundary: {events}"
        )

    # B167: prove the operational ordering, not merely the prose above main.
    # A low-disk refusal must happen before mkdir or rsync, and the positive
    # control must reach them in that order.
    events = []
    try:
        preflight_remote_tree(
            host="h", rel_base=".remote-builds/s", remote_src="$HOME/x/src",
            remote_target="$HOME/x/target", rsync_dest=".remote-builds/s/src",
            root=Path("/tmp/root"), floor_gb=10, allow_low_disk=False,
            no_sync=False, with_git=False, verbose=False,
            free_bytes_fn=lambda _h, _r: events.append("disk") or gib,
            mkdir_fn=lambda _h, _s: events.append("mkdir") or (0, ""),
            sync_fn=lambda *_a: events.append("sync"),
        )
    except SystemExit:
        pass
    if events != ["disk"]:
        problems.append(
            f"low-disk preflight order was {events}, expected ['disk']; mkdir "
            f"or sync ran before the refusal"
        )
    events.clear()
    preflight_remote_tree(
        host="h", rel_base=".remote-builds/s", remote_src="$HOME/x/src",
        remote_target="$HOME/x/target", rsync_dest=".remote-builds/s/src",
        root=Path("/tmp/root"), floor_gb=10, allow_low_disk=False,
        no_sync=False, with_git=False, verbose=False,
        free_bytes_fn=lambda _h, _r: events.append("disk") or 20 * gib,
        mkdir_fn=lambda _h, _s: events.append("mkdir") or (0, ""),
        sync_fn=lambda *_a: events.append("sync"),
    )
    events.append("build")
    if events != ["disk", "mkdir", "sync", "build"]:
        problems.append(
            f"healthy operation order was {events}, expected disk, mkdir, "
            f"sync, build"
        )
    events.clear()
    preflight_remote_tree(
        host="h", rel_base=".remote-builds/s", remote_src="$HOME/x/src",
        remote_target="$HOME/x/target", rsync_dest=".remote-builds/s/src",
        root=Path("/tmp/root"), floor_gb=10, allow_low_disk=True,
        no_sync=False, with_git=False, verbose=False,
        free_bytes_fn=lambda _h, _r: events.append("disk") or gib,
        mkdir_fn=lambda _h, _s: events.append("mkdir") or (0, ""),
        sync_fn=lambda *_a: events.append("sync"),
    )
    if events != ["mkdir", "sync"]:
        problems.append(
            f"--allow-low-disk did not bypass only the floor: {events}"
        )
    events.clear()
    preflight_remote_tree(
        host="h", rel_base=".remote-builds/s", remote_src="$HOME/x/src",
        remote_target="$HOME/x/target", rsync_dest=".remote-builds/s/src",
        root=Path("/tmp/root"), floor_gb=10, allow_low_disk=False,
        no_sync=True, with_git=False, verbose=False,
        free_bytes_fn=lambda _h, _r: events.append("disk") or 20 * gib,
        mkdir_fn=lambda _h, _s: events.append("mkdir") or (0, ""),
        sync_fn=lambda *_a: events.append("sync"),
    )
    if events != ["disk"]:
        problems.append(f"--no-sync still mutated the remote tree: {events}")

    # B167: reclaim is keyed by BOTH the former local root and the remote slug.
    former = Path("/repo/.worktrees/pool")
    former_slug = slug_for(former)
    if reclaim_request_problems(former, former_slug, set()):
        problems.append("a confirmed absent worktree reclaim was refused")
    mismatched = reclaim_request_problems(former, "other-00000000", set())
    if not mismatched or former_slug not in mismatched[0]:
        problems.append("a mismatched reclaim slug was accepted or not named")
    if not reclaim_request_problems(former, former_slug, {former}):
        problems.append("a still-registered live worktree was reclaimable")
    alternate_former = former.parent / "other" / ".." / former.name
    if slug_for(alternate_former) != former_slug:
        problems.append("alternate former-root spelling changed its remote slug")
    if not reclaim_request_problems(alternate_former, former_slug, {former}):
        problems.append(
            "an alternate spelling made a registered missing worktree reclaimable"
        )
    if not reclaim_request_problems(former, former_slug, {alternate_former}):
        problems.append(
            "an alternate registered-path spelling made a worktree reclaimable"
        )
    registration_tmp = Path(tempfile.mkdtemp(prefix="b167-registered-race-"))
    registered_former = registration_tmp / "gone"
    registered_slug = slug_for(registered_former)
    ownership_states = iter([set(), {registered_former}])
    executed_reclaims = []
    try:
        try:
            cmd_reclaim(
                "h", Path("/repo"), str(registered_former), registered_slug,
                True,
                worktree_paths_fn=lambda _root: next(ownership_states),
                lock_context_fn=lambda _host, _slug: nullcontext(),
                resolve_fn=lambda _host, _slug: (
                    f"/home/u/{REMOTE_ROOT}/{registered_slug}", "",
                ),
                run_fn=lambda *_args: executed_reclaims.append(True) or 0,
            )
            problems.append(
                "reclaim executed after the missing former path became registered"
            )
        except SystemExit:
            pass
    finally:
        shutil.rmtree(registration_tmp, ignore_errors=True)
    if executed_reclaims:
        problems.append("locked ownership revalidation ran after remote deletion")
    recreated_tmp = Path(tempfile.mkdtemp(prefix="b167-recreated-worktree-"))
    recreated_former = recreated_tmp / "gone"
    recreated_slug = slug_for(recreated_former)

    @contextmanager
    def recreate_during_lock(_host: str, _slug: str):
        recreated_former.mkdir()
        yield

    try:
        try:
            cmd_reclaim(
                "h", Path("/repo"), str(recreated_former), recreated_slug, True,
                worktree_paths_fn=lambda _root: set(),
                lock_context_fn=recreate_during_lock,
                resolve_fn=lambda _host, _slug: (
                    f"/home/u/{REMOTE_ROOT}/{recreated_slug}", "",
                ),
                run_fn=lambda *_args: executed_reclaims.append(True) or 0,
            )
            problems.append(
                "reclaim executed after the missing former path was recreated"
            )
        except SystemExit:
            pass
    finally:
        shutil.rmtree(recreated_tmp, ignore_errors=True)
    if executed_reclaims:
        problems.append("recreated local worktree reached remote deletion")

    postcheck_tmp = Path(tempfile.mkdtemp(prefix="b167-postcheck-race-"))
    postcheck_former = postcheck_tmp / "gone"
    postcheck_slug = slug_for(postcheck_former)
    recreate_succeeded = []
    deletion_lease_shapes = []

    def recreate_after_revalidation(_host: str, _slug: str):
        try:
            postcheck_former.mkdir()
            recreate_succeeded.append(True)
        except FileExistsError:
            recreate_succeeded.append(False)
        return f"/home/u/{REMOTE_ROOT}/{postcheck_slug}", ""

    try:
        rc = cmd_reclaim(
            "h", Path("/repo"), str(postcheck_former), postcheck_slug, True,
            worktree_paths_fn=lambda _root: set(),
            lock_context_fn=lambda _host, _slug: nullcontext(),
            resolve_fn=recreate_after_revalidation,
            run_fn=lambda *_args: deletion_lease_shapes.append(
                (postcheck_former.is_file(), postcheck_former.is_dir())
            ) or 0,
        )
        if rc != 0:
            problems.append(f"leased reclaim positive control returned {rc}")
    finally:
        shutil.rmtree(postcheck_tmp, ignore_errors=True)
    if recreate_succeeded != [False]:
        problems.append(
            "former worktree path could be recreated after locked revalidation"
        )
    if deletion_lease_shapes != [(True, False)]:
        problems.append(
            f"remote deletion did not hold an exact-path file lease: "
            f"{deletion_lease_shapes}"
        )
    occupied_tmp = Path(tempfile.mkdtemp(prefix="b167-occupied-lease-"))
    occupied = occupied_tmp / "occupied"
    occupied.write_text("owner data")
    entered_occupied_lease = []
    try:
        try:
            with reclaim_path_lease(occupied):
                entered_occupied_lease.append(True)
        except SystemExit:
            pass
        if entered_occupied_lease or occupied.read_text() != "owner data":
            problems.append(
                "reclaim lease opened or modified an occupied pathname"
            )
    finally:
        shutil.rmtree(occupied_tmp, ignore_errors=True)
    if not reclaim_request_problems(Path("relative/pool"), former_slug, set()):
        problems.append("a relative former-worktree path was reclaimable")
    existing = Path(tempfile.mkdtemp(prefix="b167-existing-worktree-"))
    try:
        existing_slug = slug_for(existing)
        if not reclaim_request_problems(existing, existing_slug, set()):
            problems.append("an extant unregistered path was reclaimable")
    finally:
        shutil.rmtree(existing, ignore_errors=True)
    exact_remote = f"/home/u/{REMOTE_ROOT}/{former_slug}"
    if validated_remote_reclaim_path(exact_remote, former_slug) != exact_remote:
        problems.append("the exact remote reclaim path was refused")
    for unsafe in (f"/home/u/other/{former_slug}",
                   f"relative/{REMOTE_ROOT}/{former_slug}",
                   f"/home/u/{REMOTE_ROOT}/other-00000000"):
        if validated_remote_reclaim_path(unsafe, former_slug) is not None:
            problems.append(f"unsafe remote reclaim path accepted: {unsafe}")
    if reclaim_decision(False, False) != "dry-run":
        problems.append("reclaim no longer defaults to dry-run")
    if reclaim_decision(True, True) == "delete":
        problems.append("an in-use remote tree was approved for deletion")
    if reclaim_decision(True, None) == "delete":
        problems.append("an unknown process-usage result was approved for deletion")
    if reclaim_decision(True, False) != "delete":
        problems.append("a confirmed unused execute request cannot proceed")

    # One script performs the final scan and delete. The live-cwd case must
    # refuse and preserve the tree; after that process exits, the same script
    # is the positive control and removes exactly the target.
    reclaim_tmp = Path(tempfile.mkdtemp(prefix="b167-reclaim-race-"))
    reclaim_target = reclaim_tmp / REMOTE_ROOT / "orphan-12345678"
    reclaim_target.mkdir(parents=True)
    fake_proc = reclaim_tmp / "fake-proc"
    fake_pid = fake_proc / "123"
    (fake_pid / "fd").mkdir(parents=True)
    fake_cwd = fake_pid / "cwd"
    fake_cwd.symlink_to(reclaim_target)
    fake_maps = fake_pid / "maps"
    fake_maps.write_text("")
    (fake_pid / "cmdline").write_bytes(b"fake\0")

    # A lexical $HOME/.remote-builds/slug path is not enough: if the cache
    # root is a symlink, rm would follow its parent and delete outside the
    # managed cache. The final critical section must independently refuse it.
    symlink_home = reclaim_tmp / "symlink-home"
    symlink_home.mkdir()
    outside_cache = reclaim_tmp / "outside-cache"
    escaped_target = outside_cache / "orphan-12345678"
    escaped_target.mkdir(parents=True)
    (symlink_home / REMOTE_ROOT).symlink_to(outside_cache, target_is_directory=True)
    probe_env = dict(os.environ, HOME=str(symlink_home))
    preview_symlink = subprocess.run(
        ["bash"], input=remote_reclaim_probe_script("orphan-12345678"),
        env=probe_env, text=True, capture_output=True, timeout=15,
    )
    if preview_symlink.returncode == 0:
        problems.append("reclaim preview accepted a symlinked cache root")
    safe_preview = subprocess.run(
        ["bash"], input=remote_reclaim_probe_script("orphan-12345678"),
        env=dict(os.environ, HOME=str(reclaim_tmp)), text=True,
        capture_output=True, timeout=15,
    )
    if safe_preview.returncode != 0 or RECLAIM_PATH_MARK not in safe_preview.stdout:
        problems.append(
            f"reclaim preview refused the exact safe cache target: "
            f"rc={safe_preview.returncode}, out={safe_preview.stdout!r}"
        )
    symlinked_parent = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(symlink_home / REMOTE_ROOT / "orphan-12345678"),
            str(fake_proc),
            str(symlink_home),
        ),
        text=True, capture_output=True, timeout=15,
    )
    if symlinked_parent.returncode == 0 or not escaped_target.exists():
        problems.append("reclaim followed a symlinked cache root outside its tree")
    if "unsafe cache root" not in symlinked_parent.stdout:
        problems.append(
            "symlinked cache-root refusal gave no safety evidence: "
            f"{symlinked_parent.stdout!r}"
        )

    blocked = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(reclaim_target), str(fake_proc), str(reclaim_tmp),
        ),
        text=True, capture_output=True, timeout=15,
    )
    if blocked.returncode == 0 or not reclaim_target.exists():
        problems.append(
            "atomic reclaim deleted a target while a process cwd used it"
        )
    if "in-use" not in blocked.stdout:
        problems.append(
            f"busy reclaim did not name its process evidence: {blocked.stdout!r}"
        )
    fake_cwd.unlink()
    fake_cwd.symlink_to(reclaim_tmp)
    fake_maps.chmod(0)
    unknown_scan = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(reclaim_target), str(fake_proc), str(reclaim_tmp),
        ),
        text=True, capture_output=True, timeout=15,
    )
    fake_maps.chmod(0o600)
    if unknown_scan.returncode == 0 or not reclaim_target.exists():
        problems.append("an incomplete process scan was treated as unused")
    if "scan-unknown" not in unknown_scan.stdout:
        problems.append(
            f"incomplete process scan gave no evidence: {unknown_scan.stdout!r}"
        )

    # An individual fd entry can disappear or become unreadable after the
    # directory itself was readable. That race is unknown, not evidence that
    # no process uses the target. A dangling fake link deterministically makes
    # readlink -f fail while the same-owner process directory still exists.
    unreadable_fd = fake_pid / "fd" / "9"
    unreadable_fd.symlink_to(
        reclaim_tmp / "missing-parent" / "missing-fd-target"
    )
    fd_unknown = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(reclaim_target), str(fake_proc), str(reclaim_tmp),
        ),
        text=True, capture_output=True, timeout=15,
    )
    if fd_unknown.returncode == 0 or not reclaim_target.exists():
        problems.append("a failed same-owner fd read was treated as unused")
    if str(unreadable_fd) not in fd_unknown.stdout or "scan-unknown" not in fd_unknown.stdout:
        problems.append(
            f"failed fd read gave no scan-unknown evidence: {fd_unknown.stdout!r}"
        )
    unreadable_fd.unlink()

    # A same-owner process can enter the tree after the first /proc scan even
    # though cooperating remote-build commands share the slug lock. The
    # deletion must close that non-cooperating race before removing anything.
    late_home = reclaim_tmp / "late-user-home"
    late_target = late_home / REMOTE_ROOT / "orphan-12345678"
    late_target.mkdir(parents=True)
    late_sentinel = late_target / "late.txt"
    late_sentinel.write_text("late user")
    late_user_hook = (
        f"rm -- {shlex.quote(str(fake_cwd))} && "
        f"ln -s -- {shlex.quote(str(late_target))} "
        f"{shlex.quote(str(fake_cwd))}"
    )
    late_user = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(late_target), str(fake_proc), str(late_home), late_user_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if late_user.returncode == 0 or not late_sentinel.exists():
        problems.append(
            "reclaim deleted a target acquired after the first process scan"
        )
    if "in-use" not in late_user.stdout:
        problems.append(
            "late process acquisition gave no in-use evidence: "
            f"{late_user.stdout!r}"
        )
    fake_cwd.unlink()
    fake_cwd.symlink_to(reclaim_tmp)

    # Generalize the same seam to an unreadable late fd. Absence is only safe
    # when the post-quarantine scan is complete as well as when it is idle.
    late_unknown_home = reclaim_tmp / "late-unknown-home"
    late_unknown_target = (
        late_unknown_home / REMOTE_ROOT / "orphan-12345678"
    )
    late_unknown_target.mkdir(parents=True)
    late_unknown_sentinel = late_unknown_target / "late-unknown.txt"
    late_unknown_sentinel.write_text("late unknown")
    late_unknown_link = fake_pid / "fd" / "10"
    late_unknown_hook = (
        f"ln -s -- {shlex.quote(str(reclaim_tmp / 'missing-late-parent' / 'fd'))} "
        f"{shlex.quote(str(late_unknown_link))}"
    )
    late_unknown = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(late_unknown_target), str(fake_proc),
            str(late_unknown_home), late_unknown_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if late_unknown.returncode == 0 or not late_unknown_sentinel.exists():
        problems.append(
            "reclaim deleted after its post-quarantine scan became incomplete"
        )
    if "scan-unknown" not in late_unknown.stdout:
        problems.append(
            "late incomplete scan gave no scan-unknown evidence: "
            f"{late_unknown.stdout!r}"
        )
    late_unknown_link.unlink()

    # Swap the validated parent after the /proc scan. String-path rm follows
    # the new symlink; FD-relative deletion must refuse without touching it.
    swap_home = reclaim_tmp / "swap-home"
    swap_target = swap_home / REMOTE_ROOT / "orphan-12345678"
    swap_target.mkdir(parents=True)
    (swap_target / "inside.txt").write_text("inside")
    outside_root = reclaim_tmp / "swap-outside"
    outside_target = outside_root / "orphan-12345678"
    outside_target.mkdir(parents=True)
    outside_sentinel = outside_target / "outside.txt"
    outside_sentinel.write_text("outside")
    moved_root = swap_home / f"{REMOTE_ROOT}.original"
    swap_hook = (
        f"mv -- {shlex.quote(str(swap_home / REMOTE_ROOT))} "
        f"{shlex.quote(str(moved_root))} && "
        f"ln -s -- {shlex.quote(str(outside_root))} "
        f"{shlex.quote(str(swap_home / REMOTE_ROOT))}"
    )
    parent_swapped = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(swap_target), str(fake_proc), str(swap_home), swap_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if parent_swapped.returncode == 0:
        problems.append("reclaim accepted a cache-root swap after the process scan")
    if not outside_sentinel.exists():
        problems.append("reclaim followed a swapped cache parent into outside data")

    # O_NOFOLLOW is independently significant even with identity checks: a
    # symlink can point back to the same directory/inode that was scanned.
    # Following it would still violate the canonical non-symlink root contract.
    nofollow_home = reclaim_tmp / "nofollow-home"
    nofollow_target = nofollow_home / REMOTE_ROOT / "orphan-12345678"
    nofollow_target.mkdir(parents=True)
    nofollow_sentinel = nofollow_target / "inside.txt"
    nofollow_sentinel.write_text("inside")
    nofollow_original_root = nofollow_home / f"{REMOTE_ROOT}.original"
    same_identity_symlink_hook = (
        f"mv -- {shlex.quote(str(nofollow_home / REMOTE_ROOT))} "
        f"{shlex.quote(str(nofollow_original_root))} && "
        f"ln -s -- {shlex.quote(str(nofollow_original_root))} "
        f"{shlex.quote(str(nofollow_home / REMOTE_ROOT))}"
    )
    same_identity_symlinked = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(nofollow_target), str(fake_proc), str(nofollow_home),
            same_identity_symlink_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if same_identity_symlinked.returncode == 0:
        problems.append("reclaim followed a same-identity cache-root symlink")
    if not nofollow_sentinel.exists():
        problems.append("reclaim deleted through a same-identity root symlink")

    # Replacing the validated cache root with another ordinary directory is
    # just as unsafe as replacing it with a symlink. The deletion must remain
    # bound to the exact directory identity that the process scan covered.
    replaced_home = reclaim_tmp / "replaced-home"
    replaced_target = replaced_home / REMOTE_ROOT / "orphan-12345678"
    replaced_target.mkdir(parents=True)
    replacement_sentinel = replaced_target / "original.txt"
    replacement_sentinel.write_text("original")
    replaced_original_root = replaced_home / f"{REMOTE_ROOT}.original"
    replace_root_hook = (
        f"mv -- {shlex.quote(str(replaced_home / REMOTE_ROOT))} "
        f"{shlex.quote(str(replaced_original_root))} && "
        f"mkdir -- {shlex.quote(str(replaced_home / REMOTE_ROOT))} && "
        f"mv -- {shlex.quote(str(replaced_original_root / replaced_target.name))} "
        f"{shlex.quote(str(replaced_target))}"
    )
    root_replaced = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(replaced_target), str(fake_proc), str(replaced_home),
            replace_root_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if root_replaced.returncode == 0:
        problems.append(
            "reclaim accepted a real-directory cache-root replacement after "
            "the process scan"
        )
    if not replacement_sentinel.exists():
        problems.append("reclaim deleted through a replacement cache root")

    # The leaf directory covered by the process scan is identity-sensitive as
    # well: swapping only the slug must preserve the replacement tree.
    leaf_home = reclaim_tmp / "leaf-swap-home"
    leaf_target = leaf_home / REMOTE_ROOT / "orphan-12345678"
    leaf_target.mkdir(parents=True)
    (leaf_target / "original.txt").write_text("original")
    moved_leaf = leaf_home / REMOTE_ROOT / "orphan-12345678.original"
    leaf_replacement_sentinel = leaf_target / "replacement.txt"
    replace_leaf_hook = (
        f"mv -- {shlex.quote(str(leaf_target))} "
        f"{shlex.quote(str(moved_leaf))} && "
        f"mkdir -- {shlex.quote(str(leaf_target))} && "
        f"printf replacement > {shlex.quote(str(leaf_replacement_sentinel))}"
    )
    leaf_replaced = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(leaf_target), str(fake_proc), str(leaf_home), replace_leaf_hook,
        ),
        text=True, capture_output=True, timeout=15,
    )
    if leaf_replaced.returncode == 0:
        problems.append(
            "reclaim accepted a slug-directory replacement after the process scan"
        )
    if not leaf_replacement_sentinel.exists():
        problems.append("reclaim deleted a replacement slug directory")

    removed = subprocess.run(
        ["bash"],
        input=remote_reclaim_script(
            str(reclaim_target), str(fake_proc), str(reclaim_tmp),
        ),
        text=True, capture_output=True, timeout=15,
    )
    if removed.returncode != 0 or reclaim_target.exists():
        problems.append(
            f"confirmed-unused reclaim did not remove its exact target: "
            f"rc={removed.returncode}, out={removed.stdout!r}, err={removed.stderr!r}"
        )
    shutil.rmtree(reclaim_tmp, ignore_errors=True)
    own_source = Path(__file__).read_text()
    for label, fragment in (
        ("normal build", "with RemoteLock(slug), preflighted_remote_host_lock(\n"
                         "        a.host, slug, disk_check_before_remote_mutation,"),
        ("current-tree clean", "with RemoteLock(slug), RemoteHostLock(a.host, slug):\n"
                               "            return run_ssh"),
        ("orphan reclaim", "with RemoteLock(lock_slug), "
                           "RemoteHostLock(lock_host, lock_slug):"),
    ):
        if fragment not in own_source:
            problems.append(
                f"{label} no longer holds the shared remote slug lock"
            )
    dry_args = build_parser().parse_args([
        "--reclaim-root", str(former), "--remote-slug", former_slug, "reclaim",
    ])
    if dry_args.execute_reclaim:
        problems.append("reclaim deletes by default instead of defaulting dry-run")
    parsed_inventory = parse_remote_inventory(
        "9437184\tpool-d616b701\nnoise\n12\twt-00000000\n"
    )
    if parsed_inventory != [("pool-d616b701", 9437184), ("wt-00000000", 12)]:
        problems.append(f"remote inventory parsing is wrong: {parsed_inventory}")

    # df parsing, including the shapes that must NOT be read as a number.
    df = ("Filesystem     1024-blocks      Used Available Capacity Mounted on\n"
          "/dev/nvme0n1p2   480579748 435118596  20964512      96% /")
    if parse_df_avail(df) != 20964512 * 1024:
        problems.append(f"df parsing is wrong: {parse_df_avail(df)}")
    if parse_df_avail("df: /x: No such file or directory") is not None:
        problems.append("a df error message was parsed as a free-space number")
    if parse_df_avail("") is not None:
        problems.append("empty df output was parsed as a free-space number")

    # Flag-order detection, and the control that it does not reject commands.
    for flag in LOCAL_ONLY_FLAGS:
        if misplaced_flags(["cargo", "build", flag]) != [flag]:
            problems.append(f"a misplaced `{flag}` was not caught")
    if misplaced_flags(["cargo", "test", "--sync-back=debug/x"]) != [
        "--sync-back=debug/x"
    ]:
        problems.append("the `--flag=value` spelling slips past the check")
    legitimate = ["cargo", "build", "-v", "-j4", "--release", "--all-targets",
                  "--", "--test-threads=2", "--nocapture"]
    if misplaced_flags(legitimate):
        problems.append(
            f"a legitimate remote command was rejected as a misplaced flag: "
            f"{misplaced_flags(legitimate)} — the check would block `just rrun "
            f"cargo build -v`, so it cannot be trusted to mean anything"
        )

    # A set-but-empty $LSP_REMOTE_HOST must not become the host. Checked on the
    # helper that consumes it, so the assertion is about a decision rather than
    # about an environment this process happens to have.
    if _other_host("") == "":
        problems.append("an empty host was echoed back as an alternative")

    # Signal deaths. A negative returncode taken mod 256 is nonzero by luck,
    # not by design, and names nothing.
    if exit_status(0) != (0, None) or exit_status(101) != (101, None):
        problems.append("an ordinary exit status was rewritten")
    killed, note = exit_status(-9)
    if killed != 137 or not note or "signal 9" not in note:
        problems.append(
            f"a session killed by SIGKILL reported {killed} with note {note!r} "
            f"— the shell's own 128+N spelling and the signal number are what "
            f"make it readable as a kill rather than a chosen exit code"
        )
    if exit_status(-15)[0] % 256 == 0:
        problems.append(
            "a signalled session mapped to a status that is ZERO mod 256 — it "
            "would surface as success"
        )

    # Per-worktree isolation. Two lanes sharing a slug share a target dir, and
    # the failures that produces look like real ones.
    # The collision the hash exists for is SAME BASENAME, different parent --
    # every lane worktree under .worktrees/ is a candidate. A first version of
    # this check compared /a/b with /a/c, which differ by basename alone and so
    # stayed distinct even with the hash removed: the mutation survived.
    if slug_for(Path("/one/lane/tooling")) == slug_for(Path("/two/lane/tooling")):
        problems.append(
            "two worktrees with the same basename share a remote slug — they "
            "would share a target dir, and the failures that produces look "
            "exactly like real ones"
        )
    if slug_for(Path("/a/b")) != slug_for(Path("/a/b")):
        problems.append("the slug is not stable for one worktree")
    if "tooling" not in slug_for(Path("/one/lane/tooling")):
        problems.append(
            "the slug dropped the readable part — a directory listing on the "
            "build box would no longer say which lane owns which tree"
        )

    # --- B132: an unresolved operation is not a code defect ---------------
    # The gate rsyncs FILES and cannot see the git index, so a conflicted tree
    # arrives at the compiler as syntax. The verdict was already correct (B083
    # refused a zero-unit run); the DIAGNOSIS said "your code does not
    # compile" when the truth was "another session is mid-merge in your
    # checkout".
    #
    # THE REFUSAL AND THE PROCEED CASES CARRY EQUAL WEIGHT HERE. Gating
    # uncommitted work is the normal lane workflow, so a guard that fired on a
    # dirty tree would break every lane -- a worse regression than the defect.
    # Each case below is therefore a PAIR.
    for op in ("merge", "rebase", "cherry-pick", "revert"):
        msg = unresolved_verdict(op, ["src/a.rs"])
        if not msg:
            problems.append(f"an unresolved {op} was not refused")
            continue
        if op not in msg:
            problems.append(f"the {op} refusal does not name the operation")
        # The whole point of the ticket: say WHICH files.
        if "src/a.rs" not in msg:
            problems.append(
                f"the {op} refusal does not name the unmerged files, which is "
                f"the part that turns the worst diagnostic into the clearest"
            )
        if "ANOTHER SESSION" not in msg:
            problems.append(
                f"the {op} refusal does not mention that the operation may "
                f"belong to another session -- the shared-checkout case is how "
                f"this was found"
            )
    # The proceed half. A guard that refuses everything satisfies every check
    # above and destroys the workflow, so these are not optional.
    if unresolved_verdict(None, []) is not None:
        problems.append("a clean tree was refused")
    if unresolved_verdict(None, ["src/a.rs", "src/b.rs"]) is not None:
        problems.append(
            "a DIRTY tree with no operation in progress was refused — gating "
            "uncommitted work is the normal lane workflow and refusing it "
            "would break every lane"
        )
    # A stopped rebase can have every conflict staged and still be unfinished,
    # so an empty unmerged list must not read as 'nothing is wrong'.
    staged = unresolved_verdict("rebase", [])
    if not staged or "has not been finished" not in staged:
        problems.append(
            "an unresolved operation with NO unmerged files was not refused, "
            "or did not explain why it still matters"
        )

    # `operation_in_progress` against real sentinel names, in a temp gitdir.
    with tempfile.TemporaryDirectory(prefix="b132-") as tmp:
        gd = Path(tmp)
        if operation_in_progress(gd) is not None:
            problems.append("an empty gitdir reported an operation in progress")
        if operation_in_progress(None) is not None:
            problems.append("a missing gitdir reported an operation in progress")
        for name, label in GIT_OPERATION_SENTINELS:
            marker = gd / name
            marker.mkdir() if name.startswith("rebase-") else marker.write_text("x")
            got = operation_in_progress(gd)
            if got != label:
                problems.append(f"sentinel {name} reported {got!r}, want {label!r}")
            shutil.rmtree(marker) if marker.is_dir() else marker.unlink()
        # The loop above iterates GIT_OPERATION_SENTINELS, so it proves each
        # entry WORKS and nothing at all about which entries EXIST -- delete
        # `CHERRY_PICK_HEAD` from the table and that loop still passes, having
        # simply stopped testing it. A test that derives its expectations from
        # the thing under test cannot see an omission. So the required
        # operations are named HERE, independent of the table.
        #
        # B169 RESTATED WHAT "REQUIRED" MEANS, AND KEPT THE INDEPENDENCE. The
        # original wording was "the filename git actually writes", which is
        # true of `REBASE_HEAD` and still made this list wrong, because git
        # writes that file and then LEAVES IT BEHIND once the rebase finishes.
        # The property a sentinel needs is the two-sided one: git writes it
        # during the operation AND REMOVES IT ON RESOLUTION. Requiring the
        # one-sided version made the gate refuse a resolved tree. So all four
        # operations are still required here by hand -- an omission still gets
        # caught -- but each is required through a sentinel that clears.
        # Rebase's is `rebase-merge/` / `rebase-apply/`, asserted in the loop
        # directly below, which is equally hardcoded; `REBASE_HEAD` moves to
        # the exclusion pin further down.
        for filename, want in (
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
        ):
            marker = gd / filename
            marker.write_text("x")
            got = operation_in_progress(gd)
            marker.unlink()
            if got != want:
                problems.append(
                    f"git writes {filename} for an unresolved {want}, but the "
                    f"probe reported {got!r} — that operation would be gated "
                    f"with conflict markers in the tree"
                )
        # This loop is the REBASE half of the hardcoded requirement above, not
        # a bonus check: with `REBASE_HEAD` correctly out of the table, these
        # two directories are the only thing standing between a mid-conflict
        # rebase and the remote. Drop `rebase-merge` from the table and this
        # fails, which is the omission protection working.
        for dirname in ("rebase-merge", "rebase-apply"):
            marker = gd / dirname
            marker.mkdir()
            got = operation_in_progress(gd)
            shutil.rmtree(marker)
            if got != "rebase":
                problems.append(
                    f"git writes {dirname}/ during a rebase AND REMOVES IT on "
                    f"resolution, but the probe reported {got!r} — a rebase "
                    f"stopped on conflicts would reach the remote"
                )

        # Deliberate NON-sentinel: a bisect checks out real commits and cannot
        # produce a conflict marker, so refusing during one would be
        # over-condemning. This pins the exclusion so nobody "completes" the
        # list later without noticing it costs a legitimate workflow.
        (gd / "BISECT_LOG").write_text("x")
        if operation_in_progress(gd) is not None:
            problems.append(
                "a bisect was treated as an unresolved operation — it produces "
                "no conflict markers, so gating during one is legitimate"
            )
        (gd / "BISECT_LOG").unlink()

        # Deliberate NON-sentinel (B169): git writes `REBASE_HEAD` during a
        # rebase and does NOT remove it when the rebase completes, so it is
        # present in a perfectly clean tree — every lane's tree, after every
        # rebase this repo's workflow mandates. Treating it as a sentinel made
        # the gate refuse to run for the one lane that had just resolved a
        # conflict and was re-gating. Measured, not reasoned: after a resolved
        # conflicted rebase the file remains and `git rebase --continue` says
        # `fatal: no rebase in progress`.
        #
        # This pin is asserted through NOT_UNRESOLVED_SENTINELS so the
        # exclusion is declared in the table's own file and checked here; a
        # future "the list is missing REBASE_HEAD" fix has to delete a named,
        # explained pin rather than fill a silent gap.
        for filename in NOT_UNRESOLVED_SENTINELS:
            marker = gd / filename
            marker.write_text("x")
            got = operation_in_progress(gd)
            marker.unlink()
            if got is not None:
                problems.append(
                    f"{filename} was treated as an unresolved operation, but "
                    f"git leaves it behind after the operation SUCCEEDS — this "
                    f"refuses a clean post-rebase tree, which is every lane's"
                )
        if "REBASE_HEAD" not in NOT_UNRESOLVED_SENTINELS:
            problems.append(
                "REBASE_HEAD dropped out of the exclusion pin — the B169 "
                "regression can return silently without it"
            )

    # THE WIRING. Every case above tests the decision; none of them proves
    # `main` ASKS. This runs the real entry point against a genuinely
    # conflicted repository and requires it to refuse BEFORE any ssh — which
    # is the ticket's actual claim: a tree with conflict markers must not
    # reach the remote. Offline: the guard fires before the first ssh, and the
    # host is an RFC 2606 `.invalid` name that cannot resolve either way.
    with tempfile.TemporaryDirectory(prefix="b132-wire-") as tmp:
        repo = Path(tmp) / "r"
        repo.mkdir()

        def git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
            return subprocess.run(["git", "-C", str(repo), *args],
                                  capture_output=True, text=True,
                                  check=False, timeout=30)

        git("init", "-q", "-b", "main")
        git("config", "user.email", "selftest@example.invalid")
        git("config", "user.name", "selftest")
        (repo / "f.rs").write_text("base\n")
        git("add", "f.rs")
        git("commit", "-qm", "base")
        git("checkout", "-qb", "other")
        (repo / "f.rs").write_text("theirs\n")
        git("commit", "-qam", "theirs")
        git("checkout", "-q", "main")
        (repo / "f.rs").write_text("ours\n")
        git("commit", "-qam", "ours")
        git("merge", "other")
        if "<<<<<<<" not in (repo / "f.rs").read_text():
            problems.append(
                "the wiring fixture did not actually conflict, so it proves "
                "nothing about a conflicted tree"
            )
        else:
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "check",
                 "--host", "no-such-host.invalid"],
                cwd=repo, capture_output=True, text=True, timeout=120,
            )
            blob = proc.stdout + proc.stderr
            if "unresolved merge" not in blob:
                problems.append(
                    "main() did not refuse a conflicted tree — the guard is "
                    f"not wired into the entry point (got: {blob.strip()[:200]!r})"
                )
            if "f.rs" not in blob:
                problems.append("the refusal from main() did not name f.rs")
            # It must refuse BEFORE contacting the remote; the banner is
            # printed only once the run is under way.
            if "remote-build: no-such-host.invalid" in blob:
                problems.append(
                    "main() reached the remote phase despite the conflict"
                )
            # THE PROPERTY THAT KEEPS THIS GUARD FROM BLOCKING EVERY LANE.
            # Operation state is PER-WORKTREE: a merge running in the main
            # checkout lives in `.git/`, while a linked worktree's lives in
            # `.git/worktrees/<name>/`. Resolving the gitdir by hand as
            # `root/.git` would attribute the coordinator's merge to every lane
            # and stop all of them at once -- turning a diagnostic fix into an
            # outage. Asking git is what gets this right, and this is what
            # proves it still does.
            wt = Path(tmp) / "wt"
            git("worktree", "add", "-q", "-b", "lane", str(wt))
            if wt.is_dir():
                proc_wt = subprocess.run(
                    [sys.executable, str(Path(__file__).resolve()), "check",
                     "--host", "no-such-host.invalid"],
                    cwd=wt, capture_output=True, text=True, timeout=120,
                )
                if "unresolved merge" in (proc_wt.stdout + proc_wt.stderr):
                    problems.append(
                        "a linked worktree was refused because the MAIN "
                        "checkout is mid-merge — operation state is "
                        "per-worktree, and conflating them would stop every "
                        "lane whenever the coordinator merges"
                    )

            # PAIR: the same repo, same command, with the merge aborted, must
            # get PAST this guard. Without it the wiring check above is
            # satisfied by a main() that refuses unconditionally.
            git("merge", "--abort")
            proc2 = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "check",
                 "--host", "no-such-host.invalid"],
                cwd=repo, capture_output=True, text=True, timeout=120,
            )
            if "unresolved merge" in (proc2.stdout + proc2.stderr):
                problems.append(
                    "main() still refused after the merge was aborted — the "
                    "guard fires on a clean tree"
                )

    # --- B140: --sync-back resolves under the TARGET dir --------------------
    # Its failure mode was `rsync: Broken pipe (32)`, which names the transport
    # and not the mistake. PAIRS: artefact paths must proceed, or this guard
    # would make the flag unusable.
    for good in ("debug/openplanet-lsp", "release/openplanet-lsp",
                 "doc/openplanet_lsp/index.html", None):
        if sync_back_verdict(good) is not None:
            problems.append(
                f"--sync-back {good!r} is a legitimate target-relative path "
                f"and was refused — the guard would make the flag unusable"
            )
    for bad, why in (
        ("src/main.rs", "a source path"),
        ("scripts/x.py", "a source path"),
        ("tests/fixtures/a", "a source path"),
        ("/tmp/evil", "an absolute path, which would escape target/"),
        ("../../etc/passwd", "a `..` traversal"),
        ("", "an empty path"),
    ):
        msg = sync_back_verdict(bad)
        if not msg:
            problems.append(f"--sync-back {bad!r} ({why}) was not refused")
        elif "sync-back" not in msg:
            problems.append(f"the refusal for {bad!r} does not name the flag")
    # The absolute-path case is not cosmetic: `Path('/a/b') / '/etc/x'` is
    # `/etc/x`, so it would relocate the destination out of `target/` entirely.
    if Path("/repo/target") / "/etc/x" != Path("/etc/x"):
        problems.append(
            "pathlib no longer discards the prefix on an absolute join, so the "
            "reason --sync-back refuses absolute paths needs rechecking"
        )
    missing = sync_back_missing_verdict("debug/nope", "x-alpha", "/t/debug/nope")
    for want in ("debug/nope", "x-alpha", "/t/debug/nope"):
        if want not in missing:
            problems.append(
                f"the sync-back not-found message omits {want!r} — the whole "
                f"point is to name the path rsync could not fetch"
            )

    # --- B139: ssh's own failure status must not read as a verdict ----------
    _, note255 = exit_status(255)
    if not note255 or "ssh" not in note255:
        problems.append(
            "exit 255 carries no note — it is ssh's own failure status and a "
            "bare `exit 255` reads as a result from the remote command"
        )
    for ordinary in (0, 1, 101):
        if exit_status(ordinary)[1] is not None:
            problems.append(
                f"exit {ordinary} gained a note it should not have — a note on "
                f"every status is one nobody reads"
            )

    # --- B147: the sync-back existence probe must EXPAND $HOME --------------
    # The measured defect: `test -e '$HOME/...'` returned 1 for an artefact
    # that was demonstrably there, because shlex.quote single-quotes and no
    # shell expands $HOME inside single quotes.
    #
    # These do not assert on the SHAPE of the script -- a substring check
    # would pass on any string that happens to contain `"$HOME"`. They RUN it
    # through a real bash with a known HOME and read back the path it
    # resolved, which is the same oracle the remote uses.
    probe_home = Path(tempfile.mkdtemp(prefix="rb-b147-"))
    try:
        def resolve(subpath: str, make: bool) -> tuple[int, str]:
            script = sync_back_probe_script(".remote-builds/slug", subpath)
            env = dict(os.environ, HOME=str(probe_home))
            if make:
                real = probe_home / ".remote-builds/slug/target" / subpath
                real.parent.mkdir(parents=True, exist_ok=True)
                real.write_text("artefact")
            out = subprocess.run(["bash", "-c", script], capture_output=True,
                                 text=True, env=env, timeout=30)
            return out.returncode, parse_probe_path(
                out.stdout + out.stderr, "<<unparsed>>")

        want = str(probe_home / ".remote-builds/slug/target/debug/openplanet-lsp")
        # PAIR, and it is the whole ticket: the artefact EXISTS, so the probe
        # must say so. This is the half that was failing in production.
        rc_present, path_present = resolve("debug/openplanet-lsp", make=True)
        if rc_present != 0:
            problems.append(
                "the sync-back probe reported an artefact that EXISTS as "
                "missing — this is the shipped B147 defect, $HOME reaching a "
                "single-quoted context"
            )
        if path_present != want:
            problems.append(
                f"the sync-back probe resolved {path_present!r}, not {want!r} "
                f"— the path it reports must be the path it tested"
            )
        if "$HOME" in path_present:
            problems.append(
                "the resolved sync-back path still contains a literal $HOME, "
                "so the operator is shown a path they cannot paste into a "
                "shell and cannot reproduce the failure with"
            )
        # PAIR: a genuinely absent artefact must still be reported absent, or
        # the guard above is satisfied by a probe that always succeeds.
        rc_absent, path_absent = resolve("debug/not-built", make=False)
        if rc_absent == 0:
            problems.append(
                "the sync-back probe reported a MISSING artefact as present — "
                "it would hand rsync a path that is not there and lose the "
                "one thing it exists to say"
            )
        if not path_absent.startswith(str(probe_home)):
            problems.append(
                "the probe did not report a resolved path for a MISSING "
                "artefact — that is exactly the case the not-found message "
                "needs it for"
            )
        # The subpath is operator input reaching a remote shell. It must be a
        # path that does not exist, not a command that runs.
        canary = probe_home / "canary"
        rc_evil, _ = resolve(f"debug/x; touch {canary}", make=False)
        if canary.exists():
            problems.append(
                "a --sync-back subpath containing `;` EXECUTED on the remote "
                "— the subpath must be quoted even though $HOME must not be"
            )
        if rc_evil == 0:
            problems.append(
                "a --sync-back subpath containing shell metacharacters was "
                "reported as an existing artefact"
            )
    finally:
        shutil.rmtree(probe_home, ignore_errors=True)

    # Login shells are chatty. The marker exists so a `.bashrc` that echoes
    # cannot be mistaken for the path.
    noisy = ("Welcome to x-alpha\n"
             f"{PROBE_PATH_MARK}/home/u/.remote-builds/s/target/debug/x\n")
    if parse_probe_path(noisy, "FB") != "/home/u/.remote-builds/s/target/debug/x":
        problems.append("parse_probe_path is defeated by login-shell noise")
    if parse_probe_path("motd only\n", "FB") != "FB":
        problems.append(
            "parse_probe_path invented a path when the probe printed none — "
            "it must fall back to the template rather than report nonsense"
        )

    # --- B147: a failed sync-back must be VISIBLE and non-zero --------------
    # The recorded incident: a sweep ran twice on the same stale binary
    # because the sync between them did not take effect and nothing said so.
    for flag in ("--sync-back-must-change", "--allow-unresolved"):
        if misplaced_flags(["cargo", "test", flag]) != [flag]:
            problems.append(
                f"{flag} placed after the action is not recognised as a "
                f"misplaced flag, so it would be handed to the remote shell "
                f"as `{flag}: command not found`"
            )
    if SYNC_BACK_FAILED == 0 or SYNC_BACK_UNCHANGED == 0:
        problems.append(
            "a sync-back failure status is 0, which is the whole defect: a "
            "caller cannot distinguish it from a successful fetch"
        )
    if SYNC_BACK_FAILED == SYNC_BACK_UNCHANGED:
        problems.append(
            "'not fetched' and 'fetched but unchanged' share an exit status, "
            "so a comparison script cannot branch on them"
        )
    nf = sync_back_not_fetched_verdict("debug/openplanet-lsp",
                                       Path("/r/target/debug/openplanet-lsp"),
                                       101, "a" * 64)
    for want in ("NOT", "debug/openplanet-lsp", "/r/target/debug/openplanet-lsp",
                 "101", "STALE", "PREVIOUS", "a" * 16):
        if want not in nf:
            problems.append(
                f"the sync-back-not-fetched message omits {want!r} — a caller "
                f"left holding the previous artefact has to be told so"
            )
    # PAIR: with no previous artefact there is nothing stale, and saying there
    # is would be the over-condemn half of this guard.
    nf_fresh = sync_back_not_fetched_verdict("debug/x", Path("/r/t/debug/x"),
                                             1, None)
    if "PREVIOUS" in nf_fresh:
        problems.append(
            "the not-fetched message claims a stale previous artefact when "
            "there was no file there at all"
        )

    # --- B147: hashing, which is what makes a before/after comparison real --
    hash_dir = Path(tempfile.mkdtemp(prefix="rb-b147h-"))
    try:
        missing = hash_dir / "nope"
        if file_sha256(missing) is not None:
            problems.append(
                "file_sha256 invented a hash for a file that is not there — "
                "'no previous artefact' must be distinguishable from 'the "
                "previous artefact was identical'"
            )
        a_file = hash_dir / "a"
        a_file.write_bytes(b"one")
        h1 = file_sha256(a_file)
        a_file.write_bytes(b"one")
        if file_sha256(a_file) != h1:
            problems.append("file_sha256 is not stable on identical content")
        a_file.write_bytes(b"two")
        if file_sha256(a_file) == h1:
            problems.append(
                "file_sha256 returned the same hash for different content, so "
                "the unchanged-artefact guard cannot fire"
            )
        # It must read the WHOLE file, not a prefix: two build artefacts of a
        # large binary differ late far more often than early.
        big_a = hash_dir / "big_a"
        big_b = hash_dir / "big_b"
        big_a.write_bytes(b"x" * (4 << 20) + b"A")
        big_b.write_bytes(b"x" * (4 << 20) + b"B")
        if file_sha256(big_a) == file_sha256(big_b):
            problems.append(
                "file_sha256 misses a difference in the last byte of a 4 MiB "
                "file — it is hashing a prefix, and two builds of one binary "
                "differ late"
            )
    finally:
        shutil.rmtree(hash_dir, ignore_errors=True)

    unchanged = sync_back_unchanged_verdict("debug/openplanet-lsp",
                                            Path("/r/target/debug/openplanet-lsp"),
                                            "b" * 64, "x-alpha")
    for want in ("BYTE-IDENTICAL", "debug/openplanet-lsp", "x-alpha",
                 "b" * 64, "IDENTICAL"):
        if want not in unchanged:
            problems.append(
                f"the unchanged-artefact message omits {want!r} — it has to "
                f"name the hash and the comparison it is protecting"
            )

    # --- B147: the flag list must not be able to MISS a flag ----------------
    # Two entries were missing from LOCAL_ONLY_FLAGS at once, for the ordinary
    # reason: it is a hand-maintained second copy of a subset of the parser's
    # own option strings, and a list has no way to notice what is not in it.
    # `--allow-unresolved` had been missing since B132 added it; the author who
    # found that omission then wrote `--sync-back-must-change` and omitted it
    # too, in the same hour, in the same file.
    #
    # So: walk the parser, and require every option to be classified. This does
    # not decide which side a new flag belongs on -- it makes not deciding
    # impossible. Same shape as tonight's `use Border` / `use Borders` prefix
    # collision in a completely different file: a container matched by exact
    # spelling, silently not covering a longer name that starts with a covered
    # one.
    parser_flags = {
        opt
        for action in build_parser()._actions
        for opt in action.option_strings
    }
    problems.extend(flag_classification_problems(
        parser_flags, LOCAL_ONLY_FLAGS, SHARED_SPELLING_FLAGS))

    # POSITIVE CONTROLS. The real classification is correct, so every check
    # above returns the empty list -- and an empty list is what a check that
    # does nothing returns too. Three of these were measured vacuous for
    # exactly that reason before these cases existed: the guard cannot be shown
    # to work by pointing it at a tree that is already right.
    for label, args, want in (
        ("a flag in NEITHER tuple",
         ({"--a", "--new"}, ("--a",), ()), "neither"),
        ("a classification for a flag the parser no longer has",
         ({"--a"}, ("--a", "--renamed-away"), ()), "no longer exists"),
        ("a flag classified BOTH ways",
         ({"--a"}, ("--a",), ("--a",)), "BOTH ways"),
    ):
        got = flag_classification_problems(*args)
        if not got:
            problems.append(
                f"the flag classification check does not report {label} -- it "
                f"returns the empty list for a BROKEN classification, which is "
                f"the same answer it gives for a correct one"
            )
        elif not any(want in g for g in got):
            problems.append(
                f"the report for {label} does not say why ({want!r}): {got}"
            )
    # PAIR: and a correct classification must produce NOTHING, or the guard
    # would refuse every future flag and be turned off within the week.
    if flag_classification_problems({"--a", "--b"}, ("--a",), ("--b",)):
        problems.append(
            "the flag classification check reports a problem for a COMPLETE "
            "and non-overlapping classification"
        )
    # PAIR, and it is the over-condemn half: a flag the remote command
    # plausibly shares must still get THROUGH. Refusing `--jobs` or
    # `--test-threads` would break `run -- cargo test -- --test-threads=4`,
    # which is an ordinary invocation of this tool.
    for shared in SHARED_SPELLING_FLAGS:
        if misplaced_flags(["cargo", "test", "--", shared]):
            problems.append(
                f"{shared} is refused after the action, but the remote command "
                f"spells it the same way -- this would break a normal run"
            )
    # And every local-only flag must actually BE refused, in both spellings.
    for local in LOCAL_ONLY_FLAGS:
        if misplaced_flags(["cargo", "test", local]) != [local]:
            problems.append(f"{local} is not refused after the action")
        valued = f"{local}=x"
        if misplaced_flags(["cargo", "test", valued]) != [valued]:
            problems.append(f"{valued} is not refused after the action")

    if problems:
        print("FAIL: remote-build self-test", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print("PASS: remote-build guards — $HOME spelling, provenance, disk-first\n"
          "preflight, safe reclaim, df parsing, flag order, slug isolation and\n"
          "the unresolved-operation probe reject bad cases and accept controls.")
    return 0


def build_parser() -> argparse.ArgumentParser:
    """The CLI, built where a guard can enumerate it.

    B147/B159. `LOCAL_ONLY_FLAGS` is a hand-maintained SECOND COPY of a subset
    of these option strings, and a second place to remember is a place that
    gets forgotten: `--allow-unresolved` was missing from it from the day B132
    added it, and `--sync-back-must-change` was missing from the day it was
    written, by the same author, in the same hour as the fix for the first one.
    Neither is a typo. The list has no way to notice a flag that is not in it.

    So the parser is built here rather than inline in `main`, and the selftest
    walks its actions and requires EVERY option to be classified -- local-only,
    or deliberately shared. A new flag that is neither fails the selftest by
    name. That does not make the right answer obvious; it makes the QUESTION
    unavoidable, which is the part that was missing.
    """
    ap = argparse.ArgumentParser(
        prog="remote-build.py",
        description="Run builds and tests on a remote box instead of this one.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument(
        "action",
        choices=["test", "check", "clippy", "build", "run", "doctor", "shell",
                 "clean", "inventory", "reclaim", "selftest"],
        help="test/check/clippy/build run the obvious thing; "
        "run takes an arbitrary command after --; "
        "doctor verifies the remote; shell opens one; clean removes the current "
        "tree; inventory maps remote trees; reclaim safely targets a removed "
        "worktree; selftest proves this script's own guards can fail",
    )
    ap.add_argument("rest", nargs=argparse.REMAINDER,
                    help="extra args, after --, passed to the remote command")
    ap.add_argument("--host", default=DEFAULT_HOST,
                    help=f"remote build host (default: {DEFAULT_HOST}, "
                         "or $LSP_REMOTE_HOST)")
    ap.add_argument("--jobs", type=int, default=None,
                    help="cargo -j (default: remote nproc, capped at 24)")
    ap.add_argument("--test-threads", type=int, default=None,
                    help="libtest --test-threads (default: half of --jobs)")
    ap.add_argument("--no-sync", action="store_true",
                    help="reuse the remote tree as-is; skip rsync")
    ap.add_argument("--with-git", action="store_true",
                    help="also sync .git (off by default; see module docstring)")
    ap.add_argument("--sync-back", metavar="SUBPATH", default=None,
                    help="after a successful run, copy this path back from "
                         "the remote target dir. Relative to CARGO_TARGET_DIR, "
                         "NOT the source tree (e.g. debug/openplanet-lsp)")
    ap.add_argument("--sync-back-must-change", action="store_true",
                    help="refuse (exit 4) if the fetched artefact is "
                         "byte-identical to the one already there (B147)")
    ap.add_argument("-v", "--verbose", action="store_true", help="show sync stats")
    ap.add_argument("--disk-floor", type=float, default=DEFAULT_DISK_FLOOR_GB,
                    metavar="GB",
                    help=f"refuse to build on a remote with less free space "
                         f"(default: {DEFAULT_DISK_FLOOR_GB:g}, "
                         "or $LSP_REMOTE_DISK_FLOOR_GB)")
    ap.add_argument("--allow-low-disk", action="store_true",
                    help="skip the free-space pre-flight (B106)")
    ap.add_argument("--allow-unresolved", action="store_true",
                    help="gate the tree even with an unresolved merge/rebase/"
                         "cherry-pick in progress (B132)")
    ap.add_argument("--reclaim-root", default=None, metavar="ABS_PATH",
                    help="absolute former local worktree path for reclaim")
    ap.add_argument("--remote-slug", default=None, metavar="NAME_HASH",
                    help="exact remote slug, which must match --reclaim-root")
    ap.add_argument("--execute-reclaim", action="store_true",
                    help="perform reclaim after validation (default: dry-run)")
    return ap


def main() -> int:
    ap = build_parser()
    a = ap.parse_args()
    if not (a.host or "").strip():
        die("--host / $LSP_REMOTE_HOST is empty. An empty host is not a host: "
            "ssh would be handed `:path` and fail in a dimension that has "
            "nothing to do with the empty variable.")

    # B140, and deliberately the FIRST thing after argument validation: this
    # is a pure check on a string, and refusing after a successful five-minute
    # remote build would be correct and useless.
    bad_sync_back = sync_back_verdict(a.sync_back)
    if bad_sync_back:
        die(bad_sync_back)

    if a.action == "selftest":
        return run_selftest()

    root = local_root()
    if a.action == "doctor":
        return doctor(a.host, root)
    if a.action == "inventory":
        return cmd_inventory(a.host, root)
    if a.action == "reclaim":
        return cmd_reclaim(a.host, root, a.reclaim_root, a.remote_slug,
                           a.execute_reclaim)

    slug = slug_for(root)
    # Two spellings of the same place, and they are not interchangeable. The
    # rule, and the reason, live in remote_paths -- one place, one selftest
    # assertion, because this was got wrong once per direction.
    paths = remote_paths(slug)
    rel_base = paths["rel_base"]
    base = paths["base"]
    remote_src = paths["ssh_src"]
    remote_target = paths["ssh_target"]
    rsync_dest = paths["rsync_src"]
    rsync_target = paths["rsync_target"]

    # argparse.REMAINDER keeps a leading "--"; drop it.
    rest = list(a.rest)
    if rest and rest[0] == "--":
        rest = rest[1:]

    if a.action == "clean":
        print(f"remote-build: removing {a.host}:{base}")
        with RemoteLock(slug), RemoteHostLock(a.host, slug):
            return run_ssh(a.host, f"rm -rf {base} && echo removed")

    if a.action == "shell":
        lock_rel = f".remote-build-locks/{slug}.lock"
        inner = f"cd {remote_src} 2>/dev/null || cd $HOME; exec bash -l"
        remote = (
            'mkdir -p "$HOME/.remote-build-locks" && '
            f'exec flock -x "$HOME"/{shlex.quote(lock_rel)} '
            f'sh -c {shlex.quote(inner)}'
        )
        ssh = ssh_base(a.host)
        os.execvp("ssh", [*ssh[:-1], "-t", a.host, remote])

    # B132, and deliberately BEFORE the lock and before any ssh: this is a
    # local read that costs nothing, and the condition it catches makes every
    # later step meaningless. Skipped under --no-sync, where this run copies
    # nothing and so cannot carry a conflict marker to the remote.
    #
    # Placed after the `clean` and `shell` early-returns on purpose: neither
    # syncs source, and refusing to open a shell or delete a remote tree
    # because a merge is in progress would be exactly the over-condemning this
    # guard has to avoid.
    if not a.no_sync and not a.allow_unresolved:
        refusal = unresolved_verdict(
            operation_in_progress(git_dir_for(root)), unmerged_paths(root)
        )
        if refusal:
            die(refusal)

    def disk_check_before_remote_mutation() -> None:
        preflight_remote_disk(
            host=a.host, rel_base=rel_base, remote_target=remote_target,
            floor_gb=a.disk_floor, allow_low_disk=a.allow_low_disk,
        )

    with RemoteLock(slug), preflighted_remote_host_lock(
        a.host, slug, disk_check_before_remote_mutation,
    ):
        rc, cores = capture_ssh(a.host, "nproc")
        remote_cores = int(cores) if rc == 0 and cores.isdigit() else 8
        # Cap the default: a 48-way link job can thrash memory, and leaving
        # headroom means a second lane's build is not starved.
        jobs = a.jobs if a.jobs else min(remote_cores, 24)
        tthreads = a.test_threads if a.test_threads else max(2, jobs // 2)

        # B106/B167. This one boundary owns the operational order: disk floor,
        # then mkdir, then rsync. The command/build is constructed and invoked
        # only after it returns. Its injected selftest proves a low-disk result
        # reaches neither mutating operation, plus the healthy positive path.
        preflight_remote_tree(
            host=a.host, rel_base=rel_base, remote_src=remote_src,
            remote_target=remote_target, rsync_dest=rsync_dest, root=root,
            floor_gb=a.disk_floor, allow_low_disk=a.allow_low_disk,
            no_sync=a.no_sync, with_git=a.with_git, verbose=a.verbose,
        )

        if a.action == "test":
            # `just test` self-throttles to -j2 for the sake of THIS desktop
            # (B085). On a 48-core build box that throttle is exactly wrong, so
            # pass an explicit -j, which the recipe honours over its default.
            inner = ["just", "test", f"-j{jobs}", "--",
                     f"--test-threads={tthreads}", *rest]
        elif a.action == "check":
            inner = ["cargo", "check", f"-j{jobs}", "--all-targets", *rest]
        elif a.action == "clippy":
            inner = ["cargo", "clippy", f"-j{jobs}", "--all-targets", *rest]
        elif a.action == "build":
            inner = ["cargo", "build", f"-j{jobs}", *rest]
        else:  # run
            if not rest:
                die("`run` needs a command, e.g. "
                    "`just rrun cargo test --lib`")
            inner = rest

        # Flags belong BEFORE the action; anything after it is the remote
        # command. Without this check a misplaced flag is shipped to the remote
        # shell and comes back as `bash: --sync-back: command not found`, which
        # names the wrong dimension entirely -- it reads as a broken remote
        # rather than as a local argument-order mistake. Generalised from
        # `--sync-back` alone after `-v` was made the same mistake with (exit
        # 127, same misleading message); see LOCAL_ONLY_FLAGS for why the
        # ambiguous flags are deliberately left out.
        stray = misplaced_flags(inner)
        if stray:
            die(f"`{stray[0]}` came after the action, so it was treated as part "
                f"of the remote command. Flags go first:\n"
                f"  remote-build.py {stray[0]} ... {a.action} -- <cmd>")

        cmd = " ".join(shlex.quote(x) for x in inner)
        script = (
            f"set -o pipefail; cd {remote_src} && "
            f"export CARGO_TARGET_DIR={remote_target} && "
            f"export CARGO_BUILD_JOBS={jobs} && "
            f"{cmd}"
        )
        # Name the TREE in terms the caller actually knows, not just a hash.
        #
        # A missing `cd` does not fail -- it silently retargets the run at
        # whatever tree the shell happens to be in, and every downstream signal
        # stays plausible: the build succeeds, the tests compile, the count is
        # merely smaller. A lane that meant to test `lane/unused` and actually
        # tested master gets `0 passed; 127 filtered out` and EXIT 0. That is a
        # false green by a fourth route (see docs/verification-hazards.md), and
        # the slug alone did not prevent it, because `unused-3616122f` and
        # `lsp-openplanet-bf0363fb` are equally opaque at a glance.
        #
        # The branch is not opaque. A lane knows it is on `lane/enum`, so
        # seeing `master` here is unmissable in a way a differing hash is not.
        # `dirty=N` rides the line the operator already reads. A separate
        # WARNING would fire on essentially every lane run -- gating
        # uncommitted work is the normal workflow -- and a warning that is
        # always on is one nobody reads. As a field on the banner it costs no
        # line and still answers "am I gating what I think I am?".
        nd = dirty_count(root)
        dirty = "" if nd is None else (f" dirty={nd}" if nd else "")
        print(f"remote-build: {a.host} [{slug}] branch={git_branch(root)}"
              f"{dirty} j={jobs} :: {cmd}", file=sys.stderr)
        t0 = time.monotonic()
        rc = run_ssh(a.host, script)
        status, note = exit_status(rc)
        if note:
            print(f"remote-build: {note}", file=sys.stderr)
        print(f"remote-build: exit {status} in {time.monotonic() - t0:.1f}s",
              file=sys.stderr)

        if a.sync_back:
            dest = root / "target" / a.sync_back
            # B147. Hashed BEFORE anything else touches the destination: this
            # is the only moment the previous artefact still exists, and both
            # the not-fetched message and --sync-back-must-change need it.
            before = file_sha256(dest)

            # B147. This used to read `if status == 0 and a.sync_back:` and
            # say NOTHING when it declined. The status was the command's,
            # which is right; the silence was not. The operator asked for an
            # artefact, did not get one, and was left with the previous one at
            # the destination wearing a plausible mtime.
            if status != 0:
                print(f"remote-build: "
                      f"{sync_back_not_fetched_verdict(a.sync_back, dest, status, before)}",
                      file=sys.stderr)
                return status

            dest.parent.mkdir(parents=True, exist_ok=True)
            # Measured failure this fixes, from lane-tooling hitting it in
            # anger: change_dir "/home/xertrov/$HOME/.remote-builds/..."
            # failed. Two lanes found it independently within the hour.
            src = f"{a.host}:{rsync_target}/{a.sync_back}"
            # B140: ask whether it is THERE before asking rsync to fetch it.
            # rsync's answer to a missing remote path is `Broken pipe (32)`,
            # which names the transport and not the path, and that is the
            # whole defect -- an infrastructure-shaped error standing in for a
            # condition we can simply look up.
            #
            # B147: and ask it in a spelling the remote shell will EXPAND. The
            # probe reports the path it resolved, so the refusal names the
            # real path rather than the template that produced it.
            probe_rc, probe_out = capture_ssh(
                a.host, sync_back_probe_script(rel_base, a.sync_back))
            remote_path = parse_probe_path(
                probe_out, f"{remote_target}/{a.sync_back}")
            if probe_rc != 0:
                die(sync_back_missing_verdict(a.sync_back, a.host, remote_path))
            print(f"remote-build: fetching {a.sync_back}", file=sys.stderr)
            back = subprocess.run(["rsync", "-az", src, str(dest)]).returncode
            if back != 0:
                die(f"sync-back failed (exit {back})", back or SYNC_BACK_FAILED)
            after = file_sha256(dest)
            if after is None:
                # rsync said 0 and there is no file. Nothing downstream can
                # recover from this, and a green here would be the exact
                # lost-EFFECT shape this ticket exists for.
                die(f"sync-back reported success but nothing is at {dest}. "
                    f"Treating a zero status with no artefact as a failure: "
                    f"the next step would read a path that is not there, or "
                    f"worse, one that is there and stale.", SYNC_BACK_FAILED)
            # B147: printed on EVERY fetch, not only under the flag. A
            # comparison run that records this line can tell afterwards
            # whether its two inputs were ever different, which is the check
            # the stale-artefact incident had no way to make.
            print(f"remote-build: wrote {dest} "
                  f"({dest.stat().st_size} bytes) sha256={after}",
                  file=sys.stderr)
            if a.sync_back_must_change and before is not None and before == after:
                die(sync_back_unchanged_verdict(a.sync_back, dest, after,
                                                a.host), SYNC_BACK_UNCHANGED)

        return status


if __name__ == "__main__":
    sys.exit(main())
