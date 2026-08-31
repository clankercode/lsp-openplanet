#!/usr/bin/env python3
"""
issue_repro_game.py — compile an issue-repro fixture plugin in the live game
(Openplanet RemoteBuild) and report the in-game compile diagnostics.

Purpose: ground-truth for tests/fixtures/issue-repros/<n>-<slug>/ fixtures.
A fixture's LSP assertion is only meaningful if the game compiler's verdict
on the same bytes is known. This script stages the fixture into
OpenplanetNext/Plugins, RemoteBuild-loads it, captures the fresh
Openplanet.log compile window, then unloads and removes it (leave-as-found).

Usage:
    scripts/issue_repro_game.py <fixture-dir>            # probe one fixture
    scripts/issue_repro_game.py --all                    # probe every fixture
    scripts/issue_repro_game.py --all --json out.json    # also save results

Requires: Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.
Never writes to the fleet ledger (docs/plugin-fleet-ledger.json).

READ THIS BEFORE TRUSTING A RESULT
----------------------------------
A game REJECT is evidence about the engine. A game ACCEPT may not be.

Namespaces declared by OTHER INSTALLED PLUGINS are visible to a probe compile.
`MLFeed::VirtualKey` and `VehicleState::VirtualKey` both compiled on the
machine that produced docs/probes/, not because the engine has those
namespaces but because MLFeedRaceData.op and vehicle-state-list-dev were
installed. So an ACCEPT on a prefix you do not recognise may be an artefact of
that machine's plugin set, while a REJECT with 44 plugins installed means no
plugin declared it and the engine does not have it either.

The discriminator is a PAIRED probe: submit `Prefix::RealName` and
`Prefix::ZZZNoSuchName` together.
  * `Namespace 'Prefix' doesn't exist.` for both  -> not a scope.
  * `Identifier 'ZZZNoSuchName' is not a data type in namespace 'Prefix' or
    parent`                                       -> it IS a scope, and the
    first result means something.

Two more traps, both of which have already produced wrong numbers here:
  * Count distinct NAMES, not index keys. The loader keys one engine enum
    under several modules, so `len(keys)` overcounts — it inflated a B053
    estimate 34x (758 predicted, 22 real).
  * Type lookup walks ancestor scopes; VALUE lookup does not. `UI::VirtualKey`
    compiles, `UI::GlassSmash` does not, though `GlassSmash` compiles bare.

Full write-up, and the committed verdict corpora: docs/probes/README.md
"""
from __future__ import annotations
import argparse, atexit, contextlib, io, json, os, re, secrets, shutil, signal, stat, subprocess, sys, tempfile, time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
FIXTURES = REPO / "tests/fixtures/issue-repros"

# Reuse the proven RemoteBuild client + log parsing from the fleet driver.
sys.path.insert(0, str(HERE))
import fleet_dogfood as fleet  # noqa: E402

# Hard guard: this script must never write the production ledger. process_plugin
# (which we do not call) is the only writer; make save_state a landmine anyway.
fleet.save_state = lambda st: (_ for _ in ()).throw(
    RuntimeError("issue_repro_game must not write the fleet ledger")
)

PROBE_PREFIX = "lsp-issue-repro-"

# Every directory name pattern this repo's probes stage into the game's Plugins
# dir. A leftover one is an installed plugin nobody knows about, and it declares
# exactly the names the next probe is about to ask about -- so it turns that
# probe's REJECT into an ACCEPT for the very spelling under investigation.
PROBE_DIR_PREFIXES = (PROBE_PREFIX, "lsp-enum-corpus-probe")


# Written into every directory this tooling stages. Prefix matching alone is
# not an ownership test: a real plugin named `lsp-issue-repro-anything` would
# be silently deleted by a sweep, and deleting a user's plugin is far worse
# than refusing to run.
OWNER_MARKER = ".lsp-probe-owned"


def is_probe_dir(name: str) -> bool:
    return name.startswith(PROBE_DIR_PREFIXES)


def _path_present(d: Path) -> bool:
    """Like exists(), but a dangling symlink is still present and unsafe."""
    return os.path.lexists(d)


def _valid_probe_id(fid: str) -> bool:
    """A fixture id must name one direct child of the Plugins directory."""
    return (
        is_probe_dir(fid)
        and Path(fid).name == fid
        and not any(sep in fid for sep in ("/", "\\"))
        and fid not in {".", ".."}
    )


def _marker_record(d: Path) -> tuple[int, str] | None:
    """Parse a marker strictly; malformed marker text is never an ownership proof."""
    try:
        parts = (d / OWNER_MARKER).read_text().strip().split()
        if len(parts) != 2:
            return None
        pid, token = int(parts[0]), parts[1]
        if pid <= 0 or not re.fullmatch(r"[0-9a-f]{32}", token):
            return None
        return pid, token
    except (OSError, ValueError):
        return None


def is_empty_dir(d: Path) -> bool:
    """Only an empty real directory is safe to sweep without a lease."""
    try:
        return d.is_dir() and not d.is_symlink() and not any(d.iterdir())
    except OSError:
        return False


def claim_owner(dest: Path, pid: int | None = None, token: str | None = None) -> str:
    """Write an opaque per-stage lease marker and return its token.

    A marker is useful for diagnostics and for validating *this process's*
    cleanup.  It is deliberately not considered sufficient evidence to sweep
    a non-empty directory from an earlier process: a user can create the same
    filename, and preserving a suspicious fixture is safer than deleting a
    real plugin.
    """
    dest.mkdir(parents=True, exist_ok=True)
    if dest.is_symlink() or not dest.is_dir():
        raise ValueError(f"refusing to mark non-directory probe path: {dest}")
    token = token if token is not None else secrets.token_hex(16)
    (dest / OWNER_MARKER).write_text(
        f"{pid if pid is not None else os.getpid()} {token}\n"
    )
    return token


def owner_pid(d: Path) -> int | None:
    record = _marker_record(d)
    return record[0] if record is not None else None


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def probe_dirs_present(plugins_dir: Path, allow: set[str] | None = None) -> list[str]:
    """Probe-owned directories currently installed, excluding `allow`."""
    allow = allow or set()
    if not plugins_dir.is_dir():
        return []
    return sorted(
        d.name
        for d in plugins_dir.iterdir()
        if d.is_dir() and is_probe_dir(d.name) and d.name not in allow
    )


def sweep_stale(plugins_dir: Path, allow: set[str] | None = None) -> list[str]:
    """Remove probe-owned directories left by an earlier run.

    A backstop, NOT the primary mechanism. Start-of-run cleanup alone is what
    B065 showed to be insufficient, and here it is worse than insufficient: the
    run that does the sweeping is the one that was contaminated, so by the time
    it tidies up it has already been at risk. The real fix is that an
    interrupted run cannot leave state (see `staged`); for SIGKILL and power
    loss, which nothing in-process can clean, it leaves non-empty remains in
    place so every later probe fails closed instead of measuring beside them.
    """
    removed, refused = [], []
    for name in probe_dirs_present(plugins_dir, allow):
        d = plugins_dir / name
        pid = owner_pid(d)
        if pid is not None and pid != os.getpid() and pid_alive(pid):
            # Another probe process is using it right now. Deleting it would
            # corrupt that run's measurement, which is the same class of
            # damage this ticket is about, pointed the other way.
            refused.append(f"{name} (in use by live pid {pid})")
            continue
        # A stale marker cannot prove ownership across process restarts: it can
        # be copied, forged, or refer to a reused PID.  Sweep only empty legacy
        # directories; a non-empty one is left in place so probe_dirs_present()
        # will block a later measurement rather than deleting a user's plugin.
        if not is_empty_dir(d):
            marker = _marker_record(d)
            detail = (
                "untrusted or malformed marker"
                if marker is None
                else "non-empty marked directory"
            )
            refused.append(f"{name} ({detail})")
            continue
        remove_tree(d)
        if _path_present(d):
            refused.append(f"{name} (removal failed)")
        else:
            removed.append(name)
    if refused:
        print(
            "WARNING: left "
            + ", ".join(refused)
            + " in place. Inspect and remove by hand if it is stale.",
            file=sys.stderr,
        )
    return removed


# Paths this process has staged and not yet removed. Registered before any
# source bytes are copied, so a crash midway through copying is still cleaned
# up: the old code assigned `dest` only on success, so a `stage()` that died
# partway left a half-populated directory that the `finally` never saw.
@dataclass(frozen=True)
class StageLease:
    pid: int
    token: str | None
    device: int
    inode: int


_STAGED: dict[Path, StageLease] = {}


def _lease_matches(d: Path, lease: StageLease) -> bool:
    """Only delete the exact directory we created, never a replacement."""
    try:
        st = d.lstat()
    except OSError:
        return False
    if not stat.S_ISDIR(st.st_mode) or (st.st_dev, st.st_ino) != (lease.device, lease.inode):
        return False
    return lease.token is None or _marker_record(d) == (lease.pid, lease.token)


def remove_tree(d: Path, attempts: int = 3, lease: StageLease | None = None) -> bool:
    """Remove `d`, retrying, and REPORT whether it worked.

    `shutil.rmtree(ignore_errors=True)` swallows a permission error or a file
    the game still holds open, so a failed teardown looked identical to a
    successful one -- the exact silent-success shape this batch keeps finding.
    """
    for i in range(attempts):
        if not _path_present(d):
            return True
        if lease is not None and not _lease_matches(d, lease):
            return False
        try:
            # Never use ignore_errors here.  The return value is an output of
            # the safety protocol, not merely best-effort housekeeping.
            shutil.rmtree(d)
        except Exception:
            pass
        if not _path_present(d):
            return True
        if i + 1 < attempts:
            time.sleep(0.5)
    return not _path_present(d)


def _cleanup_staged(*_args) -> None:
    """Emergency unstage. REPORTS anything it could not remove.

    This is the one teardown path with no caller left to inspect a return
    value -- the handler runs and the process dies -- so stderr is the only
    channel by which the operator learns that a fixture is still live in their
    game. A silent failure here is the same shape as the `ignore_errors=True`
    this commit removed, minus the possibility of noticing.
    """
    stuck = []
    for dest, lease in list(_STAGED.items()):
        try:
            removed = remove_tree(dest, lease=lease)
        except Exception as exc:
            stuck.append(f"{dest} ({exc})")
            continue
        if removed:
            _STAGED.pop(dest, None)
        else:
            stuck.append(str(dest))
    if stuck:
        print(
            "ERROR: could not remove staged probe fixture(s): "
            + ", ".join(stuck)
            + " -- they are STILL INSTALLED in the game and will contaminate "
            "later probes (B077). Remove them by hand.",
            file=sys.stderr,
        )


def _install_teardown_handlers() -> None:
    """Unstage on the ways a run actually dies, not just on exceptions.

    `try/finally` covers exceptions and SIGINT. It does NOT cover SIGTERM,
    which by default terminates without unwinding -- so a `kill`, a session
    teardown or an OOM leaves the fixture installed. These handlers close that;
    SIGKILL remains uncatchable, which is what `sweep_stale` is for.
    """
    atexit.register(_cleanup_staged)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        try:
            signal.signal(sig, lambda s, _f: (_cleanup_staged(), os._exit(128 + s)))
        except (ValueError, OSError):
            pass  # not the main thread, or the platform lacks it


class staged:
    """Stage a fixture so that NO exit path can leave it installed."""

    def __init__(self, fixture: Path, fid: str, plugins_dir: Path | None = None):
        if not _valid_probe_id(fid):
            raise ValueError(f"unsafe probe fixture id: {fid!r}")
        self.fixture, self.fid = fixture, fid
        root = plugins_dir if plugins_dir is not None else fleet.OPN_PLUGINS
        self.root = Path(root)
        self.dest = self.root / fid
        self.lease: StageLease | None = None
        self.removed = False
        self.preserved = False

    def preserve(self) -> None:
        """Leave a visible blocker when RemoteBuild may still have us loaded."""
        self.preserved = True
        _STAGED.pop(self.dest, None)

    def __enter__(self) -> Path:
        # mkdir without exist_ok is the reservation.  A concurrent probe of the
        # same fixture must fail closed; it must never copy over or remove the
        # other process's live directory.
        try:
            self.root.mkdir(parents=True, exist_ok=True)
            if self.root.is_symlink() or not self.root.is_dir():
                raise ValueError(f"refusing non-directory Plugins root: {self.root}")
            self.dest.mkdir()
        except FileExistsError as exc:
            raise FileExistsError(
                f"probe fixture is already reserved or installed: {self.dest}"
            ) from exc

        st = self.dest.lstat()
        self.lease = StageLease(os.getpid(), None, st.st_dev, st.st_ino)
        try:
            # Claim ownership and register BEFORE copying anything, so a
            # signal during the copy still has an exact directory lease to
            # clean up.  A marker is never used to sweep a future non-empty
            # directory; it validates only this lease's cleanup.
            token = claim_owner(self.dest, pid=os.getpid())
            self.lease = StageLease(os.getpid(), token, st.st_dev, st.st_ino)
            _STAGED[self.dest] = self.lease
            # An explicit root rather than reassigning `fleet.OPN_PLUGINS`:
            # a module global mutated for the duration is not safe under
            # concurrency or re-entrancy, and it is how the first version of
            # this staged into the REAL game directory during its own tests.
            fleet.stage(self.fixture, self.fid, root=self.dest.parent)
        except BaseException:
            # A context manager's `__exit__` is NOT called when `__enter__`
            # raises, so a failure partway through copying would otherwise
            # leave exactly the half-populated directory this class exists to
            # prevent. That was the original bug's shape.
            if self.lease is not None and remove_tree(self.dest, lease=self.lease):
                _STAGED.pop(self.dest, None)
            raise
        return self.dest

    def __exit__(self, *_exc) -> bool:
        if self.preserved:
            return False
        self.removed = (
            self.lease is not None and remove_tree(self.dest, lease=self.lease)
        )
        if self.removed:
            _STAGED.pop(self.dest, None)
        return False


def rb_alive_with_retry(
    attempts: int = 3,
    delay: float = 2.0,
    alive: Callable[[], bool] | None = None,
    sleep_fn: Callable[[float], None] | None = None,
) -> bool:
    """RemoteBuild can wedge briefly (accepts then drops). Retry before
    declaring it dead so a single flaky probe doesn't skip a fixture."""
    for i in range(attempts):
        if (alive or fleet.game_alive)():
            return True
        if i + 1 < attempts:
            (sleep_fn or time.sleep)(delay)
    return False


def probe(
    fixture: Path,
    *,
    plugins_dir: Path | None = None,
    rb_alive_fn: Callable[[], bool] | None = None,
    rb_route_fn: Callable[..., dict] | None = None,
    log_size_fn: Callable[[], int] | None = None,
    read_log_window_fn: Callable[[int, str], str] | None = None,
    sleep_fn: Callable[[float], None] | None = None,
) -> dict:
    """Stage -> load -> capture -> unload + remove. Returns a result record."""
    info = fleet.read_info(fixture / "info.toml")
    name = info.get("name") or fixture.name
    fid = PROBE_PREFIX + fleet.folder_id(name)
    rec: dict = {"fixture": fixture.name, "folder_id": fid}
    root = Path(plugins_dir) if plugins_dir is not None else fleet.OPN_PLUGINS
    # B097: default to the transport ROUTER, not to RemoteBuild directly.
    # `load_plugin`/`unload_plugin` for a probe id exceed READ_CAP and can
    # never reach RemoteBuild's router; game_route carries them over
    # tm-control-mcp instead. Same signature, so both injection seams and
    # every existing caller are unchanged.
    alive = rb_alive_fn or fleet.game_alive
    route = rb_route_fn or fleet.game_route
    log_size = log_size_fn or fleet.log_size
    # B118: the RAW window by default. The filtered one drops every `[ TRAC]`
    # line, and both build markers are TRAC, so a build that never ran was
    # indistinguishable from one that ran clean. Same signature, so every
    # existing injection seam is unchanged.
    read_log_window = read_log_window_fn or fleet.read_log_window_raw
    sleep = sleep_fn or time.sleep

    if not _valid_probe_id(fid):
        rec["skipped"] = f"unsafe probe id derived from fixture name: {fid!r}"
        return rec

    # A stranded fixture from an earlier run declares the very names this
    # probe is about to ask about, so an ACCEPT measured beside one is not
    # evidence. Refuse rather than record it.
    strays = probe_dirs_present(root, allow={fid})
    if strays:
        rec["skipped"] = (
            f"probe-owned plugin(s) already installed: {', '.join(strays)}. "
            "An ACCEPT measured beside a stranded fixture is not evidence "
            "(B077). Re-run to sweep them, or remove them by hand."
        )
        return rec

    if not rb_alive_with_retry(alive=alive, sleep_fn=sleep):
        rec["skipped"] = "RemoteBuild not alive"
        return rec

    # Refuse to clobber anything already present (a probe id should never
    # collide with a real install, but check anyway).
    dest_root = root / fid
    if dest_root.exists() or (root / f"{fid}.op").exists():
        rec["skipped"] = f"probe id {fid} already present in Plugins dir"
        return rec

    mark = log_size()
    holder = staged(fixture, fid, plugins_dir=root)
    try:
        with holder:
            try:
                load = route(
                    "load_plugin", {"id": fid, "source": "user", "type": "folder"},
                    timeout=15,
                )
                sleep(2.5)  # let compile finish + log flush
                window = read_log_window(mark, fid)
                rec["load_response"] = load
                rec["raw_window"] = window[-4000:]
                # B127: ONE place decides whether a build succeeded, and it is
                # not the transport's reply. `load` is passed in because a
                # transport error is still a build failure, but it is only ever
                # additional evidence -- `{"error": "", "data": "Plugin
                # loaded"}` comes back for a build whose dependency could not
                # be resolved, so it can never be the whole answer.
                #
                # Attribution matters as much as the verdict: Openplanet.log is
                # shared, so another session's ERR in the same window would
                # turn a game ACCEPT into a false REJECT if counted as ours.
                # `build_verdict` keeps foreign and unattributed errors in
                # their own buckets and marks the window contended.
                rec.update(fleet.build_verdict(window, fid, load))
            finally:
                # Unload BEFORE the directory is removed. The reverse order
                # deletes files the game still has open, so the removal fails
                # -- silently, under the old `ignore_errors=True` -- and leaves
                # exactly the stranded fixture this ticket is about. Master had
                # this order right and the first version of the fix broke it.
                try:
                    unload = route("unload_plugin", {"id": fid}, timeout=15)
                except Exception as exc:
                    unload = {"error": f"unload_plugin raised: {exc}"}
                rec["unload_response"] = unload
                if unload.get("error"):
                    # If RemoteBuild did not acknowledge the unload, deleting
                    # the files would hide a plugin that may still be loaded.
                    # Preserve the marked fixture so every later probe refuses
                    # rather than turning that stale namespace into an ACCEPT.
                    holder.preserve()
                    rec["unload_error"] = str(unload["error"])
    except Exception as exc:
        rec["run_error"] = str(exc)
        rec["skipped"] = f"probe failed before a trustworthy verdict: {exc}"
    finally:
        rec["restored"] = holder.removed and not _path_present(root / fid)
        if not rec["restored"]:
            # Recorded AND acted on. A teardown that failed must not read as a
            # successful probe.
            detail = rec.get("unload_error", "could not remove staged fixture")
            rec["skipped"] = (
                f"teardown incomplete for {fid}: {detail}; it is still installed "
                "and will contaminate later probes (B077)"
            )
    return rec


def _selftest_child() -> int:
    """A child that stages and then dies in a specific way, so the selftest can
    prove each teardown path independently."""
    root = Path(os.environ["B077_PLUGINS"])
    fixture = Path(os.environ["B077_FIXTURE"])
    mode = os.environ["B077_SELFTEST_CHILD"]
    _install_teardown_handlers()

    if mode in {"sigterm", "sighup"}:
        sig = signal.SIGTERM if mode == "sigterm" else signal.SIGHUP
        with staged(fixture, "lsp-issue-repro-selftest", plugins_dir=root):
            assert (root / "lsp-issue-repro-selftest" / "Main.as").exists()
            os.kill(os.getpid(), sig)
            time.sleep(5)
        return 0

    if mode == "sigterm-stuck":
        # Stage, make removal impossible, then die by signal. On this path
        # nobody is left to inspect a return value, so stderr is the only
        # channel by which the operator can learn a fixture is still live in
        # their game.
        shutil.rmtree = lambda *a, **k: (_ for _ in ()).throw(
            RuntimeError("emergency teardown exploded")
        )
        staged(fixture, "lsp-issue-repro-stuck-child", plugins_dir=root).__enter__()
        os.kill(os.getpid(), signal.SIGTERM)
        time.sleep(5)
        return 0

    if mode == "atexit-stuck":
        shutil.rmtree = lambda *a, **k: (_ for _ in ()).throw(
            RuntimeError("atexit teardown exploded")
        )
        staged(fixture, "lsp-issue-repro-atexit-stuck", plugins_dir=root).__enter__()
        sys.exit(4)

    # `atexit` mode: enter WITHOUT a `with`, so no `__exit__` will ever run,
    # then exit normally. This is the only path the signal handlers do not
    # cover, and it is why the atexit hook is not redundant with them.
    staged(fixture, "lsp-issue-repro-atexit", plugins_dir=root).__enter__()
    assert (root / "lsp-issue-repro-atexit" / "Main.as").exists()
    sys.exit(3)


# Game-access doors on `fleet` that do not follow the `rb_`/`game_` prefix.
# Keep this next to nothing else: the prefix rule carries the general case and
# this is only for the names that predate it.
GAME_DOORS = {"control_mcp", "measure_read_cap"}


def _forbid_game_access() -> None:
    """Revoke the selftest's ability to reach the game AT ALL.

    Injecting `rb_alive_fn` / `rb_route_fn` fixes the cases that remember to
    pass them, but `probe()` defaults to `fleet.rb_alive`, so a future case
    written by someone who did not know the parameters were load-bearing gets
    the LIVE game. That is precisely how this selftest -- documented as offline
    -- came to round-trip a shared RemoteBuild twelve times per mutation run.

    It survived because a running game makes the wrong code WORK: the symptom
    was noise in a log belonging to a system this suite does not own and never
    reads, so there was no red test to find and there could not have been. When
    the failure mode is "nothing goes red", the fix has to be the thing that
    makes something go red. Hence revocation rather than a patched call site:
    injection still wins where it is supplied, and everything else now raises.
    """
    def refuse(*_a, **_k):
        raise AssertionError(
            "--selftest must not touch the game: RemoteBuild access attempted. "
            "This suite is offline by construction (B077). Inject "
            "rb_alive_fn/rb_route_fn/read_log_window_fn; if a case genuinely "
            "needs a game verdict it does not belong in --selftest."
        )
    # DERIVED, not listed. B097 added `game_route`/`game_alive`/`control_mcp`
    # as the default path, and a hand-written list would have revoked only the
    # two doors that existed when this was written -- the same "a guard at one
    # of N call sites" failure this function exists because of, recurring
    # inside its own fix. A list is only ever correct on the day it is typed.
    #
    # So: anything on `fleet` named `rb_*`, `game_*`, `control_mcp` or
    # `measure_read_cap` is a way to reach the game, and all of them are
    # revoked by name pattern. A door added later inherits the convention and
    # is closed the day it appears, with no edit here.
    doors = [
        name
        for name in dir(fleet)
        if (name.startswith(("rb_", "game_")) or name in GAME_DOORS)
        and callable(getattr(fleet, name, None))
    ]
    if len(doors) < 4:
        # Vacuity: a pattern that matches nothing revokes nothing and this
        # whole function becomes a no-op that reads as protection.
        raise AssertionError(
            f"the game-door pattern matched only {doors} — it no longer names "
            f"the transport functions, so --selftest is not actually offline"
        )
    for name in doors:
        setattr(fleet, name, refuse)
    fleet.read_log_window = refuse
    # B118 added a second log door. It reads a FILE rather than the socket, so
    # it matches none of the `rb_`/`game_` patterns above and would have stayed
    # open -- the same "a guard at one of N call sites" shape this function
    # exists because of.
    fleet.read_log_window_raw = refuse
    fleet.log_size = lambda: 0


def run_selftest() -> int:
    """Offline BY CONSTRUCTION. Game access is revoked, not merely unused --
    see `_forbid_game_access`. The staging lifetime is filesystem work, and
    that is the part B077 is about."""
    _forbid_game_access()
    failures: list[str] = []

    def check(what, got, want):
        if got != want:
            failures.append(f"{what}\n    got  {got!r}\n    want {want!r}")

    def raises(fn) -> bool:
        try:
            fn("get_status", {})
        except AssertionError:
            return True
        except Exception:
            return True
        return False

    # B097. The revocation above is the thing that makes this suite offline, so
    # it is itself under test. Named doors first -- if any one of these still
    # reaches the network, "offline by construction" is a claim and not a fact.
    for door in ("rb_route", "rb_alive", "game_route", "game_alive",
                 "control_mcp", "measure_read_cap"):
        check(f"the `{door}` door is revoked", raises(getattr(fleet, door)), True)
    # And the door NOBODY has written yet. A hand-listed revocation passes this
    # loop and fails this line, which is the whole difference between the two
    # designs: a new transport function must be closed WITHOUT an edit here.
    fleet.game_zzz_future_transport = lambda *_a, **_k: "REACHED THE GAME"
    try:
        _forbid_game_access()
        check(
            "a transport added later is revoked without editing the guard",
            raises(fleet.game_zzz_future_transport),
            True,
        )
    finally:
        del fleet.game_zzz_future_transport
    # Vacuity control: the pattern must actually be matching things. A rule
    # that matches nothing revokes nothing and reads exactly like protection.
    matched = [n for n in dir(fleet)
               if (n.startswith(("rb_", "game_")) or n in GAME_DOORS)
               and callable(getattr(fleet, n, None))]
    check("the door pattern matches the transport surface",
          len(matched) >= 6, True)

    with tempfile.TemporaryDirectory(prefix="b077-") as tmp:
        root = Path(tmp) / "Plugins"
        root.mkdir()
        fixture = Path(tmp) / "fx"
        (fixture / "src").mkdir(parents=True)
        (fixture / "src" / "Main.as").write_text(
            "namespace ZZZProbeOwnedNamespace { enum E { Foo } }\nvoid Main() {}\n"
        )
        (fixture / "info.toml").write_text('[meta]\nname = "b077 selftest"\n')

        # --- the sweep only touches probe-owned directories ---------------
        (root / "lsp-issue-repro-old").mkdir()
        (root / "lsp-enum-corpus-probe").mkdir()
        (root / "SomeUsersRealPlugin").mkdir()
        (root / "MLFeedRaceData").mkdir()
        removed = sweep_stale(root)
        check("sweep removes both probe prefixes", removed,
              ["lsp-enum-corpus-probe", "lsp-issue-repro-old"])
        check("sweep leaves real plugins alone",
              sorted(d.name for d in root.iterdir()),
              ["MLFeedRaceData", "SomeUsersRealPlugin"])

        # --- the guard sees a stranded fixture, and ignores our own --------
        (root / "lsp-issue-repro-stray").mkdir()
        check("guard reports a stray", probe_dirs_present(root), ["lsp-issue-repro-stray"])
        check("guard excludes the one we staged ourselves",
              probe_dirs_present(root, allow={"lsp-issue-repro-stray"}), [])
        shutil.rmtree(root / "lsp-issue-repro-stray")

        # --- reservation cannot escape or follow a link -------------------
        # The fixture name is user-controlled when this script is given a path,
        # so it must never turn into a path outside Plugins.
        rejected = False
        try:
            staged(fixture, "lsp-issue-repro-../../escaped", plugins_dir=root)
        except ValueError:
            rejected = True
        check("a path-traversal fixture id is rejected before staging", rejected, True)

        target = Path(tmp) / "not-plugins"
        target.mkdir()
        link = root / "lsp-issue-repro-symlink"
        link.symlink_to(target, target_is_directory=True)
        rejected = False
        try:
            with staged(fixture, link.name, plugins_dir=root):
                pass
        except FileExistsError:
            rejected = True
        check("a pre-existing destination symlink is rejected", rejected, True)
        check("staging never writes through that symlink", (target / OWNER_MARKER).exists(), False)
        check("or copies fixture bytes through that symlink", (target / "Main.as").exists(), False)
        link.unlink()

        # --- same-id concurrent probes cannot clobber one another ----------
        # The second reservation must fail before it can copy over or remove
        # the live first probe.  This is the destructive race the old
        # check-then-mkdir path left open.
        with staged(fixture, "lsp-issue-repro-concurrent", plugins_dir=root) as first:
            contender_rejected = False
            try:
                with staged(fixture, "lsp-issue-repro-concurrent", plugins_dir=root):
                    pass
            except FileExistsError:
                contender_rejected = True
            check("a same-id concurrent stage is rejected", contender_rejected, True)
            check("the first live fixture survives the rejected contender", first.is_dir(), True)
            check("the first fixture bytes also survive", (first / "Main.as").exists(), True)

        # --- happy path leaves nothing ------------------------------------
        with staged(fixture, "lsp-issue-repro-happy", plugins_dir=root) as dest:
            check("staged fixture is installed while in scope", dest.is_dir(), True)
            check("and it carries the source", (dest / "Main.as").exists(), True)
        check("happy path leaves no residue", probe_dirs_present(root), [])

        # --- an EXCEPTION mid-run leaves nothing ---------------------------
        try:
            with staged(fixture, "lsp-issue-repro-boom", plugins_dir=root):
                raise RuntimeError("probe blew up")
        except RuntimeError:
            pass
        check("an exception leaves no residue", probe_dirs_present(root), [])

        # --- a failure INSIDE staging leaves nothing -----------------------
        # The original code assigned `dest` only after stage() returned, so a
        # stage that died partway left a half-populated directory the teardown
        # never saw. That is the case that stranded an `.as` file.
        real_stage = fleet.stage

        def exploding_stage(pdir, fid, root=None):
            dest = (root or fleet.OPN_PLUGINS) / fid
            dest.mkdir(parents=True, exist_ok=True)
            (dest / "Main.as").write_text("namespace ZZZProbeOwnedNamespace {}\n")
            raise OSError("disk full halfway through copying")

        fleet.stage = exploding_stage
        try:
            with staged(fixture, "lsp-issue-repro-partial", plugins_dir=root):
                pass
        except OSError:
            pass
        finally:
            fleet.stage = real_stage
        check("a mid-stage failure leaves no residue", probe_dirs_present(root), [])

        # --- the ACCEPT guard REFUSES rather than measuring ----------------
        # The ticket's anti-vacuity test: a stranded fixture declaring
        # ZZZProbeOwnedNamespace must make the tool refuse, not report ACCEPT.
        (root / "lsp-issue-repro-stranded").mkdir()
        (root / "lsp-issue-repro-stranded" / "Main.as").write_text(
            "namespace ZZZProbeOwnedNamespace { enum E { Foo } }\n"
        )
        rec = probe(
            fixture,
            plugins_dir=root,
            rb_alive_fn=lambda: (_ for _ in ()).throw(
                AssertionError("the stray guard must run before RemoteBuild")
            ),
        )
        check("a stranded fixture makes the probe REFUSE",
              "lsp-issue-repro-stranded" in rec.get("skipped", ""), True)
        check("and it reports no verdict at all", "compiled_clean" in rec, False)
        shutil.rmtree(root / "lsp-issue-repro-stranded")
        # With no stray, the injectable liveness check gives the no-game
        # result.  The assertion above makes this selftest fail if it opens a
        # RemoteBuild connection before handling the stray.
        rec = probe(
            fixture,
            plugins_dir=root,
            rb_alive_fn=lambda: False,
            sleep_fn=lambda _delay: None,
        )
        check("with nothing stranded, that refusal does not fire",
              rec.get("skipped"), "RemoteBuild not alive")

        # --- unload happens before removal and failures block future probes -
        events = []

        def clean_route(route, _data, **_kwargs):
            events.append((route, (root / "lsp-issue-repro-b077-selftest").is_dir()))
            return {"data": "ok"}

        rec = probe(
            fixture,
            plugins_dir=root,
            rb_alive_fn=lambda: True,
            rb_route_fn=clean_route,
            log_size_fn=lambda: 0,
            read_log_window_fn=lambda _mark, _fid: "",
            sleep_fn=lambda _delay: None,
        )
        check("load and unload both see the staged directory", events,
              [("load_plugin", True), ("unload_plugin", True)])
        check("successful unload removes only after the route returns", rec["restored"], True)
        check("successful teardown is not skipped", "skipped" in rec, False)

        def failing_unload(route, _data, **_kwargs):
            if route == "unload_plugin":
                return {"error": "RemoteBuild did not acknowledge unload"}
            return {"data": "ok"}

        rec = probe(
            fixture,
            plugins_dir=root,
            rb_alive_fn=lambda: True,
            rb_route_fn=failing_unload,
            log_size_fn=lambda: 0,
            read_log_window_fn=lambda _mark, _fid: "",
            sleep_fn=lambda _delay: None,
        )
        failed_unload_dir = root / "lsp-issue-repro-b077-selftest"
        check("an unload failure is reported as a skipped probe",
              "did not acknowledge unload" in rec.get("skipped", ""), True)
        check("an unload failure leaves a visible blocker", failed_unload_dir.is_dir(), True)
        shutil.rmtree(failed_unload_dir)

        # A filesystem teardown failure is just as untrustworthy as an unload
        # failure: it must reach the result record, not merely stderr.
        real_rmtree = shutil.rmtree
        shutil.rmtree = lambda *a, **k: None
        try:
            rec = probe(
                fixture,
                plugins_dir=root,
                rb_alive_fn=lambda: True,
                rb_route_fn=clean_route,
                log_size_fn=lambda: 0,
                read_log_window_fn=lambda _mark, _fid: "",
                sleep_fn=lambda _delay: None,
            )
        finally:
            shutil.rmtree = real_rmtree
        failed_remove_dir = root / "lsp-issue-repro-b077-selftest"
        check("a removal failure is reported as a skipped probe", "skipped" in rec, True)
        check("a removal failure is not reported restored", rec["restored"], False)
        check("a removal failure leaves the fixture visible", failed_remove_dir.is_dir(), True)
        _STAGED.pop(failed_remove_dir, None)
        real_rmtree(failed_remove_dir)

        # --- B118: the three false-green channels, measured as PAIRS -------
        # The bug is a MISSING signal, so a red run cannot validate these on
        # its own (docs/verification-hazards.md, "When the subject is an
        # ABSENCE"). Asserting only that the bad half is caught would pass just
        # as well for a guard that condemns EVERYTHING -- a guard that fires on
        # both halves of a pair is as broken as one that fires on neither. So
        # every case below measures BOTH halves and asserts they DIFFER.
        #
        # The log text is copied verbatim in shape from a real RemoteBuild
        # session (2026-08-31, OP 1.29.14), not invented, so the parsing is
        # tested against what the game actually emits.
        SELFTEST_FID = "lsp-issue-repro-b077-selftest"

        def log_line(sev, msg):
            return f"[    ScriptEngine] [{sev}] [23:13:19.786] [RemoteBuild]  {msg}"

        def game_log(fid, *, started=True, loaded=True, located_err=False,
                     raw_err=False, failed=False, foreign_err=False):
            out = []
            if started:
                out.append(log_line(" TRAC", f'Starting build for "{fid}"'))
                out.append(log_line(" TRAC", "-- Add source: \"C:/users/steamuser"
                                             f"/OpenplanetNext/Plugins/{fid}"
                                             "/Main.as\" (322 bytes)"))
            if located_err:
                out.append(log_line("ERROR", "C:/users/steamuser/OpenplanetNext"
                                             f"/Plugins/{fid}/Main.as (3, 34) :"
                                             "  ERR : Shared type 'X' doesn't "
                                             "match the declaration in module "
                                             "'Editor'"))
            if foreign_err:
                out.append(log_line("ERROR", "C:/users/steamuser/OpenplanetNext"
                                             "/Plugins/SomeOtherPlugin/Main.as "
                                             "(5, 1) :  ERR : Identifier 'Q' is "
                                             "not a data type"))
            if raw_err:
                out.append(log_line("ERROR", "Couldn't find required plugin "
                                             f'dependency "ghosts-pp" while '
                                             f'loading plugin "{fid}"'))
            if failed:
                out.append(log_line("ERROR", "Script compilation failed!"))
            if loaded:
                out.append(log_line(" TRAC", f"Loaded plugin '{fid}' (version 0.1.0)"))
            return "\n".join(out)

        def probe_log(window):
            return probe(
                fixture, plugins_dir=root,
                rb_alive_fn=lambda: True,
                rb_route_fn=lambda _r, _d, **_k: {"data": "ok"},
                log_size_fn=lambda: 0,
                read_log_window_fn=lambda _mark, _fid: window,
                sleep_fn=lambda _delay: None,
            )

        clean = probe_log(game_log(SELFTEST_FID))
        check("a build that ran clean is CLEAN", clean["build_status"], "CLEAN")
        check("and compiled_clean agrees", clean["compiled_clean"], True)

        # Every case here INJECTS a window, so none of them can see which
        # reader `probe()` picks when nothing is injected -- and that default
        # IS channel 1. Pin it directly: swap both log doors for sentinels and
        # check which one the un-injected probe reaches for. Without this, a
        # revert of the default to the TRAC-filtering reader passes the whole
        # suite below.
        picked = []
        saved_raw, saved_filtered = fleet.read_log_window_raw, fleet.read_log_window
        fleet.read_log_window_raw = lambda *_a, **_k: (
            picked.append("raw") or game_log(SELFTEST_FID)
        )
        fleet.read_log_window = lambda *_a, **_k: (
            picked.append("filtered") or ""
        )
        try:
            probe(
                fixture, plugins_dir=root,
                rb_alive_fn=lambda: True,
                rb_route_fn=lambda _r, _d, **_k: {"data": "ok"},
                log_size_fn=lambda: 0,
                sleep_fn=lambda _delay: None,
            )
        finally:
            fleet.read_log_window_raw, fleet.read_log_window = saved_raw, saved_filtered
        check("probe() defaults to the RAW log window, not the TRAC-filtered one",
              picked, ["raw"])

        # PAIR 1 -- channel 1. A build that never ran vs one that ran clean.
        # Pre-B118 these were the SAME observation: the window was filtered
        # down to "did anything complain", and neither had complained.
        # NOT an empty window. `Openplanet.log` is shared, so a window in which
        # OUR build never ran still contains other sessions' traffic -- and an
        # empty string would let a broken `build_slice` look correct for free
        # (mutation testing caught exactly that).
        never = probe_log(game_log("lsp-issue-repro-some-other-lane"))
        check("a build that never ran is NOT-RUN", never["build_status"], "NOT-RUN")
        check("a build that never ran is not clean", never["compiled_clean"], False)
        check("PAIR 1 discriminates: never-ran differs from ran-clean",
              never["build_status"] != clean["build_status"], True)

        # PAIR 2 -- channel 2. A located error vs a location-less one. BOTH
        # must reach a bucket, and they must reach DIFFERENT ones: a located
        # ERR is a verdict about the code, a location-less one is not.
        located = probe_log(game_log(SELFTEST_FID, located_err=True, failed=True))
        rawless = probe_log(game_log(SELFTEST_FID, raw_err=True))
        check("a located error reaches game_errors", bool(located["game_errors"]), True)
        check("a located error is REJECTED", located["build_status"], "REJECTED")
        check("a location-less error reaches raw_build_errors",
              bool(rawless["raw_build_errors"]), True)
        check("a location-less error is BUILD-ERR", rawless["build_status"], "BUILD-ERR")
        check("a location-less error is invisible to the LOCATED parser "
              "(this is the defect, still true of that parser)",
              rawless["game_errors"], [])
        # The two buckets must PARTITION the errors, not overlap. If a located
        # error also landed in the raw bucket, a plain REJECT would be
        # reportable as a build failure and the distinction B118 exists to
        # draw would be lost in the other direction.
        check("a located error is NOT double-counted as a raw build error",
              located["raw_build_errors"], [])
        check("PAIR 2 discriminates: the two error kinds do not collapse",
              located["build_status"] != rawless["build_status"], True)
        check("and neither is mistaken for clean",
              (located["compiled_clean"], rawless["compiled_clean"]), (False, False))

        # PAIR 3 -- channel 3. `Loaded plugin` is logged even AFTER
        # `Script compilation failed!`, so presence of the load line is
        # necessary and NOT sufficient. A guard keying on it alone reads a
        # failed compile as a successful one.
        failed_then_loaded = probe_log(
            game_log(SELFTEST_FID, located_err=True, failed=True, loaded=True)
        )
        check("a failed compile still logs Loaded plugin (the trap)",
              failed_then_loaded["build_loaded"], True)
        check("but it is not clean", failed_then_loaded["compiled_clean"], False)
        check("PAIR 3 discriminates: failed-then-loaded differs from a real load",
              failed_then_loaded["build_status"] != clean["build_status"], True)

        # PAIR 3b -- `Script compilation failed!` as the ONLY signal. The case
        # above pairs it with a located error, so the located error alone
        # decided the verdict and the failure summary was never load-bearing
        # (mutation testing caught that too). Here the located errors are
        # absent from the window -- which happens for real, since the summary
        # and its diagnostics can straddle a window boundary.
        failed_only = probe_log(game_log(SELFTEST_FID, failed=True))
        check("a compile failure with no located error is still not clean",
              failed_only["compiled_clean"], False)
        check("PAIR 3b discriminates: failure-summary-only differs from clean",
              failed_only["build_status"] != clean["build_status"], True)

        # PAIR 3c -- started but never finished: no errors, no `Loaded plugin`.
        # A build that hangs or dies mid-compile looks exactly like a clean one
        # to any guard that only counts errors.
        started_only = probe_log(game_log(SELFTEST_FID, loaded=False))
        check("a build that started but never loaded is not clean",
              started_only["compiled_clean"], False)
        check("and it is BUILD-ERR, not a verdict about the code",
              started_only["build_status"], "BUILD-ERR")
        check("PAIR 3c discriminates: started-but-unfinished differs from clean",
              started_only["build_status"] != clean["build_status"], True)

        # PAIR 4 -- the CONTROL that proves the harness is wired at all. This
        # attribution already worked before B118; if it ever stops, the other
        # three cases are measuring nothing.
        foreign = probe_log(game_log(SELFTEST_FID, foreign_err=True))
        ours = probe_log(game_log(SELFTEST_FID, located_err=True))
        check("a foreign plugin's error is not attributed to us",
              foreign["game_errors"], [])
        check("a foreign plugin's error still marks the window contended",
              foreign["contended"], True)
        check("our own error IS attributed to us", bool(ours["game_errors"]), True)
        check("PAIR 4 discriminates: foreign and own errors land differently",
              (bool(foreign["game_errors"]), bool(ours["game_errors"])),
              (False, True))

        # --- the sweep must NEVER delete something that is not ours --------
        # Prefix matching alone would delete a real plugin that happened to be
        # named `lsp-issue-repro-*`.  A marker alone cannot prove ownership
        # after a restart either: it can be copied, malformed, or point at a
        # reused PID.  Deleting a user's plugin is far worse than refusing.
        impostor = root / "lsp-issue-repro-someones-real-plugin"
        impostor.mkdir()
        (impostor / "Main.as").write_text("void Main() {}\n")
        removed = sweep_stale(root)
        check("sweep refuses an unmarked non-empty dir", removed, [])
        check("and leaves it on disk", impostor.is_dir(), True)
        claim_owner(impostor)
        check("a plausible marker still cannot authorize a sweep", sweep_stale(root), [])
        check("the marked impostor remains intact", impostor.is_dir(), True)
        (impostor / OWNER_MARKER).write_text("not a valid owner record\n")
        check("a malformed marker also cannot authorize a sweep", sweep_stale(root), [])
        check("the malformed-marker impostor remains intact", impostor.is_dir(), True)
        shutil.rmtree(impostor)

        # An empty unmarked dir is still safe to remove: it declares nothing,
        # and it is the shape of the two this ticket found.
        (root / "lsp-issue-repro-legacy-empty").mkdir()
        check("an empty legacy dir is swept", sweep_stale(root),
              ["lsp-issue-repro-legacy-empty"])

        # --- a LIVE probe's fixture must survive another process's sweep ----
        # A genuinely OTHER live process is never removed.  Once it exits, the
        # marker still does not authorize sweeping a non-empty directory: PID
        # reuse makes a stale liveness value unsuitable as ownership proof.
        sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        live = root / "lsp-issue-repro-live-elsewhere"
        claim_owner(live, pid=sleeper.pid)
        (live / "Main.as").write_text("namespace ZZZProbeOwnedNamespace {}\n")
        check("sweep keeps a fixture owned by a live pid", sweep_stale(root), [])
        check("and it is still there", live.is_dir(), True)
        sleeper.kill()
        sleeper.wait()
        claim_owner(live, pid=os.getpid())  # indistinguishable from PID reuse
        check("a reused pid cannot authorize deleting a non-empty fixture", sweep_stale(root), [])
        check("the reused-pid fixture remains intact", live.is_dir(), True)
        shutil.rmtree(live)

        # The live-pid branch does NOT decide this outcome, and the mutation
        # harness is what established that: disabling it left the suite green.
        # Proof it is unreachable as a protection --
        #     owner_pid(d) non-None  <=>  the marker file exists
        #     marker exists          =>   the directory is non-empty
        #     non-empty              =>   refused by the later branch anyway
        # so "empty AND marked" cannot occur and the branch never changes the
        # DECISION. It is not dead, though: it changes the WARNING, and telling
        # an operator "another probe is using this right now" versus a generic
        # refusal is the difference between waiting and investigating.
        #
        # So assert what it actually does. A guard tested through a path where
        # a different guard answers first is not tested at all -- that is why
        # the non-empty assertions above could not see it.
        sleeper2 = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        try:
            busy = root / "lsp-issue-repro-busy"
            claim_owner(busy, pid=sleeper2.pid)
            (busy / "Main.as").write_text("void Main() {}\n")
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                swept = sweep_stale(root)
            check("a live owner's fixture is not swept", swept, [])
            check("and the warning names the live pid, not a generic refusal",
                  f"in use by live pid {sleeper2.pid}" in err.getvalue(), True)
        finally:
            sleeper2.kill(); sleeper2.wait()
        shutil.rmtree(busy)

        # --- a marker lost during a current run fails closed ---------------
        # This case DOES strand a fixture, deliberately, and the test must show
        # the whole cost rather than assert the survival and tidy it away. A
        # test that removes the leak it just demonstrated can never go red for
        # it, which is the shape this ticket keeps finding.
        #
        # The trade is defensible: refusing to delete a directory whose marker
        # we cannot verify protects against an inode reused at the same path,
        # and the strand is safe because everything downstream refuses. But
        # "safe because everything downstream refuses" is a claim about the
        # chain, so the chain is what gets asserted.
        missing_marker = root / "lsp-issue-repro-marker-lost"
        with staged(fixture, missing_marker.name, plugins_dir=root) as dest:
            (dest / OWNER_MARKER).unlink()
        check("a lease whose marker disappeared is not removed blindly",
              missing_marker.is_dir(), True)
        check("and the fixture really is left installed -- this is a STRAND",
              (missing_marker / "Main.as").exists(), True)
        # 1. the sweep must not silently adopt and delete it either
        check("the sweep also refuses it", sweep_stale(root), [])
        check("so it is still on disk", missing_marker.is_dir(), True)
        # 2. and nothing may measure beside it. This is what makes the strand
        #    safe: it costs an operator a manual cleanup, never a false ACCEPT.
        rec = probe(
            fixture,
            plugins_dir=root,
            rb_alive_fn=lambda: (_ for _ in ()).throw(
                AssertionError("must refuse before reaching RemoteBuild")
            ),
        )
        check("a later probe REFUSES beside the strand",
              missing_marker.name in rec.get("skipped", ""), True)
        check("and emits no verdict", "compiled_clean" in rec, False)
        _STAGED.pop(missing_marker, None)
        shutil.rmtree(missing_marker)

        # --- a removal that FAILS must not report success ------------------
        stuck = root / "lsp-issue-repro-stuck"
        claim_owner(stuck)
        real_rmtree = shutil.rmtree
        shutil.rmtree = lambda *a, **k: None  # silently does nothing
        try:
            check("remove_tree reports failure instead of swallowing it",
                  remove_tree(stuck, attempts=1), False)
            shutil.rmtree = lambda *a, **k: (_ for _ in ()).throw(
                RuntimeError("teardown implementation exploded")
            )
            check("remove_tree also reports an unexpected teardown exception",
                  remove_tree(stuck, attempts=1), False)
        finally:
            shutil.rmtree = real_rmtree
        real_rmtree(stuck)

        # --- SIGTERM leaves nothing ----------------------------------------
        # `try/finally` does NOT cover SIGTERM: it terminates without
        # unwinding. Run a real child process and kill it.
        def run_child(mode: str):
            return subprocess.run(
                [sys.executable, str(Path(__file__).resolve())],
                env={**os.environ, "B077_PLUGINS": str(root),
                     "B077_FIXTURE": str(fixture), "B077_SELFTEST_CHILD": mode},
                capture_output=True, text=True, timeout=60,
            )

        proc = run_child("sigterm")
        check("the SIGTERM child did not exit cleanly", proc.returncode != 0, True)
        check("SIGTERM leaves no residue", probe_dirs_present(root), [])

        proc = run_child("sighup")
        check("the SIGHUP child did not exit cleanly", proc.returncode != 0, True)
        check("SIGHUP leaves no residue", probe_dirs_present(root), [])

        # A normal interpreter exit with something still staged. The signal
        # handlers cannot see this one, so it is what makes the atexit hook
        # load-bearing rather than decorative.
        proc = run_child("atexit")
        check("the atexit child exited normally", proc.returncode, 3)
        check("a normal exit leaves no residue", probe_dirs_present(root), [])

        proc = run_child("atexit-stuck")
        check("an atexit teardown exception is reported", "STILL INSTALLED" in proc.stderr, True)
        check("the atexit exception leaves the fixture visible", (root / "lsp-issue-repro-atexit-stuck").is_dir(), True)
        real_rmtree(root / "lsp-issue-repro-atexit-stuck")

        # --- an emergency teardown that FAILS must SAY SO -------------------
        # The same silent-success shape as `remove_tree`, on the one path where
        # no caller is left to check a return value: the handler runs, cannot
        # remove, and the process dies. Without a message the operator's game
        # is left with a live fixture in it and nothing said so.
        proc = run_child("sigterm-stuck")
        check("a failed emergency teardown names the directory it could not remove",
              "lsp-issue-repro-stuck-child" in proc.stderr, True)
        check("and says the fixture is STILL INSTALLED",
              "still installed" in proc.stderr.lower(), True)
        # Proves the scenario is real rather than a test that merely fails to
        # print: the child genuinely left the fixture installed.
        check("and the stuck child really did leave it installed",
              (root / "lsp-issue-repro-stuck-child").is_dir(), True)
        real_rmtree(root / "lsp-issue-repro-stuck-child")

        # --- the suite cannot reach the game even by accident -------------
        # The incident was not a decision; it was the absence of one. The code
        # took a shared resource while its docstring said it did not, so the
        # only durable check is on the CALL, not on the outcome: an outcome
        # assertion cannot tell "never touched it" from "touched it and got the
        # expected answer".
        # Overwriting `fleet.rb_alive` with a recorder would destroy the very
        # guard under test, so the assertion is on the REFUSAL, which is the
        # observable form of "the call was not made".
        refused = None
        try:
            probe(fixture, plugins_dir=root)   # no injection: the incident's shape
        except AssertionError as exc:
            refused = str(exc)
        check("an un-injected probe is refused rather than reaching the game",
              refused is not None and "must not touch the game" in refused, True)

    # --- B145: the engine's verdict must be RECORDED, and recorded honestly --
    # 64 fixtures carried zero committed engine evidence because the generator
    # printed and nobody stored. Every case here is a PAIR: a recorder that
    # refuses everything, or that calls everything CLEAN, passes a suite that
    # only checks records get written.
    def rec_of(status, errors=(), skipped=None):
        r = {"fixture": "fx", "build_status": status,
             "game_errors": [dict(file="lsp-issue-repro-fx/Main.as", line=l,
                                  col=c, sev="ERR", msg=m)
                             for l, c, m in errors]}
        if skipped:
            r["skipped"] = skipped
        return r

    rejected = tsv_row(rec_of("REJECTED", [(21, 25, "Only object types ... &inout")]),
                       {"count": 12}, "1.29.14", "2026-09-01")
    check("B145: a REJECTED fixture records the engine's polarity",
          (rejected["verdict"], rejected["build_status"]), ("REJECT", "REJECTED"))
    check("B145: the engine message is recorded VERBATIM to the right of the "
          "colon — that is the part hand-transcription loses",
          rejected["engine_message"],
          "Main.as(21,25): Only object types ... &inout")
    check("B145: the staging folder id is normalised away — it is an artefact "
          "of the probe, not of the fixture",
          "lsp-issue-repro" in rejected["engine_message"], False)
    check("B145: this LSP's answer is recorded beside the engine's",
          rejected["lsp_today"], "DIAGNOSES(12)")

    # PAIR: the ACCEPT half. B086's defect was a recorder that only handled
    # one polarity.
    accepted = tsv_row(rec_of("CLEAN"), {"count": 0}, "1.29.14", "2026-09-01")
    check("B145: a CLEAN fixture records the OTHER polarity",
          (accepted["verdict"], accepted["build_status"], accepted["lsp_today"]),
          ("ACCEPT", "CLEAN", "CLEAN"))
    check("B145: the two polarities produce DIFFERENT rows",
          rejected["engine_message"] == accepted["engine_message"], False)

    # A probe that could not look must not be recorded as an answer.
    notrun = tsv_row(rec_of("NOT-RUN", skipped="RemoteBuild not alive"),
                     {"count": 0}, "1.29.14", "2026-09-01")
    check("B145: a probe that never ran is NOT recorded as ACCEPT",
          notrun["verdict"], "UNKNOWN")
    # A skipped probe never sets build_status AT ALL -- that is the real shape,
    # and testing only an explicit "NOT-RUN" left the fallback untested. It is
    # the fallback that decides what a probe which never spoke is recorded as.
    absent = tsv_row({"fixture": "fx", "skipped": "RemoteBuild not alive"},
                     {"count": 0}, "1.29.14", "2026-09-01")
    check("B145: a record with NO build_status at all falls back to NOT-RUN, "
          "not to CLEAN — a probe that never spoke has not accepted anything",
          (absent["build_status"], absent["verdict"]), ("NOT-RUN", "UNKNOWN"))
    check("B145: and its message says why it could not be measured",
          absent["engine_message"], "RemoteBuild not alive")
    check("B145: an unmeasured LSP is UNMEASURED, not CLEAN",
          tsv_row(rec_of("CLEAN"), {"count": None}, "1.29.14",
                  "2026-09-01")["lsp_today"], "UNMEASURED")

    # --- B145 follow-up: a prose reason must EXPIRE when its subject moves --
    # B157 and B168 found the same defect in two separate audits: a JUSTIFIED
    # row whose free-text reason WAS the false claim, so the audit accepted the
    # defect BECAUSE of the justification. This artifact reproduced it within
    # one regeneration: b102-array-sort-correct-callbacks carried the note "the
    # engine rejects all three 'correct' comparators and the LSP is silent on
    # all three", and after B152 moved those comparators to the sibling fixture
    # the row measured engine ACCEPT and LSP DIAGNOSES(3) -- both halves
    # inverted -- with the note carried across unchanged and nothing firing.
    def stamped(**kw):
        base = {"fixture": "fx", "build_status": "REJECTED", "verdict": "REJECT",
                "engine_compiled": "4", "lsp_today": "CLEAN",
                "divergence": "the LSP misses this",
                "divergence_for": "REJECT|CLEAN"}
        base.update(kw)
        return base

    check("B145: a note stamped for the CURRENT measurement settles the row",
          agreement_problems([stamped()]), [])
    flipped = agreement_problems([stamped(verdict="ACCEPT",
                                          build_status="CLEAN",
                                          lsp_today="DIAGNOSES(3)")])
    if not flipped:
        check("B145: a note survives a POLARITY FLIP — the exact b102 shape, "
              "where both halves inverted and the prose still read as a reason",
              flipped, ["<a problem>"])
    elif "stamped for" not in flipped[0]:
        check("B145: the expiry report names the stamp and the new state",
              flipped, ["stamped for"])
    # It must expire on EITHER half moving, not only on a full flip.
    for label, kw in (("the LSP's answer alone", {"lsp_today": "DIAGNOSES(9)"}),
                      ("the engine's verdict alone",
                       {"verdict": "ACCEPT", "build_status": "CLEAN"})):
        if not agreement_problems([stamped(**kw)]):
            check(f"B145: a note expires when {label} moves", [], ["<a problem>"])
    # An unstamped note -- hand-typed straight into the file, dodging the
    # generator's expiry -- must not settle anything either.
    if not agreement_problems([stamped(divergence_for="-")]):
        check("B145: an UNSTAMPED note does not settle a row; typing prose "
              "directly into the file must not bypass the expiry",
              [], ["<a problem>"])
    # PAIR: a row with NO note is judged on its own merits, not condemned for
    # a missing stamp.
    check("B145: a row with no note is not asked for a stamp",
          agreement_problems([stamped(divergence="-", divergence_for="-",
                                      verdict="REJECT",
                                      lsp_today="DIAGNOSES(3)")]), [])
    # The carry/expire decision itself, which lives inside a function that
    # needs a live game -- so it is tested here, where it can be reached. Two
    # mutations that disabled it were MISSED before this existed.
    _probe = {"fixture": "fx", "verdict": "REJECT", "lsp_today": "CLEAN"}
    check("B145: a note whose stamp still holds is carried across a regen",
          carry_forward({"fx": ("a reason", "REJECT|CLEAN")}, _probe),
          ("a reason", "REJECT|CLEAN"))
    check("B145: a note whose stamp no longer holds is DROPPED, not carried",
          carry_forward({"fx": ("a reason", "ACCEPT|DIAGNOSES(3)")}, _probe),
          None)
    check("B145: a fixture with no previous note carries nothing",
          carry_forward({}, _probe), None)
    check("B145: a placeholder note is not carried as if it were a reason",
          carry_forward({"fx": ("-", "REJECT|CLEAN")}, _probe), None)
    # And a freshly written row must be stamped with ITS OWN state, or the
    # stamp stops describing what the note was written against.
    _fresh = tsv_row(rec_of("REJECTED", [(1, 1, "boom")]), {"count": 0},
                     "1.29.14", "2026-09-01", {"fx": "a fresh reason"})
    check("B145: a freshly stamped note records this row's own measurement",
          (_fresh["divergence"], _fresh["divergence_for"]),
          ("a fresh reason", divergence_state(_fresh["verdict"],
                                              _fresh["lsp_today"])))
    check("B145: a row with no note carries no stamp",
          tsv_row(rec_of("CLEAN"), {"count": 0}, "1.29.14",
                  "2026-09-01")["divergence_for"], "-")

    check("B145: divergence_state is the pair a divergence claim is ABOUT",
          divergence_state("ACCEPT", "DIAGNOSES(3)"), "ACCEPT|DIAGNOSES(3)")
    if divergence_state("ACCEPT", "CLEAN") == divergence_state("REJECT", "CLEAN"):
        check("B145: divergence_state distinguishes the engine's verdict",
              "collapsed", "distinct")

    # --- B145 follow-up: polarity agreement is not the same question -------
    # b102-array-sort-mismatched-callbacks declares `int &inout` at line 21,
    # which is not declarable. The engine emitted two errors there, compiled
    # ZERO bodies, and never verdicted one of the twelve Sort calls the fixture
    # exists to measure -- while the LSP reported 12 diagnostics about exactly
    # those calls. Recorded as REJECTED beside DIAGNOSES(12), the agreement
    # check called it agreement. Five of 63 rows were in that state.
    def reached_row(**kw):
        base = {"fixture": "fx", "build_status": "REJECTED", "verdict": "REJECT",
                "engine_compiled": "3", "lsp_today": "DIAGNOSES(12)",
                "divergence": "-"}
        base.update(kw)
        base.setdefault("divergence_for",
                        divergence_state(base["verdict"], base["lsp_today"]))
        return base

    aborted = agreement_problems([reached_row(engine_compiled="0")])
    if not aborted:
        check("B145: an engine that compiled ZERO bodies beside an LSP "
              "reporting call-site diagnostics is reported — both spoke, and "
              "that is all the polarity check can see",
              aborted, ["<a problem>"])
    elif "same question" not in aborted[0]:
        check("B145: the report says why polarity is not enough",
              aborted, ["same question"])

    # PAIR, and it is the one that matters: four of the five real rows are
    # fixtures whose SUBJECT is a declaration, where a declaration-time
    # rejection is the complete and correct answer. A note settles them, and a
    # check that refused them regardless would be turned off within the week.
    check("B145: a declared reason settles a declaration-time rejection",
          agreement_problems([reached_row(
              engine_compiled="0",
              divergence="the fixture's subject IS the duplicate declaration")]),
          [])
    # PAIR: an engine that DID reach the call sites needs no note.
    check("B145: an engine that compiled bodies is ordinary agreement",
          agreement_problems([reached_row()]), [])
    # PAIR: and zero bodies beside a SILENT LSP is a real disagreement already
    # caught by the polarity check — it must not be re-reported as this.
    silent = agreement_problems([reached_row(engine_compiled="0",
                                             lsp_today="CLEAN")])
    if len(silent) != 1 or "no divergence is declared" not in silent[0]:
        check("B145: zero bodies beside a SILENT LSP stays the plain polarity "
              "disagreement, reported once and not twice",
              silent, ["<one polarity problem>"])
    # PAIR: a CLEAN build compiles bodies or not; either way this rule is about
    # REJECTED units only, or every clean fixture would demand a note.
    check("B145: a CLEAN engine verdict is never asked for this note",
          agreement_problems([reached_row(build_status="CLEAN",
                                          verdict="ACCEPT",
                                          engine_compiled="0",
                                          lsp_today="CLEAN")]), [])

    # The counter itself: attributed, and INFO-only.
    _cr = {"folder_id": "lsp-issue-repro-fx", "diagnostics": [
        {"file": "lsp-issue-repro-fx/Main.as", "sev": "INFO",
         "msg": "Compiling void Main()"},
        {"file": "lsp-issue-repro-fx/Main.as", "sev": "INFO",
         "msg": "Compiling void Other()"},
        {"file": "lsp-issue-repro-fx/Main.as", "sev": "INFO",
         "msg": "Something else entirely"},
        {"file": "someone-elses-plugin/Main.as", "sev": "INFO",
         "msg": "Compiling void Theirs()"},
        {"file": "lsp-issue-repro-fx/Main.as", "sev": "ERR",
         "msg": "Compiling void NotAnInfoLine()"},
    ]}
    check("B145: only this folder's INFO `Compiling` lines are counted — the "
          "log is shared, and another session's compile would inflate ours",
          folder_compiled(_cr), 2)
    check("B145: a record with no folder id counts nothing rather than "
          "everything",
          folder_compiled({"diagnostics": _cr["diagnostics"]}), 0)
    aborted_rec = tsv_row(
        {"fixture": "fx", "folder_id": "lsp-issue-repro-fx",
         "build_status": "REJECTED", "diagnostics": [],
         "game_errors": [{"file": "lsp-issue-repro-fx/Main.as", "line": 21,
                          "col": 25, "sev": "ERR", "msg": "not declarable"}]},
        {"count": 12}, "1.29.14", "2026-09-01")
    check("B145: a declaration-time abort records ZERO compiled bodies",
          aborted_rec["engine_compiled"], 0)

    # --- B145: measuring the LSP side, and refusing to guess ---------------
    # Every one of these returns "I could not measure", which the recorder
    # turns into UNMEASURED. A version that answered 0 instead would record a
    # CLI that never ran as a clean run -- the same silence-as-answer shape
    # that left 64 fixtures with no evidence.
    class _Proc:
        def __init__(self, out):
            self.stdout, self.stderr = out, ""

    _fx = FIXTURES / "does-not-matter"
    check("B145: an LSP binary that is not there is UNMEASURED, not clean",
          run_lsp_on_fixture(_fx, binary=Path("/nonexistent/openplanet-lsp"))
          ["count"], None)
    check("B145: output with no recognised summary line is UNMEASURED",
          run_lsp_on_fixture(_fx, binary=Path(__file__),
                             runner=lambda c: _Proc("panicked at src/main.rs"))
          ["count"], None)
    check("B145: a CLI that could not be launched at all is UNMEASURED",
          run_lsp_on_fixture(
              _fx, binary=Path(__file__),
              runner=lambda c: (_ for _ in ()).throw(OSError("boom")))
          ["count"], None)
    # PAIR: and real output must still parse, or the recorder can never
    # measure anything and the artifact is empty by construction.
    check("B145: an ordinary summary line parses",
          run_lsp_on_fixture(_fx, binary=Path(__file__),
                             runner=lambda c: _Proc("7 diagnostics (7 errors) in /x"))
          ["count"], 7)
    check("B145: B090's zero shape parses as zero, not as unmeasured",
          run_lsp_on_fixture(
              _fx, binary=Path(__file__),
              runner=lambda c: _Proc("0 errors/warnings (3 info hidden) in /x"))
          ["count"], 0)

    # folder_warnings must select on SEVERITY as well as attribution. Widening
    # it to errors would double-count every rejection: the errors are already
    # in the message from `game_errors`.
    _mixed = {"folder_id": "lsp-issue-repro-fx", "diagnostics": [
        {"file": "lsp-issue-repro-fx/Main.as", "line": 1, "col": 1,
         "sev": "ERR", "msg": "an error"},
        {"file": "lsp-issue-repro-fx/Main.as", "line": 2, "col": 1,
         "sev": "WARN", "msg": "a warning"},
        {"file": "lsp-issue-repro-fx/Main.as", "line": 3, "col": 1,
         "sev": "INFO", "msg": "compiling"},
    ]}
    check("B145: folder_warnings selects WARN only — errors are already "
          "carried by game_errors and would be double-counted",
          [d["msg"] for d in folder_warnings(_mixed)], ["a warning"])
    check("B145: a record with no folder id attributes nothing rather than "
          "attributing everything",
          folder_warnings({"diagnostics": _mixed["diagnostics"]}), [])

    # --- B145: engine and LSP must be compared, with gaps DECLARED ----------
    def agree_row(**kw):
        base = {"fixture": "fx", "build_status": "REJECTED",
                "verdict": "REJECT", "lsp_today": "DIAGNOSES(3)",
                "divergence": "-"}
        base.update(kw)
        # Default the stamp to this row's own state, so a case that is not
        # ABOUT expiry does not accidentally test expiry.
        base.setdefault("divergence_for",
                        divergence_state(base["verdict"], base["lsp_today"]))
        return base

    check("B145: engine REJECTED and LSP diagnosing is agreement",
          agreement_problems([agree_row()]), [])
    check("B145: engine CLEAN and LSP clean is agreement",
          agreement_problems([agree_row(build_status="CLEAN",
                                        verdict="ACCEPT",
                                        lsp_today="CLEAN")]), [])
    # An engine WARNING is the engine saying something. Recording it as
    # silence manufactured a disagreement with an LSP that was right, and lost
    # the message text this artifact exists to keep.
    check("B145: an engine WARNING beside an LSP diagnostic is AGREEMENT, not "
          "a gap — the engine is a compiler and this LSP is also a linter",
          agreement_problems([agree_row(build_status="CLEAN",
                                        verdict="ACCEPT-WITH-WARNINGS",
                                        lsp_today="DIAGNOSES(1)")]), [])
    # PAIR: and a warned fixture the LSP says NOTHING about is still a gap.
    if not agreement_problems([agree_row(build_status="CLEAN",
                                         verdict="ACCEPT-WITH-WARNINGS",
                                         lsp_today="CLEAN")]):
        check("B145: a fixture the engine WARNS about and the LSP is silent "
              "on is still reported", [], ["<a problem>"])
    warned = tsv_row(
        {"fixture": "fx", "folder_id": "lsp-issue-repro-fx",
         "build_status": "CLEAN", "game_errors": [],
         "diagnostics": [{"file": "lsp-issue-repro-fx/Main.as", "line": 8,
                          "col": 13, "sev": "WARN",
                          "msg": "Variable 'x' hides another variable"},
                         {"file": "someone-elses-plugin/Main.as", "line": 1,
                          "col": 1, "sev": "WARN", "msg": "not ours"}]},
        {"count": 1}, "1.29.14", "2026-09-01")
    check("B145: an engine WARNING is recorded verbatim, not replaced by the "
          "clean boilerplate",
          warned["engine_message"],
          "Main.as(8,13): Variable 'x' hides another variable")
    check("B145: a warned fixture is distinguishable from a silent one",
          warned["verdict"], "ACCEPT-WITH-WARNINGS")
    check("B145: another session's warning in the shared log is NOT "
          "attributed to this fixture",
          "not ours" in warned["engine_message"], False)

    for label, row, want in (
        ("a gap the LSP misses",
         agree_row(lsp_today="CLEAN"), "no divergence is declared"),
        ("a false positive the LSP invents",
         agree_row(build_status="CLEAN", verdict="ACCEPT"),
         "no divergence is declared"),
        ("an engine verdict the probe could not take",
         agree_row(build_status="NOT-RUN", verdict="UNKNOWN"),
         "could not measure"),
        ("a BUILD-ERR engine verdict",
         agree_row(build_status="BUILD-ERR", verdict="UNKNOWN"),
         "could not measure"),
        ("an LSP answer that was never measured",
         agree_row(lsp_today="UNMEASURED"), "not measured"),
        ("a STALE divergence note on a row that now agrees",
         agree_row(divergence="the LSP misses this"), "now AGREE"),
    ):
        got = agreement_problems([row])
        if not got:
            check(f"B145: {label} is reported", got, ["<a problem>"])
        elif want not in got[0]:
            check(f"B145: the report for {label} says why", got, [want])

    # A NOT-RUN row is settled by a stamped reason, and ONLY by one.
    _nr = {"fixture": "fx", "build_status": "NOT-RUN", "verdict": "UNKNOWN",
           "engine_compiled": "0", "lsp_today": "CLEAN"}
    if not agreement_problems([dict(_nr, divergence="-", divergence_for="-")]):
        check("B145: an unexplained NOT-RUN row is still reported", [],
              ["<a problem>"])
    check("B145: a NOT-RUN row with a stamped reason is a recorded finding, "
          "not a permanent red — a gate that can never go green is ignored",
          agreement_problems([dict(_nr, divergence="the name contains a slash",
                                   divergence_for="UNKNOWN|CLEAN")]), [])
    if not agreement_problems([dict(_nr, divergence="the name contains a slash",
                                    divergence_for="UNKNOWN|DIAGNOSES(2)")]):
        check("B145: a NOT-RUN row's reason expires like any other", [],
              ["<a problem>"])

    # PAIR: a DECLARED divergence is data, not a failure. A check that refuses
    # every known gap would make the artifact impossible to commit at all.
    check("B145: a declared divergence is accepted — a known gap is a "
          "recorded fact, not an error",
          agreement_problems([agree_row(lsp_today="CLEAN",
                                        divergence="LSP has no &inout check")]),
          [])

    # --- B145: writing and reading the corpus back -------------------------
    _b145 = Path(tempfile.mkdtemp(prefix="b145-tsv-"))
    try:
        dest = _b145 / "corpus.tsv"
        rows = [tsv_row(rec_of("CLEAN"), {"count": 0}, "1.29.14", "2026-09-01")]
        rows[0]["fixture"] = "fx-a"
        rows[0]["divergence"] = "a hand-written note"
        rows[0]["divergence_for"] = divergence_state(rows[0]["verdict"],
                                                     rows[0]["lsp_today"])
        write_tsv(dest, rows, "1.29.14", "2026-09-01", "e" * 64)
        text = dest.read_text()
        for want in ("1.29.14", "2026-09-01", "e" * 64, "fx-a"):
            if want not in text:
                check(f"B145: the corpus records {want!r}", text[:200], want)
        body = [l for l in text.splitlines() if not l.startswith("#")]
        check("B145: the corpus has a header row and one data row per fixture",
              (body[0].split("\t"), len(body)), (list(TSV_COLUMNS), 2))
        # The hand-written note is the ONLY authored field; a regen that drops
        # it turns every explained gap into an unexplained one, and the run
        # still looks green because the notes were what made it green.
        check("B145: hand-written divergence notes survive a regen, WITH the "
              "state each was written against",
              read_declared_divergences(dest),
              {"fx-a": ("a hand-written note", rows[0]["divergence_for"])})
        check("B145: a corpus that does not exist yet yields no notes rather "
              "than raising",
              read_declared_divergences(_b145 / "nope.tsv"), {})
        # A tab inside a message would silently shift every later column.
        tabby = tsv_row(rec_of("REJECTED", [(1, 1, "a\tb")]), {"count": 1},
                        "1.29.14", "2026-09-01")
        write_tsv(dest, [tabby], "1.29.14", "2026-09-01", None)
        data = [l for l in dest.read_text().splitlines()
                if not l.startswith("#")][1]
        check("B145: a tab inside an engine message does not shift the "
              "columns of every row after it",
              len(data.split("\t")), len(TSV_COLUMNS))

        # A failed regeneration must never replace the last known-good
        # evidence. The generator used to write first and validate second, so
        # an UNMEASURED LSP or an engine probe that never ran destroyed the
        # committed corpus immediately before returning 1. Exercise all three
        # non-answer states rather than pinning only the incident's spelling.
        original = b"last known-good corpus bytes\n"
        saved_version = globals()["fleet_engine_version"]
        saved_binary = globals()["lsp_cli_bin"]
        saved_run_lsp = globals()["run_lsp_on_fixture"]
        globals()["fleet_engine_version"] = lambda: "1.29.14"
        globals()["lsp_cli_bin"] = lambda: Path(__file__)
        try:
            invalid_regens = (
                ("NOT-RUN", rec_of("NOT-RUN", skipped="probe did not run"),
                 {"count": 0}),
                ("BUILD-ERR", rec_of("BUILD-ERR", skipped="transport failed"),
                 {"count": 0}),
                ("UNMEASURED", rec_of("CLEAN"), {"count": None}),
            )
            for label, rec, lsp_result in invalid_regens:
                dest.write_bytes(original)
                globals()["run_lsp_on_fixture"] = (
                    lambda _fixture, result=lsp_result: result
                )
                rc = write_corpus(dest, [rec], all_fixtures=True)
                check(f"B145: a failing {label} regeneration returns non-zero",
                      rc, 1)
                check(f"B145: a failing {label} regeneration leaves the "
                      "existing corpus byte-identical",
                      dest.read_bytes(), original)
        finally:
            globals()["fleet_engine_version"] = saved_version
            globals()["lsp_cli_bin"] = saved_binary
            globals()["run_lsp_on_fixture"] = saved_run_lsp

        # Publication itself must be one same-directory rename. Simulate the
        # rename failing after the complete replacement has been staged: the
        # known-good destination must remain untouched and the temp must be
        # cleaned. This is the filesystem failure half of the validation test
        # above; either one alone leaves a destructive gap.
        dest.write_bytes(original)
        saved_replace = os.replace
        replace_calls = []

        def fail_replace(src, target):
            replace_calls.append((Path(src), Path(target)))
            raise OSError("simulated rename failure")

        os.replace = fail_replace
        rename_failed = False
        try:
            write_tsv(dest, rows, "1.29.14", "2026-09-01", "e" * 64)
        except OSError:
            rename_failed = True
        finally:
            os.replace = saved_replace
        check("B145: corpus publication reaches one atomic rename",
              rename_failed and len(replace_calls) == 1, True)
        if replace_calls:
            src, target = replace_calls[0]
            check("B145: the replacement temp is in the destination directory",
                  (src.parent, target.parent), (dest.parent, dest.parent))
        check("B145: a failed atomic rename preserves the old corpus bytes",
              dest.read_bytes(), original)
        check("B145: a failed atomic rename removes its staged temp",
              list(dest.parent.glob(f".{dest.name}.*.tmp")), [])
    finally:
        shutil.rmtree(_b145, ignore_errors=True)

    if failures:
        print(f"FAIL: {len(failures)} check(s) failed", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print("PASS: cleanup, isolation, and fail-closed guards verified.")
    return 0


# ---- B145: committing the engine's verdict, not just printing it -----------
# 64 fixtures carried ZERO committed engine evidence. The generator existed and
# was hardened (B118's four-valued build_status, B127's load-is-not-build fix,
# B123's raw reader); its output went to an ad-hoc path a human named, and the
# engine's message was then hand-typed into a Rust assertion. So the fixture
# README's own rule -- "a fixture's LSP assertion is only meaningful against the
# game compiler's verdict on the same bytes" -- held at the moment someone ran
# the probe and not afterwards.
#
# The record is a B129 CORPUS artifact and a manifest row, deliberately not a
# parallel mechanism: B129's inventory found the load-bearing gap is that the
# artifact -> generator edge exists nowhere machine-readable, and "a generator
# with no recorded output" is exactly that shape.
#
# TWO measurements per fixture, both machine-produced:
#   * the ENGINE's verdict, from this probe;
#   * what THIS LSP says about the same bytes today, from the real CLI.
# The only hand-written field is `divergence`, and it is required precisely
# where the two disagree -- so a disagreement cannot be recorded silently, and
# a disagreement that gets FIXED shows up as a stale divergence note.

LSP_TSV_FLAGS = ("--typedb-dir", "tests/fixtures/typedb")
CLEAN_REASON = "compiled and loaded, no error attributed to this folder"


def repo_root_for_tsv() -> Path:
    return FIXTURES.parent.parent.parent


def lsp_cli_bin() -> Path:
    """The CLI the Rust issue-repro tests drive, which is the DEBUG binary.

    Deliberately not the release build: `run_issue_repro` uses
    `CARGO_BIN_EXE_openplanet-lsp`, and recording a different binary's answer
    would make the corpus agree with something no test runs.
    """
    return repo_root_for_tsv() / "target/debug/openplanet-lsp"


def run_lsp_on_fixture(fixture: Path, binary: Path | None = None,
                       runner=None) -> dict:
    """What this LSP says about the fixture today. Never raises.

    Returns `count=None` when the CLI could not be run or its output could not
    be parsed. That is a THIRD answer and the corpus writer refuses on it:
    "I could not measure the LSP" must not be recorded as "the LSP was clean",
    which is the shape that put 64 fixtures here in the first place.
    """
    binary = lsp_cli_bin() if binary is None else binary
    if not binary.is_file():
        return {"count": None, "error": f"no LSP binary at {binary}"}
    cmd = [str(binary), "check", *LSP_TSV_FLAGS, str(fixture)]
    run = runner or (lambda c: subprocess.run(
        c, capture_output=True, text=True, timeout=120,
        cwd=str(repo_root_for_tsv()),
        env={**os.environ, "NO_COLOR": "1"}))
    try:
        proc = run(cmd)
    except Exception as exc:  # noqa: BLE001 - a driver failure, not a verdict
        return {"count": None, "error": f"lsp run failed: {exc}"}
    out = (proc.stdout or "") + (proc.stderr or "")
    # fleet.parse_lsp_count, not a fourth copy of the regex: it already knew
    # one summary shape and B145 measured a second (B090's `0 errors/warnings
    # (N info hidden: ...)`), which is exactly the kind of drift a duplicated
    # pattern keeps out of sight.
    count = fleet.parse_lsp_count(out)
    if count is None:
        return {"count": None, "error": "no diagnostic count in LSP output"}
    return {"count": count, "error": ""}


def _engine_text(d: dict) -> str:
    """One engine diagnostic, with the STAGING path normalised away.

    The folder id is an artefact of this probe -- it is derived from the
    fixture's info.toml name and changes when that does -- so keeping it would
    make the record churn for a reason that has nothing to do with the engine.
    Everything to the right of the colon is verbatim: that is the part
    hand-transcription loses and the part that moves between engine versions.
    """
    leaf = str(d.get("file", "")).split("/")[-1] or "?"
    return f"{leaf}({d['line']},{d['col']}): {d['msg']}"


def folder_compiled(rec: dict) -> int:
    """How many function bodies the engine actually COMPILED for this folder.

    B145 follow-up, and it is the difference between a verdict and a verdict
    about the thing you asked. Openplanet logs `Compiling <signature>` per
    function body it reaches. A DECLARATION error -- an undeclarable parameter
    mode, a name conflict, a parse failure -- aborts the unit before any body
    is compiled, so the engine returns REJECTED having never looked at a single
    call site.

    Measured: `b102-array-sort-mismatched-callbacks` declares
    `bool WrongModeInout(int &inout a, int &inout b)` at line 21. `int &inout`
    is not declarable. The engine emitted exactly two errors, both at line 21,
    and compiled ZERO bodies -- so it never returned a verdict on ANY of the
    twelve `Sort` calls at lines 32-43, which are the entire subject of the
    fixture. The row nevertheless recorded REJECTED beside an LSP reporting 12
    diagnostics, and the agreement check called that agreement.

    Both sides said something. They were not talking about the same code.
    """
    fid = rec.get("folder_id") or ""
    if not fid:
        return 0
    return sum(
        1 for d in rec.get("diagnostics", [])
        if d.get("sev") == "INFO"
        and str(d.get("msg", "")).startswith("Compiling ")
        and str(d.get("file", "")).split("/")[0] == fid
    )


def folder_warnings(rec: dict) -> list[dict]:
    """WARN diagnostics attributed to THIS probe's folder.

    `build_verdict` buckets errors by attribution because Openplanet.log is
    shared; warnings get no such bucket, so they are filtered here by the same
    rule rather than by widening a shared primitive four consumers depend on.
    An unattributed warning is another session's, and counting it would turn a
    clean fixture into a warned one at random.
    """
    fid = rec.get("folder_id") or ""
    if not fid:
        return []
    return [d for d in rec.get("diagnostics", [])
            if d.get("sev") == "WARN"
            and str(d.get("file", "")).split("/")[0] == fid]


def divergence_state(verdict: str, lsp_today: str) -> str:
    """The measurement a divergence note was written against.

    B157/B168 found the same defect in two separate audits: a JUSTIFIED row
    whose free-text reason WAS the false claim, so the audit accepted the
    defect BECAUSE of the justification. Prose records that somebody was
    convinced; it never records that they were right.

    This artifact had a live instance of it within one regeneration.
    `b102-array-sort-correct-callbacks` carried the note *"the engine rejects
    all three 'correct' comparators and the LSP is silent on all three"*. After
    lane-const's B152 moved those comparators to the sibling fixture, the same
    row measured engine ACCEPT and LSP DIAGNOSES(3) -- BOTH HALVES INVERTED --
    and the note carried across the regeneration unchanged. Nothing fired: the
    stale-note rule only triggers when the two sides come to AGREE, and these
    two still disagreed, just in the opposite direction.

    So a note is stamped with the (verdict, LSP answer) pair it was written
    against, and is carried across a regeneration ONLY while that pair still
    holds. It cannot be verified -- prose never can -- but it can be made to
    EXPIRE the moment its subject moves, which turns an unfalsifiable claim
    into one with a shelf life. A note whose stamp no longer matches is
    dropped, and the row goes back to demanding a fresh reason.
    """
    return f"{verdict}|{lsp_today}"


def tsv_row(rec: dict, lsp: dict, engine_version: str, probe_date: str,
            divergences: dict[str, str] | None = None) -> dict:
    """One corpus row: the engine's answer and this LSP's, side by side."""
    divergences = divergences or {}
    slug = rec["fixture"]
    status = rec.get("build_status") or "NOT-RUN"
    # B145. The engine's WARNINGS are ground truth too, and the first run of
    # this recorder dropped them: 37d-variable-shadow came back CLEAN with the
    # boilerplate "no error attributed", while the engine had in fact said
    # `Variable 'x' hides another variable of same name in outer scope`. That
    # is precisely the message text this artifact exists to stop losing, and
    # discarding it also manufactured a disagreement with an LSP that was
    # right to flag the same line.
    #
    # build_status stays B118's four ERROR-polarity values -- that contract is
    # not this ticket's to change. The warnings ride in `verdict` and in the
    # message, so "the engine said nothing at all" stays distinguishable from
    # "the engine compiled it but complained".
    warns = folder_warnings(rec)
    verdict = {"CLEAN": "ACCEPT", "REJECTED": "REJECT"}.get(status, "UNKNOWN")
    if verdict == "ACCEPT" and warns:
        verdict = "ACCEPT-WITH-WARNINGS"

    if status == "REJECTED":
        # The staging folder id is an artefact of THIS probe, not engine ground
        # truth about the fixture, so the path is normalised to the file inside
        # it. Everything to the right of the colon -- which is the part
        # hand-transcription loses and the part that moves between engine
        # versions -- is verbatim.
        message = " | ".join(_engine_text(d)
                             for d in rec.get("game_errors", [])) \
            or "REJECTED with no attributed error"
    elif status == "CLEAN":
        message = " | ".join(_engine_text(d) for d in warns) or CLEAN_REASON
    else:
        message = rec.get("skipped") or f"build_status={status}"

    lsp_count = lsp.get("count")
    lsp_says = "UNMEASURED" if lsp_count is None else (
        "CLEAN" if lsp_count == 0 else f"DIAGNOSES({lsp_count})")
    return {
        "fixture": slug,
        "verdict": verdict,
        "build_status": status,
        "engine_compiled": folder_compiled(rec),
        "engine_message": message,
        "lsp_today": lsp_says,
        "engine_version": engine_version,
        "probe_date": probe_date,
        "divergence_for": (divergence_state(verdict, lsp_says)
                           if divergences.get(slug) else "-"),
        "divergence": divergences.get(slug, "-"),
    }


TSV_COLUMNS = ("fixture", "verdict", "build_status", "engine_compiled",
               "engine_message", "lsp_today", "engine_version", "probe_date",
               "divergence_for", "divergence")


def agreement_problems(rows: list[dict]) -> list[str]:
    """Where the engine and this LSP disagree without a declared reason.

    The check the ticket asks for, expressed over the corpus rather than over
    the Rust source: a hand-typed assertion cannot drift from the engine
    evidence behind it if both sides are measured and compared.

    An UNMEASURED LSP or a NOT-RUN engine is refused rather than skipped. "I
    could not look" is not "I looked and they agree" -- the whole reason this
    artifact exists is that a silence was being read as an answer.
    """
    problems = []
    for row in rows:
        slug = row["fixture"]
        status, lsp = row["build_status"], row["lsp_today"]
        declared = (row.get("divergence") or "-").strip()
        # A note must be stamped with the measurement it was written against,
        # and the stamp must still hold. Checked FIRST, above every branch that
        # a note could settle: an expired reason is not a reason about this
        # row, and a hand-edited note with no stamp must not slip past the
        # generator's expiry by being typed straight into the file.
        if declared != "-":
            stamp = (row.get("divergence_for") or "-").strip()
            now = divergence_state(row.get("verdict", ""), lsp)
            if stamp != now:
                problems.append(
                    f"{slug}: the divergence note is stamped for {stamp!r} but "
                    f"this row now measures {now!r}. A reason written against a "
                    f"different measurement is not evidence about this one, "
                    f"however plausible it still reads -- rewrite it against "
                    f"what is measured now, or delete it.")
                continue

        if status in ("NOT-RUN", "BUILD-ERR"):
            # Settled by a stamped note, like every other row. B129's rule is
            # that an artifact which cannot be regenerated is a FINDING and is
            # reported on every run -- reported, which a recorded reason does.
            # Refusing it forever instead would leave the artifact permanently
            # red for a fact about the fixture that nobody can change from
            # here, and a gate that can never go green is one that gets
            # ignored. What must not happen is the row reading as an ANSWER,
            # and `verdict=UNKNOWN` plus a written reason is the opposite.
            if declared != "-":
                continue
            problems.append(
                f"{slug}: the engine verdict is {status} -- the probe could "
                f"not measure it, so this row is not evidence of anything. "
                f"Record in `divergence` why it cannot be probed, or remove "
                f"the row; do not leave it recorded as though it were an "
                f"answer.")
            continue
        if lsp == "UNMEASURED":
            problems.append(
                f"{slug}: this LSP's answer was not measured, so the row "
                f"cannot say whether it agrees with the engine.")
            continue
        # B145. Compared on "did this side say ANYTHING about the code", not
        # on error polarity. The engine is a compiler and this LSP is also a
        # linter, so a fixture the engine compiles WITH A WARNING and the LSP
        # flags is agreement, not a gap -- and treating it as a gap was
        # manufacturing divergences the first run of this recorder reported.
        # `verdict` carries that distinction; `build_status` deliberately does
        # not, because B118's four values are about errors.
        engine_silent = row.get("verdict") == "ACCEPT"
        agrees = engine_silent == (lsp == "CLEAN")

        # AGREEING ON POLARITY IS NOT ANSWERING THE SAME QUESTION. When the
        # engine rejected the unit at DECLARATION time it compiled no function
        # bodies, so its verdict covers the declarations and nothing else --
        # while the LSP's diagnostics may be entirely about call sites the
        # engine never reached. Both said something; that is all polarity sees.
        #
        # Measured on b102-array-sort-mismatched-callbacks: `int &inout` at
        # line 21 is not declarable, the engine emitted two errors there and
        # compiled ZERO bodies, and so never verdicted one of the twelve Sort
        # calls the fixture exists to measure. Recorded REJECTED beside
        # DIAGNOSES(12), it read as agreement.
        #
        # A note is REQUIRED rather than the row refused, because for a fixture
        # whose subject IS a declaration -- a duplicate function, an
        # unimplemented interface, a reserved keyword -- a declaration-time
        # rejection is the complete and correct answer, and four of the five
        # rows in this state are exactly that. Which one this is cannot be
        # derived from the log. Make the question unavoidable; do not infer the
        # answer.
        needs_note = (
            agrees
            and status == "REJECTED"
            and str(row.get("engine_compiled", "")) == "0"
            and lsp != "CLEAN"
        )

        if needs_note and declared == "-":
            problems.append(
                f"{slug}: the engine rejected this unit at DECLARATION time "
                f"(0 function bodies compiled) and this LSP reports {lsp}. "
                f"They agree on polarity, which is not evidence they are "
                f"answering the same question -- the engine never reached a "
                f"call site. Record in `divergence` either that the fixture's "
                f"subject IS the declaration, so the engine's verdict is "
                f"complete, or that the engine never verdicted what this "
                f"fixture exists to measure.")
        elif agrees and not needs_note and declared != "-":
            problems.append(
                f"{slug}: a divergence is declared ({declared!r}) but the "
                f"engine ({status}) and this LSP ({lsp}) now AGREE. A stale "
                f"divergence note is how a fixed gap keeps being described as "
                f"an open one -- delete it.")
        elif not agrees and declared == "-":
            problems.append(
                f"{slug}: the engine says {status} and this LSP says {lsp}, "
                f"and no divergence is declared. Either the LSP has a gap "
                f"worth naming in the `divergence` column, or one of the two "
                f"has drifted since this row was written.")
    return problems


def write_tsv(path: Path, rows: list[dict], engine_version: str,
              probe_date: str, binary_sha: str | None) -> None:
    preamble = [
        "# B145 ISSUE-REPRO ENGINE VERDICTS - what the GAME compiler says about",
        "# each fixture in tests/fixtures/issue-repros/, beside what THIS LSP",
        "# says about the same bytes.",
        "#",
        "# Registered in docs/probes/MANIFEST.tsv. Regenerate with the",
        "# generator named there; it needs Trackmania + Openplanet with",
        "# RemoteBuild, and it stages/loads/unloads each fixture in turn.",
        "#",
        "# COLUMNS",
        "#   fixture         directory under tests/fixtures/issue-repros/.",
        "#   verdict         ACCEPT / REJECT / UNKNOWN - the engine's polarity.",
        "#   build_status    B118's four values. CLEAN and REJECTED are",
        "#                   answers; NOT-RUN and BUILD-ERR are the probe",
        "#                   saying it could not look, and are never recorded",
        "#                   as agreement.",
        "#   engine_compiled how many `Compiling <sig>` lines the engine logged",
        "#                   for THIS folder. Read it ONLY beside a REJECTED",
        "#                   row, where 0 means the unit was rejected at",
        "#                   DECLARATION time and the engine never reached a",
        "#                   call site -- so its verdict covers the",
        "#                   declarations and nothing else. On a CLEAN row 0",
        "#                   carries no such meaning: a clean compile usually",
        "#                   logs no per-function lines at all (15 of 20 clean",
        "#                   rows here are 0). Do not read a clean 0 as",
        "#                   'nothing was compiled'.",
        "#   engine_message  the engine's text, verbatim to the right of the",
        "#                   colon. The staging folder id is normalised away -",
        "#                   it is an artefact of the probe, not of the fixture",
        "#                   - so `Main.as(21,25): <msg>`. Multiple errors are",
        "#                   joined with ` | `.",
        "#   lsp_today       what this repo's CLI reports for the same fixture,",
        f"#                   run as `check {' '.join(LSP_TSV_FLAGS)} <fixture>`.",
        "#   engine_version  PER ROW, not only per file: the repo already holds",
        "#                   claims from three engine generations, and a",
        "#                   partial re-probe must not silently inherit the",
        "#                   file's stamp.",
        "#   probe_date      when this row was last measured.",
        "#   divergence_for  the `verdict|lsp_today` the note beside it was",
        "#                   written against. A reason is carried across a",
        "#                   regeneration ONLY while that pair still holds.",
        "#                   Prose cannot be verified -- B157 and B168 each",
        "#                   found an audit whose JUSTIFIED row was defended by",
        "#                   a reason that was itself the false claim -- but it",
        "#                   CAN be made to expire when its subject moves. This",
        "#                   artifact needed it: b102's note survived a full",
        "#                   polarity flip and still read as a reason.",
        "#   divergence      why the two columns differ, or `-`. REQUIRED",
        "#                   wherever they differ, and REFUSED where they",
        "#                   agree - so neither an unexplained gap nor a stale",
        "#                   note about a fixed one can sit here quietly.",
        "#",
        f"# LSP binary sha256: {binary_sha or 'UNKNOWN'}",
        f"# Engine: {engine_version}   Probed: {probe_date}",
        "#",
    ]
    lines = preamble + ["\t".join(TSV_COLUMNS)]
    for row in rows:
        lines.append("\t".join(str(row[c]).replace("\t", " ") for c in TSV_COLUMNS))
    payload = "\n".join(lines) + "\n"

    # Publish with one rename in the destination directory. A direct
    # write/truncate destroys the last known-good corpus if the process dies
    # halfway through; a temp elsewhere cannot be renamed atomically across
    # filesystems. Preserve the existing file's mode when replacing it.
    mode = (path.stat().st_mode & 0o777) if path.exists() else 0o644
    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as fh:
            fd = -1
            fh.write(payload)
            fh.flush()
            os.fsync(fh.fileno())
        os.chmod(tmp_path, mode)
        os.replace(tmp_path, path)
    finally:
        if fd >= 0:
            os.close(fd)
        tmp_path.unlink(missing_ok=True)


def main() -> int:
    if os.environ.get("B077_SELFTEST_CHILD"):
        return _selftest_child()

    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("fixture", nargs="?", help="fixture dir name or path")
    ap.add_argument("--all", action="store_true", help="probe every fixture")
    ap.add_argument("--json", help="also write results to this JSON file")
    ap.add_argument("--tsv", help="write the committed B129 corpus (B145). "
                                  "Refuses to write a partial or unmeasured "
                                  "record; see agreement_problems.")
    ap.add_argument("--selftest", action="store_true", help="offline cleanup checks; no game")
    args = ap.parse_args()

    if args.selftest:
        return run_selftest()


    if args.all:
        fixtures = sorted(
            d for d in FIXTURES.iterdir() if d.is_dir() and (d / "info.toml").exists()
        )
    elif args.fixture:
        p = Path(args.fixture)
        fixtures = [p if p.is_dir() else FIXTURES / args.fixture]
    else:
        ap.error("give a fixture dir or --all")

    # Sweep only once we know we are actually going to probe. Sweeping is
    # destructive, and doing it before argument validation meant an invocation
    # with no fixture at all still deleted things on its way to an arg error.
    _install_teardown_handlers()
    swept = sweep_stale(fleet.OPN_PLUGINS)
    if swept:
        # Loud: finding one means a previous run died without unstaging, and
        # anything it measured after that point is suspect.
        print(
            f"WARNING: swept {len(swept)} stranded probe plugin(s) before starting: "
            + ", ".join(swept),
            file=sys.stderr,
        )

    results = []
    for fx in fixtures:
        if not (fx / "info.toml").exists():
            print(f"SKIP {fx.name}: no info.toml")
            continue
        rec = probe(fx)
        results.append(rec)
        if rec.get("skipped"):
            print(f"SKIP {rec['fixture']}: {rec['skipped']}")
            continue
        status = rec.get("build_status") or (
            "CLEAN" if rec.get("compiled_clean") else "?"
        )
        if status == "REJECTED":
            status = f"REJECTED ({len(rec['game_errors'])} ERR)"
        print(f"{rec['fixture']}: game={status}")
        for d in rec["diagnostics"]:
            print(f"    ({d['line']},{d['col']}) {d['sev']}: {d['msg']}")

    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
        print(f"wrote {args.json}")

    if args.tsv:
        rc = write_corpus(Path(args.tsv), results, all_fixtures=args.all)
        if rc:
            return rc

    # Exit non-zero if any probe failed to run (skipped) so CI/callers notice.
    return 1 if any(r.get("skipped") for r in results) else 0


def write_corpus(dest: Path, results: list[dict], *, all_fixtures: bool) -> int:
    """B145. Measure this LSP beside the engine, then commit both.

    Refuses a PARTIAL write. The corpus is registered in B129's manifest with a
    row count the Rust test checks against the file on disk, so a single-fixture
    run overwriting the 64-row artifact would be a silent, total data loss that
    looks exactly like a successful regen. `--tsv` without `--all` is the
    natural way to do that by accident, so it is refused by name.
    """
    if not all_fixtures:
        print("--tsv writes the WHOLE corpus and would truncate it to the "
              "fixtures probed in this run. Use --all --tsv, or --json for a "
              "single fixture.", file=sys.stderr)
        return 2
    version = fleet_engine_version()
    if version is None:
        print("cannot stamp the corpus: no Openplanet version line in the log. "
              "An unstamped engine artifact is one that cannot be diffed "
              "against a later generation, and this repo already holds claims "
              "from three of them (B129).", file=sys.stderr)
        return 2
    binary = lsp_cli_bin()
    sha = _sha256(binary)
    date = time.strftime("%Y-%m-%d")
    previous = read_declared_divergences(dest)
    rows, expired = [], []
    for rec in results:
        lsp = run_lsp_on_fixture(FIXTURES / rec["fixture"])
        # Build the row with NO note first, so its freshly measured verdict and
        # LSP answer are available to test the old note's stamp against.
        probe_row = tsv_row(rec, lsp, version, date, {})
        slug = probe_row["fixture"]
        note, stamp = previous.get(slug, ("", ""))
        now = divergence_state(probe_row["verdict"], probe_row["lsp_today"])
        kept = carry_forward(previous, probe_row)
        if kept:
            probe_row["divergence"], probe_row["divergence_for"] = kept
        elif note:
            # EXPIRED. The measurement this reason was written against no
            # longer holds, so the reason is not evidence about the current
            # one -- however plausible it still reads.
            expired.append(f"{slug}: written for {stamp!r}, now {now!r}\n"
                           f"      dropped note: {note}")
        rows.append(probe_row)
    rows.sort(key=lambda r: r["fixture"])
    if expired:
        print(f"\n{len(expired)} divergence note(s) EXPIRED and were dropped -- "
              f"the state each was written against no longer holds:",
              file=sys.stderr)
        for e in expired:
            print(f"  - {e}", file=sys.stderr)
    problems = agreement_problems(rows)
    if problems:
        print(f"\n{len(problems)} row(s) need attention:", file=sys.stderr)
        for pr in problems:
            print(f"  - {pr}", file=sys.stderr)
        return 1
    write_tsv(dest, rows, version, date, sha)
    print(f"wrote {dest} ({len(rows)} rows, engine {version})")
    return 0


def carry_forward(previous: dict, probe_row: dict) -> tuple[str, str] | None:
    """The note to keep for this row, or None if it has EXPIRED.

    Pure, and split out of `write_corpus` on purpose: that function needs a
    live game, so the carry/expire decision inside it could never be exercised
    offline -- and two mutations that disabled it were duly MISSED. A guard
    that cannot be reached by a test is not a guard, and the remedy is the same
    one the flag classification needed: extract the judgement, hand it
    synthetic inputs, and let the mutation run confirm it bites.
    """
    slug = probe_row["fixture"]
    entry = previous.get(slug)
    if not entry:
        return None
    note, stamp = entry
    if not note or note == "-":
        return None
    now = divergence_state(probe_row["verdict"], probe_row["lsp_today"])
    if stamp != now:
        return None
    return note, now


def read_declared_divergences(dest: Path) -> dict[str, tuple[str, str]]:
    """Hand-written notes from the previous corpus, with the state each was
    written against.

    They are the ONLY field a regeneration cannot re-derive, so dropping them
    wholesale would convert every explained gap into an unexplained one -- and
    the run would still look green, because the notes were what made it green.

    But carrying them UNCONDITIONALLY is how a note outlives its subject, which
    is the B157/B168 defect and which this artifact reproduced within one
    regeneration. So each note comes back with its `divergence_for` stamp and
    the caller drops it unless the new measurement still matches.
    """
    if not dest.is_file():
        return {}
    out: dict[str, tuple[str, str]] = {}
    header = None
    for line in dest.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            continue
        cells = line.split("\t")
        if header is None:
            header = cells
            continue
        row = dict(zip(header, cells))
        note = (row.get("divergence") or "-").strip()
        if row.get("fixture") and note != "-":
            out[row["fixture"]] = (note, (row.get("divergence_for") or "-").strip())
    return out


def fleet_engine_version() -> str | None:
    """The live engine's version, read from the same log the probe reads."""
    m = None
    try:
        text = Path(fleet.OPN_LOG).read_text(errors="replace") \
            if hasattr(fleet, "OPN_LOG") else \
            (fleet.HOME / "OpenplanetNext/Openplanet.log").read_text(errors="replace")
    except Exception:
        return None
    for line in text.splitlines():
        hit = re.search(r"Openplanet:\s+(\d+\.\d+\.\d+)", line)
        if hit:
            m = hit.group(1)
    return m


def _sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


if __name__ == "__main__":
    sys.exit(main())
