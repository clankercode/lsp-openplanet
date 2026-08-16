#!/usr/bin/env python3
"""
fleet_dogfood.py — drive openplanet-lsp dogfooding across ~/src/openplanet/my-plugins.

Per plugin (one iteration):
  1. snapshot state  (installed? running? id resolution)
  2. LSP dogfood     (openplanet-lsp check, typedb + plugins-dir)
  3. stage folder    (only if not already installed; .op installs are skipped)
  4. RemoteBuild load + capture fresh OP-log compile window
  5. unload + restore ONLY what we added (leave-as-found)
  6. record everything into state JSON + human ledger

State persists in fleet_state.json so re-runs pick the NEXT undone plugin.
Usage:
  fleet_dogfood.py next            # process the next undone plugin
  fleet_dogfood.py <plugin-dir>    # process one specific plugin (e.g. tm-buffer-time)
  fleet_dogfood.py status          # show progress table
  fleet_dogfood.py list            # list candidate plugin dirs
"""
from __future__ import annotations
import json, os, re, shutil, socket, subprocess, sys, time
from pathlib import Path

# ---- paths ----------------------------------------------------------------
HOME = Path.home()
MY_PLUGINS = HOME / "src/openplanet/my-plugins"
OPN_PLUGINS = HOME / "OpenplanetNext/Plugins"
OP_DATA_DIR = HOME / ".steam/steam/steamapps/compatdata/2225070/pfx/drive_c/users/steamuser/OpenplanetNext"
OP_LOG = OP_DATA_DIR / "Openplanet.log"
LSP_REPO = HOME / "src/lsp-openplanet"
LSP_BIN = LSP_REPO / "target/release/openplanet-lsp"
TYPEDB = LSP_REPO / "tests/fixtures/typedb"
STATE_FILE = LSP_REPO / "docs/plugin-fleet-ledger.json"
LEDGER_MD = LSP_REPO / "docs/plugin-fleet-ledger.md"
RB_HOST, RB_PORT = "127.0.0.1", 30000

# dirs under my-plugins that are NOT testable plugins
NON_PLUGIN = {
    "skills", "op-tm-api-docs", "vscode-openplanet-angelscript",
    "ai-tm-agent", "tm-assets",
    "tm-spoderzone",  # crashes RemoteBuild (2026-08-16); investigate separately
}

# Allow tests / one-off runs to redirect state away from the production ledger
# via env var so a logic test can never clobber docs/plugin-fleet-ledger.json.
STATE_FILE = Path(os.environ.get("FLEET_STATE_FILE", str(STATE_FILE)))

# ---- RemoteBuild minimal client --------------------------------------------
def rb_route(route: str, data: dict, timeout=8) -> dict:
    try:
        s = socket.create_connection((RB_HOST, RB_PORT), timeout=timeout)
        s.sendall(json.dumps({"route": route, "data": data}).encode())
        time.sleep(0.2)
        raw = s.recv(65536)
        s.close()
        # first 4 bytes are a length header
        txt = raw[4:].decode(errors="replace") if len(raw) > 4 else raw.decode(errors="replace")
        return json.loads(txt)
    except Exception as e:
        return {"error": f"rb_route {route} failed: {e}", "data": ""}

def rb_alive() -> bool:
    return rb_route("get_status", {}).get("data") == "Alive"

# ---- plugin discovery / id resolution --------------------------------------
def plugin_dirs() -> list[Path]:
    out = []
    for d in sorted(MY_PLUGINS.iterdir()):
        if not d.is_dir() or d.name in NON_PLUGIN:
            continue
        if (d / "info.toml").exists():
            out.append(d)
    return out

def read_info(path: Path) -> dict:
    """Very small info.toml reader: name, dependencies, optional_dependencies,
    export_dependencies, category. Good enough for id + dep reporting."""
    info = {"name": None, "dependencies": [], "optional_dependencies": [],
            "export_dependencies": [], "category": None, "module": None}
    try:
        txt = path.read_text(errors="replace")
    except Exception:
        return info
    def arr(key):
        m = re.search(rf'^\s*{key}\s*=\s*\[(.*?)\]', txt, re.M | re.S)
        if not m:
            return []
        return re.findall(r'"([^"]+)"', m.group(1))
    def scalar(key):
        m = re.search(rf'^\s*{key}\s*=\s*"([^"]*)"', txt, re.M)
        return m.group(1) if m else None
    info["name"] = scalar("name")
    info["category"] = scalar("category")
    info["module"] = scalar("module")
    for k in ("dependencies", "optional_dependencies", "export_dependencies"):
        info[k] = arr(k)
    return info

def folder_id(pretty_name: str) -> str:
    # mirror build.sh: tr -d '(),;'"' then lowercase, spaces->dashes
    s = re.sub(r"[(),;'\"+]", "", pretty_name)
    s = s.strip().lower().replace(" ", "-")
    return s

def installed_state(pdir: Path, info: dict) -> dict:
    """Is this plugin already present in OpenplanetNext/Plugins?
    Check folder id, .op filename, and module-name .op/dir."""
    name = info.get("name") or pdir.name
    fid = folder_id(name)
    cands = [fid, pdir.name]
    if info.get("module"):
        cands.append(info["module"])
    present = []
    for c in cands:
        if (OPN_PLUGINS / c).is_dir():
            present.append(f"dir:{c}")
        if (OPN_PLUGINS / f"{c}.op").exists():
            present.append(f"op:{c}.op")
    # also scan for any .op whose basename matches module or name loosely
    return {"installed": bool(present), "how": present, "folder_id": fid,
            "is_op": any(p.startswith("op:") for p in present)}

# ---- OP log window capture --------------------------------------------------
def log_size() -> int:
    try:
        return OP_LOG.stat().st_size
    except Exception:
        return 0

def read_log_window(mark: int, plugin_folder: str) -> str:
    try:
        with open(OP_LOG, "rb") as f:
            f.seek(mark)
            raw = f.read().decode(errors="replace")
    except Exception as e:
        return f"<log read failed: {e}>"
    lines = []
    for ln in raw.splitlines():
        if "[ TRAC]" in ln or "phone-proxy" in ln or "User-Agent" in ln:
            continue
        # keep compile-relevant lines: ERROR/WARN/LOG mentioning the plugin or ScriptEngine
        if ("ScriptEngine" in ln) or (plugin_folder in ln) or ("ERR" in ln) or ("WARN" in ln):
            lines.append(ln)
    return "\n".join(lines)

def parse_game_diagnostics(window: str) -> list[dict]:
    """Extract `path (line, col) : SEV : msg` compile diagnostics from OP log."""
    diags = []
    rx = re.compile(r"\((\d+),\s*(\d+)\)\s*:\s*(ERR|WARN|INFO)\s*:\s*(.*)$")
    for ln in window.splitlines():
        m = rx.search(ln)
        if m:
            sev = m.group(3)
            filem = re.search(r"Plugins/([^/]+/[^ (\]]+\.as)", ln)
            diags.append({
                "file": filem.group(1) if filem else "?",
                "line": int(m.group(1)), "col": int(m.group(2)),
                "sev": sev, "msg": m.group(4).strip(),
            })
    return diags

# ---- LSP -------------------------------------------------------------------
def run_lsp(pdir: Path) -> dict:
    cmd = [str(LSP_BIN), "check", "--typedb-dir", str(TYPEDB),
           "--plugins-dir", str(OPN_PLUGINS), str(pdir)]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        out = (p.stdout + p.stderr)
        return {"exit": p.returncode, "output": out.strip()}
    except Exception as e:
        return {"exit": -1, "output": f"<lsp run failed: {e}>"}

def parse_lsp_count(output: str) -> int | None:
    m = re.search(r"(\d+) diagnostics?", output)
    return int(m.group(1)) if m else None

# ---- staging / restore ------------------------------------------------------
def stage(pdir: Path, fid: str) -> Path:
    dest = OPN_PLUGINS / fid
    dest.mkdir(parents=True, exist_ok=True)
    src = pdir / "src"
    if src.is_dir():
        for item in src.iterdir():
            if item.is_file():
                shutil.copy2(item, dest / item.name)
            elif item.is_dir():
                shutil.copytree(item, dest / item.name, dirs_exist_ok=True)
    else:
        # single-folder plugins: copy *.as at root
        for item in pdir.glob("*.as"):
            shutil.copy2(item, dest / item.name)
    if (pdir / "info.toml").exists():
        shutil.copy2(pdir / "info.toml", dest / "info.toml")
    if (pdir / "fonts").is_dir():
        shutil.copytree(pdir / "fonts", dest / "fonts", dirs_exist_ok=True)
    return dest

def unstage(dest: Path):
    if dest.is_dir():
        shutil.rmtree(dest, ignore_errors=True)

# ---- state ------------------------------------------------------------------
def load_state() -> dict:
    if STATE_FILE.exists():
        try:
            return json.loads(STATE_FILE.read_text())
        except Exception:
            pass
    return {"plugins": {}}

def save_state(st: dict):
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    STATE_FILE.write_text(json.dumps(st, indent=2))

# ---- one iteration -----------------------------------------------------------
def process_plugin(pdir: Path, st: dict) -> dict:
    rec = {"dir": pdir.name, "when": time.strftime("%Y-%m-%d %H:%M:%S")}
    info = read_info(pdir / "info.toml")
    rec["info"] = info
    ist = installed_state(pdir, info)
    rec["initial_state"] = ist

    # 2. LSP dogfood (always; no install needed)
    lsp = run_lsp(pdir)
    rec["lsp"] = {"exit": lsp["exit"], "count": parse_lsp_count(lsp["output"]),
                  "output": lsp["output"]}

    # 3-5. game compile, unless .op-installed (skip staging per user rule)
    if ist["is_op"]:
        rec["game"] = {"skipped": "already installed as .op"}
    else:
        if not rb_alive():
            rec["game"] = {"skipped": "RemoteBuild not alive"}
        else:
            fid = ist["folder_id"]
            staged_by_me = not ist["installed"]
            dest = None
            mark = log_size()
            try:
                if staged_by_me:
                    dest = stage(pdir, fid)
                # `load` on an already-installed plugin is a RELOAD (recompile).
                # Openplanet handles this gracefully; it gives us the compile
                # output we need without changing the plugin's installed state.
                load = rb_route("load_plugin", {"id": fid, "source": "user", "type": "folder"}, timeout=15)
                time.sleep(2.5)  # let compile finish + log flush
                window = read_log_window(mark, fid)
                rec["game"] = {
                    "staged_by_me": staged_by_me,
                    "load_response": load,
                    "diagnostics": parse_game_diagnostics(window),
                    "raw_window": window[-6000:],
                }
            finally:
                # Leave-as-found:
                #  - staged_by_me (was absent): unload + remove -> back to absent.
                #  - already installed (maybe running): do NOT unload. Leave it
                #    exactly as found (installed; running if it was running).
                if staged_by_me:
                    rb_route("unload_plugin", {"id": fid}, timeout=15)
                    if dest is not None:
                        unstage(dest)
                rec["restored"] = True

    st["plugins"][pdir.name] = rec
    save_state(st)
    return rec

# ---- reporting ----------------------------------------------------------------
def verdict(rec: dict) -> str:
    lspn = rec.get("lsp", {}).get("count")
    game = rec.get("game", {})
    if game.get("skipped"):
        return f"lsp={lspn} game=SKIP({game['skipped']})"
    gd = game.get("diagnostics", [])
    gerr = sum(1 for d in gd if d["sev"] == "ERR")
    gwarn = sum(1 for d in gd if d["sev"] == "WARN")
    if lspn == 0 and gerr == 0:
        return f"PARITY-CLEAN (lsp=0 game_err=0 warn={gwarn})"
    if (lspn or 0) > 0 and gerr == 0:
        return f"FP? lsp={lspn} game_err=0"
    if lspn == 0 and gerr > 0:
        return f"FN? lsp=0 game_err={gerr} warn={gwarn}"
    return f"BOTH lsp={lspn} game_err={gerr} warn={gwarn}"

def next_undone(st: dict) -> Path | None:
    done = set(st["plugins"].keys())
    for d in plugin_dirs():
        if d.name not in done:
            return d
    return None

def gap_class(msg: str) -> str:
    """Bucket a game diagnostic message into a dedup class."""
    m = msg
    for pat, cls in [
        (r"No matching symbol '([^']+)'", r"no-symbol:\1"),
        (r"Use of uninitialized global variable '([^']+)'", r"CASCADE-uninit-global:\1"),
        (r"Expression is not an l-value", "l-value"),
        (r"Signed/Unsigned mismatch", "signed-unsigned"),
        (r"No matching signatures to '([^']+)'", r"no-signature:\1"),
        (r"Can't convert '([^']+)' to '([^']+)'", r"convert:\1->\2"),
        (r"Expected '([^']+)'", r"expected:\1"),
        (r"Unexpected token '([^']+)'", r"unexpected-token:\1"),
        (r"Identifier '([^']+)' is not a data type", r"not-a-type:\1"),
        (r"'([^']+)' is not declared", r"not-declared:\1"),
        (r"Deprecated", "deprecated"),
    ]:
        mm = re.search(pat, m)
        if mm:
            return re.sub(pat, cls, m)
    return "other:" + m[:60]

def cmd_sweep(st: dict, limit: int | None = None):
    """Process every undone plugin sequentially; isolate per-plugin failures;
    then emit a dedup'd gap-class summary."""
    done = 0
    for d in plugin_dirs():
        if d.name in st["plugins"]:
            continue
        if limit is not None and done >= limit:
            break
        try:
            rec = process_plugin(d, st)
            print_rec(rec)
        except Exception as e:
            st["plugins"][d.name] = {"dir": d.name, "error": str(e),
                                     "when": time.strftime("%Y-%m-%d %H:%M:%S")}
            save_state(st)
            print(f"\n=== {d.name} ===\n  DRIVER ERROR: {e}")
        done += 1
    # dedup gap summary across all processed
    classes: dict[str, dict] = {}
    for name, rec in st["plugins"].items():
        for gd in rec.get("game", {}).get("diagnostics", []):
            if gd["sev"] not in ("ERR", "WARN"):
                continue
            cls = gap_class(gd["msg"])
            e = classes.setdefault(cls, {"count": 0, "sev": gd["sev"], "examples": []})
            e["count"] += 1
            if len(e["examples"]) < 3:
                e["examples"].append(f"{name}:{gd['file']}:{gd['line']}")
    print("\n\n===== DEDUP GAP CLASSES (game-side, all processed plugins) =====")
    for cls, e in sorted(classes.items(), key=lambda kv: -kv[1]["count"]):
        print(f"  {e['count']:4}x [{e['sev']}] {cls}   e.g. {', '.join(e['examples'])}")

def print_rec(rec: dict):
    print(f"\n=== {rec['dir']} ===")
    ist = rec["initial_state"]
    print(f"  initial: installed={ist['installed']} {ist['how']} id={ist['folder_id']}")
    print(f"  lsp: {rec['lsp'].get('count')} diagnostics (exit {rec['lsp'].get('exit')})")
    g = rec.get("game", {})
    if g.get("skipped"):
        print(f"  game: SKIPPED ({g['skipped']})")
    else:
        print(f"  game diagnostics ({len(g.get('diagnostics', []))}):")
        for d in g.get("diagnostics", []):
            if d["sev"] in ("ERR", "WARN"):
                print(f"    {d['sev']:4} {d['file']}:{d['line']}:{d['col']}  {d['msg']}")
    print(f"  VERDICT: {verdict(rec)}")

# ---- main ---------------------------------------------------------------------
def main():
    args = sys.argv[1:]
    st = load_state()
    if not args or args[0] == "next":
        tgt = next_undone(st)
        if tgt is None:
            print("All plugins processed.")
            return
        print(f"Next undone plugin: {tgt.name}")
        print_rec(process_plugin(tgt, st))
    elif args[0] == "status":
        done = st["plugins"]
        total = len(plugin_dirs())
        print(f"{len(done)}/{total} processed")
        for name, rec in done.items():
            print(f"  {name:40} {verdict(rec)}")
    elif args[0] == "list":
        for d in plugin_dirs():
            mark = "x" if d.name in st["plugins"] else " "
            print(f"  [{mark}] {d.name}")
    elif args[0] == "sweep":
        limit = int(args[1]) if len(args) > 1 else None
        cmd_sweep(st, limit)
    else:
        tgt = MY_PLUGINS / args[0]
        if not tgt.is_dir():
            print(f"no such plugin dir: {tgt}")
            sys.exit(2)
        print_rec(process_plugin(tgt, st))

if __name__ == "__main__":
    main()
