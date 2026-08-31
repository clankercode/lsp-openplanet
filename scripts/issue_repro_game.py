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
import argparse, atexit, json, os, re, shutil, signal, subprocess, sys, tempfile, time
from pathlib import Path

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


def owned_by_us(d: Path) -> bool:
    """Safe to delete: we marked it, or it is empty.

    The empty case is for directories stranded before the marker existed --
    the two this ticket found were empty, and an empty directory declares
    nothing so it cannot have contaminated anything either.
    """
    if (d / OWNER_MARKER).exists():
        return True
    return not any(d.iterdir())


def claim_owner(dest: Path, pid: int | None = None) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    (dest / OWNER_MARKER).write_text(f"{pid if pid is not None else os.getpid()}\n")


def owner_pid(d: Path) -> int | None:
    try:
        return int((d / OWNER_MARKER).read_text().strip())
    except (OSError, ValueError):
        return None


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
    interrupted run cannot leave state (see `staged`); this catches SIGKILL and
    power loss, which nothing in-process can.
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
        if not owned_by_us(d):
            refused.append(f"{name} (not ours: no {OWNER_MARKER} and not empty)")
            continue
        remove_tree(d)
        if d.exists():
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


# Paths this process has staged and not yet removed. Registered BEFORE any
# filesystem work, so a crash midway through copying is still cleaned up: the
# old code assigned `dest` only on success, so a `stage()` that died partway
# left a half-populated directory that the `finally` never saw.
_STAGED: set[Path] = set()


def remove_tree(d: Path, attempts: int = 3) -> bool:
    """Remove `d`, retrying, and REPORT whether it worked.

    `shutil.rmtree(ignore_errors=True)` swallows a permission error or a file
    the game still holds open, so a failed teardown looked identical to a
    successful one -- the exact silent-success shape this batch keeps finding.
    """
    for i in range(attempts):
        shutil.rmtree(d, ignore_errors=True)
        if not d.exists():
            return True
        if i + 1 < attempts:
            time.sleep(0.5)
    return not d.exists()


def _cleanup_staged(*_args) -> None:
    """Emergency unstage. REPORTS anything it could not remove.

    This is the one teardown path with no caller left to inspect a return
    value -- the handler runs and the process dies -- so stderr is the only
    channel by which the operator learns that a fixture is still live in their
    game. A silent failure here is the same shape as the `ignore_errors=True`
    this commit removed, minus the possibility of noticing.
    """
    stuck = []
    for dest in list(_STAGED):
        if remove_tree(dest):
            _STAGED.discard(dest)
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

    removed = False

    def __init__(self, fixture: Path, fid: str, plugins_dir: Path | None = None):
        self.fixture, self.fid = fixture, fid
        root = plugins_dir if plugins_dir is not None else fleet.OPN_PLUGINS
        self.dest = root / fid

    def __enter__(self) -> Path:
        # Register first. Registering after staging is the bug: the window
        # between "directory exists with files in it" and "we recorded that we
        # made it" is exactly when an interrupted run strands a fixture.
        _STAGED.add(self.dest)
        # Claim ownership BEFORE copying anything, so even a fixture stranded
        # mid-copy is identifiable as ours and safe for a later sweep to
        # remove. An unmarked directory is never deleted unless it is empty.
        try:
            claim_owner(self.dest)
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
            remove_tree(self.dest)
            _STAGED.discard(self.dest)
            raise
        return self.dest

    def __exit__(self, *_exc) -> bool:
        self.removed = remove_tree(self.dest)
        if self.removed:
            _STAGED.discard(self.dest)
        return False


def rb_alive_with_retry(attempts: int = 3, delay: float = 2.0) -> bool:
    """RemoteBuild can wedge briefly (accepts then drops). Retry before
    declaring it dead so a single flaky probe doesn't skip a fixture."""
    for i in range(attempts):
        if fleet.rb_alive():
            return True
        if i + 1 < attempts:
            time.sleep(delay)
    return False


def probe(fixture: Path) -> dict:
    """Stage -> load -> capture -> unload + remove. Returns a result record."""
    info = fleet.read_info(fixture / "info.toml")
    name = info.get("name") or fixture.name
    fid = PROBE_PREFIX + fleet.folder_id(name)
    rec: dict = {"fixture": fixture.name, "folder_id": fid}

    # A stranded fixture from an earlier run declares the very names this
    # probe is about to ask about, so an ACCEPT measured beside one is not
    # evidence. Refuse rather than record it.
    strays = probe_dirs_present(fleet.OPN_PLUGINS, allow={fid})
    if strays:
        rec["skipped"] = (
            f"probe-owned plugin(s) already installed: {', '.join(strays)}. "
            "An ACCEPT measured beside a stranded fixture is not evidence "
            "(B077). Re-run to sweep them, or remove them by hand."
        )
        return rec

    if not rb_alive_with_retry():
        rec["skipped"] = "RemoteBuild not alive"
        return rec

    # Refuse to clobber anything already present (a probe id should never
    # collide with a real install, but check anyway).
    dest_root = fleet.OPN_PLUGINS / fid
    if dest_root.exists() or (fleet.OPN_PLUGINS / f"{fid}.op").exists():
        rec["skipped"] = f"probe id {fid} already present in Plugins dir"
        return rec

    mark = fleet.log_size()
    holder = staged(fixture, fid)
    try:
        with holder:
            try:
                load = fleet.rb_route(
                    "load_plugin", {"id": fid, "source": "user", "type": "folder"},
                    timeout=15,
                )
                time.sleep(2.5)  # let compile finish + log flush
                window = fleet.read_log_window(mark, fid)
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
                fleet.rb_route("unload_plugin", {"id": fid}, timeout=15)
    finally:
        rec["restored"] = holder.removed and not (fleet.OPN_PLUGINS / fid).exists()
        if not rec["restored"]:
            # Recorded AND acted on. A teardown that failed must not read as a
            # successful probe.
            rec["skipped"] = (
                f"could not remove staged fixture {fid}; it is still installed "
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

    if mode == "sigterm":
        with staged(fixture, "lsp-issue-repro-selftest", plugins_dir=root):
            assert (root / "lsp-issue-repro-selftest" / "Main.as").exists()
            os.kill(os.getpid(), signal.SIGTERM)
            time.sleep(5)
        return 0

    if mode == "sigterm-stuck":
        # Stage, make removal impossible, then die by signal. On this path
        # nobody is left to inspect a return value, so stderr is the only
        # channel by which the operator can learn a fixture is still live in
        # their game.
        shutil.rmtree = lambda *a, **k: None
        staged(fixture, "lsp-issue-repro-stuck-child", plugins_dir=root).__enter__()
        os.kill(os.getpid(), signal.SIGTERM)
        time.sleep(5)
        return 0

    # `atexit` mode: enter WITHOUT a `with`, so no `__exit__` will ever run,
    # then exit normally. This is the only path the signal handlers do not
    # cover, and it is why the atexit hook is not redundant with them.
    staged(fixture, "lsp-issue-repro-atexit", plugins_dir=root).__enter__()
    assert (root / "lsp-issue-repro-atexit" / "Main.as").exists()
    sys.exit(3)


def run_selftest() -> int:
    """Offline. No game, no RemoteBuild -- the staging lifetime is filesystem
    work, and that is the part B077 is about."""
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
        real_root = fleet.OPN_PLUGINS
        fleet.OPN_PLUGINS = root
        try:
            (root / "lsp-issue-repro-stranded").mkdir()
            (root / "lsp-issue-repro-stranded" / "Main.as").write_text(
                "namespace ZZZProbeOwnedNamespace { enum E { Foo } }\n"
            )
            rec = probe(fixture)
            check("a stranded fixture makes the probe REFUSE",
                  "lsp-issue-repro-stranded" in rec.get("skipped", ""), True)
            check("and it reports no verdict at all", "compiled_clean" in rec, False)
            shutil.rmtree(root / "lsp-issue-repro-stranded")
            # And with the dir gone it must NOT refuse for that reason -- or the
            # guard would just be a permanent "no" and prove nothing.
            rec = probe(fixture)
            check("with nothing stranded, that refusal does not fire",
                  "already installed" in rec.get("skipped", ""), False)
        finally:
            fleet.OPN_PLUGINS = real_root

        # --- the sweep must NEVER delete something that is not ours --------
        # Prefix matching alone would delete a real plugin that happened to be
        # named `lsp-issue-repro-*`. Deleting a user's plugin is far worse than
        # refusing to run, so ownership needs a marker, not a name.
        impostor = root / "lsp-issue-repro-someones-real-plugin"
        impostor.mkdir()
        (impostor / "Main.as").write_text("void Main() {}\n")
        removed = sweep_stale(root)
        check("sweep refuses an unmarked non-empty dir", removed, [])
        check("and leaves it on disk", impostor.is_dir(), True)
        claim_owner(impostor)
        check("but removes it once it carries our marker", sweep_stale(root),
              ["lsp-issue-repro-someones-real-plugin"])

        # An empty unmarked dir is still safe to remove: it declares nothing,
        # and it is the shape of the two this ticket found.
        (root / "lsp-issue-repro-legacy-empty").mkdir()
        check("an empty legacy dir is swept", sweep_stale(root),
              ["lsp-issue-repro-legacy-empty"])

        # --- a LIVE probe's fixture must survive another process's sweep ----
        # A genuinely OTHER live process. Using our own pid would not test
        # this: the sweep deliberately reclaims our own leftovers, since a
        # directory this process owns and is no longer using is exactly what
        # should be cleaned.
        sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        live = root / "lsp-issue-repro-live-elsewhere"
        claim_owner(live, pid=sleeper.pid)
        (live / "Main.as").write_text("namespace ZZZProbeOwnedNamespace {}\n")
        check("sweep keeps a fixture owned by a live pid", sweep_stale(root), [])
        check("and it is still there", live.is_dir(), True)
        sleeper.kill()
        sleeper.wait()
        claim_owner(live, pid=2**22)  # a pid that cannot exist
        check("but sweeps it once the owner is gone", sweep_stale(root),
              ["lsp-issue-repro-live-elsewhere"])

        # --- a removal that FAILS must not report success ------------------
        stuck = root / "lsp-issue-repro-stuck"
        claim_owner(stuck)
        real_rmtree = shutil.rmtree
        shutil.rmtree = lambda *a, **k: None  # silently does nothing
        try:
            check("remove_tree reports failure instead of swallowing it",
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

        # A normal interpreter exit with something still staged. The signal
        # handlers cannot see this one, so it is what makes the atexit hook
        # load-bearing rather than decorative.
        proc = run_child("atexit")
        check("the atexit child exited normally", proc.returncode, 3)
        check("a normal exit leaves no residue", probe_dirs_present(root), [])

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

    if failures:
        print(f"FAIL: {len(failures)} check(s) failed", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print("PASS: no exit path leaves a probe fixture installed.")
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
