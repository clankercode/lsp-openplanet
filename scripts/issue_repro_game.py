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
"""
from __future__ import annotations
import argparse, json, re, sys, time
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
    dest = None
    try:
        dest = fleet.stage(fixture, fid)
        load = fleet.rb_route(
            "load_plugin", {"id": fid, "source": "user", "type": "folder"}, timeout=15
        )
        time.sleep(2.5)  # let compile finish + log flush
        window = fleet.read_log_window(mark, fid)
        rec["load_response"] = load
        rec["diagnostics"] = fleet.parse_game_diagnostics(window)
        rec["raw_window"] = window[-4000:]
        rec["game_errors"] = [d for d in rec["diagnostics"] if d["sev"] == "ERR"]
        rec["compiled_clean"] = not rec["game_errors"] and not load.get("error")
    finally:
        fleet.rb_route("unload_plugin", {"id": fid}, timeout=15)
        if dest is not None:
            fleet.unstage(dest)
        rec["restored"] = True
    return rec


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("fixture", nargs="?", help="fixture dir name or path")
    ap.add_argument("--all", action="store_true", help="probe every fixture")
    ap.add_argument("--json", help="also write results to this JSON file")
    args = ap.parse_args()

    if args.all:
        fixtures = sorted(
            d for d in FIXTURES.iterdir() if d.is_dir() and (d / "info.toml").exists()
        )
    elif args.fixture:
        p = Path(args.fixture)
        fixtures = [p if p.is_dir() else FIXTURES / args.fixture]
    else:
        ap.error("give a fixture dir or --all")

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
