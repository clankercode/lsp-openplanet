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
import subprocess
import sys
import time
from pathlib import Path

DEFAULT_HOST = os.environ.get("LSP_REMOTE_HOST", "x-alpha")
REMOTE_ROOT = ".remote-builds"

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


def main() -> int:
    ap = argparse.ArgumentParser(
        prog="remote-build.py",
        description="Run builds and tests on a remote box instead of this one.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument(
        "action",
        choices=["test", "check", "clippy", "build", "run", "doctor", "shell", "clean"],
        help="test/check/clippy/build run the obvious thing; "
        "run takes an arbitrary command after --; "
        "doctor verifies the remote; shell opens one; clean removes the remote tree",
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
    a = ap.parse_args()

    root = local_root()
    if a.action == "doctor":
        return doctor(a.host, root)

    slug = slug_for(root)
    # Two spellings of the same place, and they are not interchangeable.
    # ssh runs a shell, so `$HOME` expands there. rsync does NOT run a shell on
    # its remote path -- it passes it literally, so `$HOME/x` becomes the absurd
    # `/home/user/$HOME/x`. rsync's remote paths are already relative to the
    # login directory, so the bare relative form is the correct one for it.
    rel_base = f"{REMOTE_ROOT}/{slug}"
    base = f"$HOME/{rel_base}"
    remote_src = f"{base}/src"
    remote_target = f"{base}/target"
    rsync_dest = f"{rel_base}/src"

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

    with RemoteLock(slug):
        rc, cores = capture_ssh(a.host, "nproc")
        remote_cores = int(cores) if rc == 0 and cores.isdigit() else 8
        # Cap the default: a 48-way link job can thrash memory, and leaving
        # headroom means a second lane's build is not starved.
        jobs = a.jobs if a.jobs else min(remote_cores, 24)
        tthreads = a.test_threads if a.test_threads else max(2, jobs // 2)

        if not a.no_sync:
            run_ssh(a.host, f"mkdir -p {remote_src} {remote_target}", quiet=True)
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

        cmd = " ".join(shlex.quote(x) for x in inner)
        script = (
            f"set -o pipefail; cd {remote_src} && "
            f"export CARGO_TARGET_DIR={remote_target} && "
            f"export CARGO_BUILD_JOBS={jobs} && "
            f"{cmd}"
        )
        print(f"remote-build: {a.host} [{slug}] j={jobs} :: {cmd}", file=sys.stderr)
        t0 = time.monotonic()
        rc = run_ssh(a.host, script)
        print(f"remote-build: exit {rc} in {time.monotonic() - t0:.1f}s",
              file=sys.stderr)

        if rc == 0 and a.sync_back:
            dest = root / "target" / a.sync_back
            dest.parent.mkdir(parents=True, exist_ok=True)
            src = f"{a.host}:{remote_target}/{a.sync_back}"
            print(f"remote-build: fetching {a.sync_back}", file=sys.stderr)
            back = subprocess.run(["rsync", "-az", src, str(dest)]).returncode
            if back != 0:
                die(f"sync-back failed (exit {back})", back)
            print(f"remote-build: wrote {dest}", file=sys.stderr)

        return rc


if __name__ == "__main__":
    sys.exit(main())
