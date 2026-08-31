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
        if (alive or fleet.rb_alive)():
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
    alive = rb_alive_fn or fleet.rb_alive
    route = rb_route_fn or fleet.rb_route
    log_size = log_size_fn or fleet.log_size
    read_log_window = read_log_window_fn or fleet.read_log_window
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
                rec["diagnostics"] = fleet.parse_game_diagnostics(window)
                rec["raw_window"] = window[-4000:]
                # Attribute errors to THIS probe. Openplanet.log is shared:
                # other sessions load their own plugins into the same game, and
                # the log window is a time slice, not a per-plugin channel.
                # Counting a foreign plugin's ERR as ours turns a game ACCEPT
                # into a false REJECT -- and REJECT is the direction we treat
                # as solid evidence, so it would be encoded into a test as fact.
                errs = [d for d in rec["diagnostics"] if d["sev"] == "ERR"]
                rec["game_errors"] = [
                    d for d in errs if d["file"].startswith(f"{fid}/")
                ]
                # Never silently drop the rest: an unattributable ERR means the
                # window is ambiguous and the run should not be trusted.
                rec["foreign_errors"] = [
                    d for d in errs
                    if not d["file"].startswith(f"{fid}/") and d["file"] != "?"
                ]
                rec["unattributed_errors"] = [d for d in errs if d["file"] == "?"]
                rec["contended"] = bool(
                    rec["foreign_errors"] or rec["unattributed_errors"]
                )
                rec["compiled_clean"] = (
                    not rec["game_errors"] and not load.get("error")
                )
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
    fleet.rb_alive = refuse
    fleet.rb_route = refuse
    fleet.read_log_window = refuse
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

    if failures:
        print(f"FAIL: {len(failures)} check(s) failed", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print("PASS: cleanup, isolation, and fail-closed guards verified.")
    return 0


def main() -> int:
    if os.environ.get("B077_SELFTEST_CHILD"):
        return _selftest_child()

    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("fixture", nargs="?", help="fixture dir name or path")
    ap.add_argument("--all", action="store_true", help="probe every fixture")
    ap.add_argument("--json", help="also write results to this JSON file")
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
        status = "CLEAN" if rec["compiled_clean"] else f"{len(rec['game_errors'])} ERR"
        print(f"{rec['fixture']}: game={status}")
        for d in rec["diagnostics"]:
            print(f"    ({d['line']},{d['col']}) {d['sev']}: {d['msg']}")

    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
        print(f"wrote {args.json}")

    # Exit non-zero if any probe failed to run (skipped) so CI/callers notice.
    return 1 if any(r.get("skipped") for r in results) else 0


if __name__ == "__main__":
    sys.exit(main())
