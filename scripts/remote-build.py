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
import fcntl
import hashlib
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

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
    "--no-sync",
    "--with-git",
    "--host",
    "--allow-low-disk",
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


# An unresolved git operation, by the sentinel git itself writes. Ordered
# most-specific first, because a stopped rebase has BOTH `REBASE_HEAD` and a
# `rebase-merge/` directory and should be named once, as a rebase.
#
# `BISECT_LOG` is deliberately ABSENT. A bisect checks out real commits and
# produces no conflict markers, so gating during one is a legitimate thing to
# want; refusing there would be over-condemning, which costs a lane its whole
# workflow to prevent a failure that cannot occur.
GIT_OPERATION_SENTINELS = (
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase"),
    ("REBASE_HEAD", "rebase"),
    ("MERGE_HEAD", "merge"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
)


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
    lines.append(
        f"\nFinish or abort it (`git {operation} --continue` / "
        f"`git {operation} --abort`), or gate a different worktree. "
        "Pass --allow-unresolved to gate the tree exactly as it sits."
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
    return rc, None


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
        # operations are named HERE, by the filename git actually writes,
        # independent of the table.
        for filename, want in (
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("REBASE_HEAD", "rebase"),
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
        for dirname in ("rebase-merge", "rebase-apply"):
            marker = gd / dirname
            marker.mkdir()
            got = operation_in_progress(gd)
            shutil.rmtree(marker)
            if got != "rebase":
                problems.append(
                    f"git writes {dirname}/ during a rebase, but the probe "
                    f"reported {got!r}"
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

    if problems:
        print("FAIL: remote-build self-test", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print("PASS: remote-build guards — $HOME spelling, disk floor, df parsing,\n"
          "flag order, slug isolation and the unresolved-operation probe each\n"
          "reject the bad case and accept the good one.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        prog="remote-build.py",
        description="Run builds and tests on a remote box instead of this one.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument(
        "action",
        choices=["test", "check", "clippy", "build", "run", "doctor", "shell",
                 "clean", "selftest"],
        help="test/check/clippy/build run the obvious thing; "
        "run takes an arbitrary command after --; "
        "doctor verifies the remote; shell opens one; clean removes the remote "
        "tree; selftest proves this script's own guards can fail",
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
                    help="after a successful run, copy this path back from the "
                         "remote target dir (e.g. debug/openplanet-lsp)")
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
    a = ap.parse_args()
    if not (a.host or "").strip():
        die("--host / $LSP_REMOTE_HOST is empty. An empty host is not a host: "
            "ssh would be handed `:path` and fail in a dimension that has "
            "nothing to do with the empty variable.")

    if a.action == "selftest":
        return run_selftest()

    root = local_root()
    if a.action == "doctor":
        return doctor(a.host, root)

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
        return run_ssh(a.host, f"rm -rf {base} && echo removed")

    if a.action == "shell":
        os.execvp("ssh", ["ssh", "-t", a.host,
                          f"cd {remote_src} 2>/dev/null || cd $HOME; exec bash -l"])

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

    with RemoteLock(slug):
        rc, cores = capture_ssh(a.host, "nproc")
        remote_cores = int(cores) if rc == 0 and cores.isdigit() else 8
        # Cap the default: a 48-way link job can thrash memory, and leaving
        # headroom means a second lane's build is not starved.
        jobs = a.jobs if a.jobs else min(remote_cores, 24)
        tthreads = a.test_threads if a.test_threads else max(2, jobs // 2)

        # B106, before anything expensive or destructive. A build box with no
        # room does not fail as "no room": rsync, cargo and rustc each report
        # it in their own dimension, and the one the operator reads is usually
        # cargo's, which blames the code.
        if not a.allow_low_disk:
            verdict = disk_floor_verdict(
                remote_free_bytes(a.host, rel_base), a.disk_floor, a.host,
                remote_target,
            )
            if verdict:
                die(verdict)

        if not a.no_sync:
            # This return status used to be discarded, AND `quiet=True` ate the
            # message with it. MEASURED on a full filesystem: the remote said
            # `mkdir: No space left on device` and the wrapper carried on to
            # rsync as though the tree existed. B106 is about statuses that do
            # not survive; this was one of them, in this file.
            rc, out = capture_ssh(a.host, f"mkdir -p {remote_src} {remote_target}")
            if rc != 0:
                die(f"could not create the remote tree on {a.host} "
                    f"(mkdir exited {rc}):\n  {out}", rc)
            sync_up(a.host, root, rsync_dest, a.with_git, a.verbose)

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

        if status == 0 and a.sync_back:
            dest = root / "target" / a.sync_back
            dest.parent.mkdir(parents=True, exist_ok=True)
            # Measured failure this fixes, from lane-tooling hitting it in
            # anger: change_dir "/home/xertrov/$HOME/.remote-builds/..."
            # failed. Two lanes found it independently within the hour.
            src = f"{a.host}:{rsync_target}/{a.sync_back}"
            print(f"remote-build: fetching {a.sync_back}", file=sys.stderr)
            back = subprocess.run(["rsync", "-az", src, str(dest)]).returncode
            if back != 0:
                die(f"sync-back failed (exit {back})", back)
            print(f"remote-build: wrote {dest}", file=sys.stderr)

        return status


if __name__ == "__main__":
    sys.exit(main())
