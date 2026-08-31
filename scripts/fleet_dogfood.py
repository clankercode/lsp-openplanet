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
    """One RemoteBuild call, reading the LENGTH-PREFIXED reply properly (B097).

    The previous version did a single `recv(65536)` and `json.loads(raw[4:])`.
    That works for a one-frame reply like `get_status` and fails for any reply
    that arrives as two frames: the second frame's own 4-byte header lands
    inside the text handed to `json.loads`, which raises
    `Extra data: line 1 column 40`. Loud, so it never produced a wrong verdict
    — but it masked the far more important defect behind a parse error.

    Note what this does NOT fix, and do not read a working reader as a working
    transport. RemoteBuild's own read loop calls `Json::Parse` on each raw
    socket read and `Net::Socket::ReadRaw` returns at most READ_CAP (64) bytes,
    so any REQUEST over that is unreachable and `load_plugin` (72 bytes with an
    EMPTY id) has never worked. That is a plugin-side fix, in Max's game plugin,
    and it is not ours to make; see B097 and docs/probes/README.md.

    This is the LOW-LEVEL client. Callers that want a plugin loaded or unloaded
    want `game_route`, which picks a transport that can actually carry the
    request.
    """
    sock = None
    try:
        sock = socket.create_connection((RB_HOST, RB_PORT), timeout=timeout)
        sock.sendall(rb_encode(route, data))

        def recv_exactly(n: int) -> bytes:
            buf = b""
            while len(buf) < n:
                chunk = sock.recv(n - len(buf))
                if not chunk:
                    # A short read is reported, never silently returned as
                    # partial JSON: half a reply that happens to parse is the
                    # shape this whole batch keeps finding.
                    raise ConnectionError(
                        f"connection closed after {len(buf)}/{n} bytes")
                buf += chunk
            return buf

        length = int.from_bytes(recv_exactly(4), "little")
        if not 0 < length <= 8 * 1024 * 1024:
            return {"error": f"rb_route {route}: implausible frame length {length}",
                    "data": ""}
        return json.loads(recv_exactly(length).decode(errors="replace"))
    except Exception as e:
        return {"error": f"rb_route {route} failed: {e}", "data": ""}
    finally:
        if sock is not None:
            sock.close()

def rb_alive() -> bool:
    return rb_route("get_status", {}).get("data") == "Alive"

def rb_encode(route: str, data: dict) -> bytes:
    """THE request encoding. One function, because the size test below has to
    measure the bytes that actually go out.

    Compact separators are not cosmetic here: they are free bytes against a
    64-byte ceiling, and Python's default `", "` / `": "` spends 9 of them on a
    minimal `load_plugin`. It is not enough to rescue that call -- the compact
    minimum is 72 with an EMPTY id, so no plugin id is short enough and it has
    never been reachable -- but it is enough to keep shorter routes on the
    working transport instead of detouring.
    """
    return json.dumps({"route": route, "data": data},
                      separators=(",", ":")).encode()


# ---- the wire's own limit, and the transport that gets past it -------------
#
# HOISTED from probe_export_binding.py (B097). There were two readers of one
# wire format in this repo and only one of them was right -- the same "a rule
# living at one of N call sites" shape this batch keeps finding. There is now
# one, here, and the probes import it.

# `Net::Socket::ReadRaw` returns at most this many bytes per call. MEASURED off
# the game's own `Read payload:` trace rather than inferred from whether a
# request parsed -- an instrument that reports the thing asked about beats one
# that reports a consequence of it. An earlier "pad until it stops parsing"
# method produced 66, which was the framing bytes, not the cap.
READ_CAP = 64

CONTROL_MCP = HOME / "src/openplanet/my-plugins/tm-control-mcp"

# RemoteBuild routes that `tm-control-mcp` implements, and its name for each.
# Only routes with real parity: a route absent here is NOT silently dropped,
# it falls through to RemoteBuild and fails honestly if it is too long.
CONTROL_MCP_ACTIONS = {
    "load_plugin": "load",
    "unload_plugin": "unload",
    "reload_plugin": "reload",
}


def rb_request_size(route: str, data: dict) -> int:
    """Bytes RemoteBuild would have to read for this request, in one call.

    Measured through `rb_encode`, the same function that does the sending, so
    the size test and the wire can never disagree. A size predicate that
    serialises the request its OWN way is a check on a request nobody sends.
    """
    return len(rb_encode(route, data))


_RB_REASSEMBLES: bool | None = None


def rb_reassembles(force: bool = False) -> bool:
    """Does RemoteBuild reassemble a request LARGER than one `ReadRaw`?

    B096. This is a LIVE round trip, cached per process, and it replaces an
    arithmetic inference that B097 invalidated.

    The old test was `request size <= READ_CAP`. That premise is still true —
    `Net::Socket::ReadRaw` really does return at most 64 bytes per call — but
    B097 fixed the game-side handler to DRAIN on `Available()` instead of
    parsing each read, so size no longer decides whether a request arrives.
    `game_route`'s docstring promised it would "start using RemoteBuild again
    with no edit" the day the read loop was fixed; it could not, because the
    quantity it branches on is not the quantity the fix changed.

    The probe is `get_status` padded past READ_CAP, chosen because it has NO
    side effect. A truncated payload yields the router's empty-route
    `Not implemented:`; a reassembled one yields `Alive`. Two different texts
    from two different causes, which is the discrimination the old wire could
    not make.
    """
    global _RB_REASSEMBLES
    if _RB_REASSEMBLES is not None and not force:
        return _RB_REASSEMBLES
    pad = "A" * (READ_CAP * 2)
    reply = rb_route("get_status", {"p": pad})
    _RB_REASSEMBLES = reply.get("data") == "Alive" and not reply.get("error")
    return _RB_REASSEMBLES


def rb_can_carry(route: str, data: dict) -> bool:
    """Can RemoteBuild receive this request AT ALL?

    Not "will it succeed" -- whether the bytes can reach the router in one
    `ReadRaw`. A `False` here is why `{"error":"Not implemented: "}` comes back
    with an EMPTY route name: neither fragment parsed, so the router was asked
    about route `""`. That message sends readers off to check route SPELLINGS,
    which is how two sessions lost a day to it.

    Corollary worth stating where it can be read: a >READ_CAP probe of ANY
    route answers `Not implemented`, including a deliberately bogus one. So a
    bogus-route negative control fails through the SAME channel as the
    positives and discriminates nothing. No route may be called unimplemented
    on that evidence.
    """
    if rb_request_size(route, data) <= READ_CAP:
        return True
    # B096: over the cap is no longer decisive — ask the wire, once.
    return rb_reassembles()


def measure_read_cap(trials: int = 1) -> int | None:
    """Measure ReadRaw's per-call cap from the game log. Returns None if unclear.

    This is deliberately NOT "pad a request until it stops parsing", which is
    how an earlier version of this file got the wrong number. That method
    measures whether a particular payload survives being cut, not where the cut
    falls: padding `{"route":"get_status","data":{"p":"AAA..."}}` to 65 and 66
    bytes leaves the first 64 ending in `"}` and `"` respectively, still
    lenient-parseable with `route` intact, so both answered `Alive` and the
    boundary read as 66/67. The cut was at 64 the whole time.

    The game traces `Read payload:` verbatim per read, so the length of the
    FIRST read is the cap, directly. An instrument that reports the thing
    asked about beats one that reports a consequence of it.
    """
    log = Path(OP_LOG)
    try:
        mark = log.stat().st_size
    except OSError:
        return None
    payload = '{"route":"get_status","data":{"p":"' + "A" * 100 + '"}}'
    try:
        sock = socket.create_connection((RB_HOST, RB_PORT), timeout=8)
    except OSError:
        return None
    try:
        sock.sendall(payload.encode())
        time.sleep(3.0)
    finally:
        sock.close()
    try:
        with open(log, "rb") as handle:
            handle.seek(mark)
            window = handle.read().decode(errors="replace")
    except OSError:
        return None
    reads = [
        line.split("Read payload: ", 1)[1]
        for line in window.splitlines()
        if "Read payload: " in line
    ]
    return len(reads[0]) if reads else None


def control_mcp(action: str, timeout: float = 120.0, **fields) -> dict:
    """Drive the plugin through `tm-control-mcp`, which reassembles its socket.

    It has RemoteBuild parity (load/unload/reload/enable/disable/getLogs/
    ListPlugins/GetPlugin) plus one property that matters more here than
    convenience: it returns the compile log INLINE, attributed to the build
    THIS call triggered. Collecting diagnostics by severity out of the shared
    `Openplanet.log` picks up whatever another session happened to load in the
    same second, which is a contamination channel in its own right.
    """
    payload = json.dumps({"action": action, **fields}, separators=(",", ":"))
    try:
        proc = subprocess.run(
            [sys.executable, "tools/call.py", "ControlPlugin", payload],
            cwd=CONTROL_MCP, capture_output=True, text=True, timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return {"error": f"tm-control-mcp could not be run: {exc}", "data": ""}
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            envelope = json.loads(line)
        except json.JSONDecodeError:
            continue
        # B096: a TRANSPORT failure is reported by the tool as a top-level
        # `{"ok": false, "error": ...}` envelope with no `data` at all, and
        # it exits non-zero. Both were ignored here, so
        # `Could not reach TM Control MCP ... Connection refused` came back
        # as `{"error": "", "data": {}}` — SUCCESS-SHAPED. That defeated the
        # caller that exists precisely to catch it: probe_export_binding's
        # preflight bails on `probe["error"]`, which was always empty, so a
        # dead transport preflighted as healthy and every variant then came
        # back AMBIGUOUS with an empty log window. Measured, not theorised:
        # rc=4 with that envelope while the plugin was not listening.
        if envelope.get("ok") is False:
            return {"error": envelope.get("error") or "tm-control-mcp call failed",
                    "data": "", "result": {}}
        result = envelope.get("data", {}).get("result", {})
        if result.get("success") is False:
            return {"error": result.get("error") or result.get("code") or "failed",
                    "data": "", "result": result}
        return {"error": "", "data": result, "result": result}
    # No JSON line at all, or a non-zero exit with nothing parseable: that is
    # a failure, not an empty success.
    if proc.returncode != 0:
        return {"error": f"tm-control-mcp exited {proc.returncode}: "
                         f"{(proc.stderr or proc.stdout or '').strip()[:200]}",
                "data": "", "result": {}}
    return {"error": f"no JSON reply from tm-control-mcp: "
                     f"{proc.stdout[-300:]}{proc.stderr[-300:]}", "data": ""}


def game_route(route: str, data: dict, timeout: float = 20.0) -> dict:
    """One game control call, over whichever transport can carry it.

    Deliberately the SAME signature and reply shape as `rb_route`, so it drops
    into every existing call site and into the `rb_route_fn` seam the probes
    already inject through.

    The choice is made on a MEASURED property of the request -- its size against
    READ_CAP -- not on a flag or a preference. RemoteBuild stays the transport
    for everything it can actually receive, so the day its read loop is fixed
    this function starts using it again with no edit. And when RemoteBuild
    cannot carry the request and `tm-control-mcp` has no parity action for the
    route, this does NOT quietly succeed: it returns the error naming the size,
    because a silent fallback to a transport that cannot deliver is how a probe
    reports on a plugin the game never compiled.
    """
    if rb_can_carry(route, data):
        return rb_route(route, data, timeout=timeout)
    action = CONTROL_MCP_ACTIONS.get(route)
    if action is None:
        return {
            "error": (
                f"`{route}` is {rb_request_size(route, data)} bytes and "
                f"RemoteBuild reads at most {READ_CAP} per call, so the request "
                f"cannot reach the router; tm-control-mcp has no parity action "
                f"for it either. B097."
            ),
            "data": "",
        }
    fields = {k: v for k, v in data.items() if k == "id"}
    return control_mcp(action, timeout=timeout, **fields)


def game_alive() -> bool:
    """Is SOME transport able to drive the game?

    `rb_alive` cannot answer this. `get_status` is ~32 bytes, so it fits inside
    READ_CAP and returns `Alive` on an install where every route that matters is
    unreachable -- a liveness check that does not exercise the failing path.
    """
    if rb_alive():
        return True
    return not control_mcp("getLogs", id="tm-control-mcp", maxLines=1)["error"]


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
    """TEST-ONLY (B128). Nothing in production calls this — use
    `read_log_window_raw`.

    THIS IS THE FUNCTION WHOSE SEVERITY FILTER CAUSED B118's THREE FALSE-GREEN
    CHANNELS, and it survives only because two mutation harnesses need the name
    to exist. Its plausible spelling is the hazard: reach for the
    obvious-sounding reader and the channels come back.

    What the filter costs, concretely: it drops every `[ TRAC]` line, and BOTH
    build markers -- `Starting build for "<id>"` and `Loaded plugin '<id>'` --
    are TRAC. So `build_slice` returns ZERO lines from a filtered window, and
    no build-health check can be written on top of one. A consumer reading this
    can see whether anything COMPLAINED, never whether the build HAPPENED,
    which made "never ran" and "ran clean" the same observation on questions
    whose subject was acceptance.

    Attribution was never the problem: `parse_game_diagnostics` reads only
    lines with a source location, the filter keeps every line containing "ERR",
    so both readers yield the same ERR set. That equivalence is why B123's
    migration could not change a corpus row -- and it is also why this function
    looks harmless. It is harmless for attribution and disqualifying for
    everything else.

    WHY IT IS NOT DELETED. `check_probe_cleanup_nonvacuous.py` and
    `check_enum_probe_nonvacuous.py` each carry a mutation that reverts a call
    site TO this reader, to prove the raw default is load-bearing. Delete the
    function and those mutations still get scored as CAUGHT -- by an
    AttributeError at import, not by the guard they exist to exercise. The
    harnesses would keep reporting a full score while one mutation had silently
    stopped testing reader semantics. That is a worse outcome than a dead
    function: a loud break would be fine, a quiet downgrade is not.

    `test_only_reader_has_no_production_callers` in `run_selftest` holds the
    "no production callers" half, so this docstring cannot drift into a lie.
    """
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

# The ONE definition of "this log line carries a source location". Shared by
# `parse_game_diagnostics` (which needs the captures) and `raw_build_errors`
# (which needs the negation). Two copies of this pattern is how a located
# error could be counted twice and a location-less one not at all.
_LOCATED_DIAG = re.compile(r"\((\d+),\s*(\d+)\)\s*:\s*(ERR|WARN|INFO)\s*:\s*(.*)$")


def read_log_window_raw(mark: int, _plugin_folder: str | None = None) -> str:
    """Everything the log gained since `mark`, UNFILTERED (B118).

    `read_log_window` above keeps only what looked compile-relevant to the
    question it was written for, and drops every `[ TRAC]` line. But
    `Starting build for "<id>"` and `Loaded plugin '<id>'` are BOTH TRAC, so a
    consumer of the filtered window cannot see whether its build ran at all --
    it can only see whether anything complained. That is the difference
    between "clean" and "never happened", and the filter erases it.

    Two probes had already worked around this privately
    (`probe_export_binding.read_raw_window`, and B096 claim 2 injecting through
    the `read_log_window_fn` seam) before it was recognised as one defect. This
    is the shared primitive, so the third lane does not have to rediscover it.

    Signature is deliberately `(mark, plugin_folder)`-compatible with
    `read_log_window` so it drops into the same injection seam.
    """
    try:
        with open(OP_LOG, "rb") as f:
            f.seek(mark)
            return f.read().decode(errors="replace")
    except Exception as e:
        return f"<log read failed: {e}>"


BUILD_START = 'Starting build for "%s"'
BUILD_LOADED = "Loaded plugin '%s'"
BUILD_FAILED = "Script compilation failed!"


def build_slice(window: str, fid: str) -> list[str]:
    """The log lines of ONE build, bracketed by its own start/finish markers.

    RemoteBuild serialises builds and brackets each with
    `Starting build for "<fid>"` ... `Loaded plugin '<fid>'`, so the slice
    belongs to that build and to nothing else. Slicing by MARKER rather than by
    severity or by time is what makes this attribution instead of a guess: a
    concurrent session's plugin cannot appear between another build's own two
    markers.

    The slice deliberately ENDS at the load line. Everything after it is
    runtime, and a compile question that reads runtime errors as compile
    errors is a different false verdict.

    Returns [] when the build never started -- which callers MUST distinguish
    from "started and produced nothing", because that is the whole defect.
    """
    lines = window.splitlines()
    start = next((i for i, l in enumerate(lines) if BUILD_START % fid in l), None)
    if start is None:
        return []
    marker = BUILD_LOADED % fid
    end = next((i for i, l in enumerate(lines[start:], start) if marker in l),
               len(lines) - 1)
    return lines[start:end + 1]


def raw_build_errors(window: str, fid: str) -> list[str]:
    """Build `[ERROR]` lines carrying NO source location (B118).

    `parse_game_diagnostics` matches only `path (line, col) : SEV : msg`, so an
    engine error without a location matches nothing and is invisible to EVERY
    bucket derived from it -- `game_errors`, `foreign_errors` and
    `unattributed_errors` alike -- which leaves `contended` False as well. No
    guard anywhere fires. Measured shape:

        [ERROR] Couldn't find required plugin dependency "x" while loading
                plugin "<fid>"
        [ TRAC] Loaded plugin '<fid>' (version 0.1.0)

    Under the pre-B118 oracle that scored a clean ACCEPT.

    `Script compilation failed!` is excluded here and reported separately: it
    is a SUMMARY of errors reported above it, so counting it as an error of its
    own would double-count a located rejection and, worse, would make a genuine
    REJECT indistinguishable from a location-less build failure.
    """
    return [
        l.strip() for l in build_slice(window, fid)
        if "[ERROR]" in l
        and BUILD_LOADED % fid not in l
        and BUILD_FAILED not in l
        and not _LOCATED_DIAG.search(l)
    ]


def probe_run_problems(rows: list[dict], ambiguous: str = "AMBIGUOUS") -> list[str]:
    """Is this probe RUN evidence at all? (B139)

    `build_verdict` answers the per-row question -- did THIS build happen. This
    answers the run-level one, which every probe had been hand-rolling as

        bad = [r for r in rows if r["expect"] != "UNKNOWN"
               and r["verdict"] != r["expect"]]
        return 1 if bad else 0

    and that shape exits 0 for a run that measured NOTHING. Two ways:

    * `--only <subset>` can select rows that carry no control. `bad` is then
      computed over an EMPTY control set, is empty, and the run writes its
      corpus and exits 0 having verified no channel at all. An oracle that was
      never shown to say ACCEPT cannot mean anything by REJECT.
    * the transport can die MIDWAY. Every later row becomes AMBIGUOUS
      ("RemoteBuild not alive"), no control mismatches because controls that
      never ran cannot mismatch, and a full column of AMBIGUOUS is exported as
      the measured answer with a success status.

    THE RULE, and the reason this is not simply "AMBIGUOUS is fatal": refuse on
    the condition that makes the MEASUREMENT meaningless, and report everything
    else as data. A single ambiguous row beside passing controls is a real,
    honest result -- B096 claim 2 shipped two of them, correctly. A run with no
    controls, or with nothing but ambiguity, is not a result at all.
    """
    problems: list[str] = []
    if not rows:
        return ["the run produced no rows at all, so nothing was measured"]

    controls = [r for r in rows if r.get("expect", "UNKNOWN") != "UNKNOWN"]
    if not controls:
        problems.append(
            "this run contains NO control rows, so nothing established that "
            "the channel can produce a verdict at all. An oracle never shown "
            "to say the affirmative cannot mean anything by the negative "
            "(narrow with --only, and the controls can be filtered out with "
            "everything else)."
        )
    for r in controls:
        if r["verdict"] != r["expect"]:
            problems.append(
                f"control {r.get('id', '?')!r} expected {r['expect']} and got "
                f"{r['verdict']}: {str(r.get('reason', ''))[:120]}"
            )

    measured = [r for r in rows if r.get("expect", "UNKNOWN") == "UNKNOWN"]
    if measured and all(r["verdict"] == ambiguous for r in measured):
        problems.append(
            f"every one of the {len(measured)} measured rows came back "
            f"{ambiguous}. That is a dead channel reported as a table, not a "
            f"finding -- an empty result is not a clean result."
        )
    return problems


BUILD_STATUSES = ("CLEAN", "REJECTED", "BUILD-ERR", "NOT-RUN")


def build_verdict(window: str, fid: str, load: dict | None = None) -> dict:
    """Did THIS build succeed? The one place that knows (B118 / B127).

    Every probe needs this answer and each had been deriving it privately, so
    each inherited a different subset of the same three false-green channels.

    THE PROPOSITION THIS ANSWERS IS NOT THE ONE THE TRANSPORT ANSWERS. A reply
    of `{"error": "", "data": "Plugin loaded"}` is truthful and says the plugin
    WAS loaded; it is not a claim that the build succeeded. Measured
    2026-09-01: a plugin whose dependency cannot be resolved gets exactly that
    reply, while the engine logs `Couldn't find required plugin dependency` and
    then `Loaded plugin` anyway. So a guard reading `load["error"]` as build
    health never fires. The two were conflated for an honest reason -- before
    B097 `load_plugin` had never once worked, so the reply was inert and
    carried no information at all. It became load-bearing the day the transport
    started working, and not one line of any consumer changed. **A value that
    was decoration becoming meaningful is a contract change that no diff
    records and no test fails on.**

    FOUR VALUES, NOT A BOOLEAN, because the consumers need different policies
    and a boolean cannot express them:

      CLEAN     built and loaded, nothing attributed to it
      REJECTED  the engine refused THIS code -- a verdict about the source
      BUILD-ERR the build did not happen properly: a location-less engine
                error, a transport error, or a build that started and never
                loaded. NOT a verdict about the source.
      NOT-RUN   no build for this id appears in the window at all

    `REJECTED` and `BUILD-ERR` are deliberately distinct: collapsing them is
    what produced B118, where a missing dependency read as a verdict about the
    code under test. And the split is what lets each caller pick its own
    policy -- `probe_enum_corpus` EXPECTS REJECTED every run, because its
    sentinel is a deliberate error, while a `probe_export_binding` exporter
    must be CLEAN or the whole run says nothing.

    `load` is optional and only ever adds a failure; a caller that has no reply
    to offer loses nothing else.
    """
    diagnostics = parse_game_diagnostics(window)
    errs = [d for d in diagnostics if d["sev"] == "ERR"]
    mine = f"{fid}/"
    game_errors = [d for d in errs if d["file"].startswith(mine)]
    # Never silently dropped: an unattributable ERR means the window is
    # ambiguous and the run should not be trusted.
    foreign_errors = [
        d for d in errs if not d["file"].startswith(mine) and d["file"] != "?"
    ]
    unattributed_errors = [d for d in errs if d["file"] == "?"]

    slice_ = build_slice(window, fid)
    build_ran = bool(slice_)
    build_loaded = any(BUILD_LOADED % fid in l for l in slice_)
    build_failed = any(BUILD_FAILED in l for l in slice_)
    raw_errs = raw_build_errors(window, fid)
    load_error = bool((load or {}).get("error"))

    if not build_ran:
        status = "NOT-RUN"
    elif game_errors or build_failed:
        status = "REJECTED"
    elif raw_errs or load_error or not build_loaded:
        # `not build_loaded` belongs here, not with CLEAN: `Loaded plugin` is
        # logged even AFTER `Script compilation failed!`, so its presence is
        # necessary and not sufficient -- and its ABSENCE, with the build
        # started, means the build never finished.
        status = "BUILD-ERR"
    else:
        status = "CLEAN"

    return {
        "build_status": status,
        "build_ran": build_ran,
        "build_loaded": build_loaded,
        "build_failed": build_failed,
        "raw_build_errors": raw_errs,
        "diagnostics": diagnostics,
        "game_errors": game_errors,
        "foreign_errors": foreign_errors,
        "unattributed_errors": unattributed_errors,
        "contended": bool(foreign_errors or unattributed_errors),
        "compiled_clean": status == "CLEAN",
    }


def parse_game_diagnostics(window: str) -> list[dict]:
    """Extract `path (line, col) : SEV : msg` compile diagnostics from OP log."""
    diags = []
    rx = _LOCATED_DIAG
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
def stage(pdir: Path, fid: str, root: Path | None = None) -> Path:
    dest = (root if root is not None else OPN_PLUGINS) / fid
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
                # B123/B127: the RAW window. The filtered one drops `[ TRAC]`,
                # and both build markers are TRAC, so this consumer could not
                # see whether its build had happened -- while `verdict()` below
                # reads an empty diagnostic list as PARITY-CLEAN. A reload of
                # an already-installed plugin does emit the bracket (verified
                # against Openplanet.log), so gating on it is safe here.
                window = read_log_window_raw(mark, fid)
                bv = build_verdict(window, fid, load)
                rec["game"] = {
                    "staged_by_me": staged_by_me,
                    "load_response": load,
                    "diagnostics": bv["diagnostics"],
                    "build_status": bv["build_status"],
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
    # B127. This must come BEFORE any counting. `gerr == 0` means "the game
    # reported no errors", and an empty diagnostic list satisfies it whether
    # the build was clean or never happened at all -- so a build that did not
    # run would be reported as PARITY-CLEAN, and, worse, as `FP?` when the LSP
    # had findings, manufacturing false-positive evidence AGAINST the LSP out
    # of a build that produced nothing. Silence is this function's affirmative
    # answer, which is the B118 polarity in the tool that generates the parity
    # claims.
    #
    # REJECTED is NORMAL here: finding real compile errors is the point.
    status = game.get("build_status")
    if status in ("NOT-RUN", "BUILD-ERR"):
        return f"lsp={lspn} game=NO-BUILD({status})"
    unverified = "" if status else " build=?"  # record predates B127
    gd = game.get("diagnostics", [])
    gerr = sum(1 for d in gd if d["sev"] == "ERR")
    gwarn = sum(1 for d in gd if d["sev"] == "WARN")
    if lspn == 0 and gerr == 0:
        return f"PARITY-CLEAN (lsp=0 game_err=0 warn={gwarn}){unverified}"
    if (lspn or 0) > 0 and gerr == 0:
        return f"FP? lsp={lspn} game_err=0{unverified}"
    if lspn == 0 and gerr > 0:
        return f"FN? lsp=0 game_err={gerr} warn={gwarn}{unverified}"
    return f"BOTH lsp={lspn} game_err={gerr} warn={gwarn}{unverified}"

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
def run_selftest() -> int:
    """Offline checks for `build_verdict` and `verdict` (B118 / B123 / B127).

    These two decide whether a build succeeded and whether a plugin shows
    PARITY, and neither had a test of its own -- `build_verdict` was covered
    only indirectly through two consumers, and `verdict` not at all, which is
    how it kept reporting PARITY-CLEAN for builds that never happened.

    Every case is a PAIR asserting the two halves DIFFER. A guard that
    condemns every build satisfies a one-sided assertion just as well as a
    correct one, and B118 found four such guards only by mutation.
    """
    failures: list[str] = []

    def check(what, got, want):
        if got != want:
            failures.append(f"{what}\n    got  {got!r}\n    want {want!r}")

    fid = "lsp-selftest-probe"

    def eng(sev, msg):
        return f"[    ScriptEngine] [{sev}] [00:00:00.000] [RemoteBuild]  {msg}"

    plug = f"C:/users/steamuser/OpenplanetNext/Plugins/{fid}/Main.as"
    start = eng(" TRAC", f'Starting build for "{fid}"')
    loaded = eng(" TRAC", f"Loaded plugin '{fid}' (version 0.1.0)")
    located = eng("ERROR", f"{plug} (3, 5) :  ERR : No matching symbol 'Nope'")
    failed = eng("ERROR", "Script compilation failed!")
    # Verbatim in shape from Openplanet.log, 2026-09-01. This is the B127 case.
    locless = eng("ERROR", 'Couldn\'t find required plugin dependency "zzz" '
                           f'while loading plugin "{fid}"')
    foreign = eng("ERROR", "C:/users/steamuser/OpenplanetNext/Plugins/"
                           "other-plugin/Main.as (1, 1) :  ERR : boom")
    ok = {"error": "", "data": "Plugin loaded"}

    clean = build_verdict("\n".join([start, loaded]), fid, ok)
    check("a build that ran and loaded is CLEAN", clean["build_status"], "CLEAN")

    # THE B127 HEADLINE. The transport reply is a clean success -- it is
    # truthful, the plugin WAS loaded -- while the build could not resolve its
    # dependency. Reading the reply as build health is the defect.
    b127 = build_verdict("\n".join([start, locless, loaded]), fid, ok)
    check("B127: a success reply does NOT certify a build whose dependency "
          "could not be resolved", b127["build_status"], "BUILD-ERR")
    check("B127: and it is not mistaken for a verdict about the source",
          b127["build_status"] == "REJECTED", False)
    check("B127: the pair discriminates -- same reply, different build",
          b127["build_status"] != clean["build_status"], True)

    for label, window, load, want in [
        ("the engine refused the source", [start, located, failed, loaded], ok, "REJECTED"),
        # NOT an empty window: Openplanet.log is shared and always carries
        # other traffic, and an empty string lets a broken `build_slice` look
        # correct for free. B118 was caught making exactly this mistake, and
        # writing these fixtures fresh reintroduced it -- so the case is
        # another lane's build, not nothing.
        ("our build absent, another lane's present",
         [eng(" TRAC", 'Starting build for "some-other-lane"'),
          eng(" TRAC", "Loaded plugin 'some-other-lane' (version 2.0)")],
         ok, "NOT-RUN"),
        # `Script compilation failed!` as the ONLY signal. Paired with a
        # located error it never decides anything, because `game_errors`
        # already forces REJECTED -- the summary and its diagnostics can
        # straddle a window boundary for real.
        ("compilation failed, no located diagnostic in the window",
         [start, failed, loaded], ok, "REJECTED"),
        ("build started, never loaded", [start], ok, "BUILD-ERR"),
        ("the transport itself failed", [start, loaded], {"error": "boom"}, "BUILD-ERR"),
    ]:
        got = build_verdict("\n".join(window), fid, load)
        check(f"build_verdict: {label}", got["build_status"], want)
        check(f"build_verdict: {label} differs from CLEAN",
              got["build_status"] != clean["build_status"], True)

    # Attribution is the control that proves the rest is wired: this already
    # worked before B118, and if it stops, every case above measures nothing.
    fgn = build_verdict("\n".join([start, foreign, loaded]), fid, ok)
    check("a foreign plugin's error is not attributed to us", fgn["game_errors"], [])
    check("a foreign plugin's error marks the window contended",
          fgn["contended"], True)
    check("and it does not become a verdict about our build",
          fgn["build_status"], "CLEAN")
    own = build_verdict("\n".join([start, located, loaded]), fid, ok)
    check("our own error IS attributed to us", bool(own["game_errors"]), True)
    check("foreign and own land differently",
          (bool(fgn["game_errors"]), bool(own["game_errors"])), (False, True))

    # --- verdict(): silence must not read as parity ------------------------
    def v(lsp, game):
        return verdict({"lsp": {"count": lsp}, "game": game})

    check("a genuinely clean build reports parity",
          v(0, {"diagnostics": [], "build_status": "CLEAN"}).startswith("PARITY-CLEAN"),
          True)
    for status in ("NOT-RUN", "BUILD-ERR"):
        check(f"a {status} build is NOT reported as parity",
              v(0, {"diagnostics": [], "build_status": status}),
              f"lsp=0 game=NO-BUILD({status})")
    # The worse half: with LSP findings, a build that never ran used to read as
    # `FP?` -- false-positive evidence AGAINST the LSP, manufactured from a
    # build that produced nothing.
    check("a build that never ran does not manufacture FP? against the LSP",
          "FP?" in v(3, {"diagnostics": [], "build_status": "NOT-RUN"}), False)
    check("while a real clean build still can report FP?",
          v(3, {"diagnostics": [], "build_status": "CLEAN"}).startswith("FP?"), True)
    # A record written before B127 has no build_status. It is not evidence that
    # the build ran, so it must not pass as verified.
    check("a pre-B127 record is marked unverified rather than silently trusted",
          v(0, {"diagnostics": []}).endswith("build=?"), True)
    check("and a verified one carries no marker",
          v(0, {"diagnostics": [], "build_status": "CLEAN"}).endswith("build=?"),
          False)

    # --- B139: is a probe RUN evidence at all? -----------------------------
    # Four probes hand-rolled this gate as "did any CONTROL row mismatch",
    # which exits 0 for a run that measured nothing: `--only` can filter the
    # controls out, and a transport that dies midway leaves every later row
    # AMBIGUOUS with no control able to mismatch because none ran.
    #
    # Pairs, as always. The PROCEED half is the one that stops the cure being
    # worse than the disease: a single ambiguous row beside passing controls
    # is an honest result -- B096 claim 2 shipped two of them.
    def row(rid, verdict, expect="UNKNOWN"):
        return {"id": rid, "verdict": verdict, "expect": expect, "reason": ""}

    healthy = [row("c", "ACCEPT", "ACCEPT"), row("a", "ACCEPT"),
               row("b", "AMBIGUOUS")]
    check("a healthy run with one ambiguous row PROCEEDS — refusing here "
          "would break every honest partial result",
          probe_run_problems(healthy), [])
    for label, rows_, want in [
        ("no rows at all", [], "nothing was measured"),
        ("controls filtered out by --only",
         [row("a", "ACCEPT"), row("b", "ACCEPT")], "NO control rows"),
        ("a control mismatched",
         [row("c", "REJECT", "ACCEPT"), row("a", "ACCEPT")], "control 'c'"),
        ("every measured row ambiguous",
         [row("c", "ACCEPT", "ACCEPT"), row("a", "AMBIGUOUS"),
          row("b", "AMBIGUOUS")], "dead channel"),
    ]:
        got = probe_run_problems(rows_)
        if not got:
            check(f"B139 [{label}] is refused", got, ["<a refusal>"])
        elif not any(want in g for g in got):
            check(f"B139 [{label}] names the condition ({want!r})", got, [want])
    # A control that is itself ambiguous must not pass as a working control:
    # it did not demonstrate the channel, it failed to.
    check("an AMBIGUOUS control is a failed control, not a passing one",
          bool(probe_run_problems(
              [row("c", "AMBIGUOUS", "ACCEPT"), row("a", "ACCEPT")])), True)

    # --- B128: this reader must stay test-only ---------------------------
    # A docstring saying "nothing calls this" is a claim that rots the first
    # time somebody reaches for the obvious-sounding name. The grep IS the
    # verification, so it runs here rather than living in the ticket.
    #
    # Mutation harnesses are excluded BY FILENAME: they carry the call inside a
    # string literal on purpose, to prove the raw default is load-bearing.
    # Assembled at runtime so this line does not match ITSELF. The obvious
    # alternative -- skipping fleet_dogfood.py -- would blind the check to the
    # exact file that held B127's worst caller (`probe_one`), so the scan has
    # to cover its own module.
    needle = "fleet.read_log_window" + "("
    scripts_dir = Path(__file__).resolve().parent
    offenders = []
    for path in sorted(scripts_dir.glob("*.py")):
        if path.name.startswith("check_") and path.name.endswith(
                "_nonvacuous.py"):
            continue
        for n, line in enumerate(path.read_text().splitlines(), 1):
            # A CALL, not a stub assignment (`fleet.read_log_window = ...`),
            # which is how the offline selftests revoke the door.
            if needle in line:
                offenders.append(f"{path.name}:{n}: {line.strip()}")
    check("the severity-filtered reader has no production callers (B128) — "
          "it drops [ TRAC], so both build markers are invisible and a build "
          "that never ran reads as one that ran clean",
          offenders, [])

    for f in failures:
        print(f"  - {f}")
    if failures:
        print(f"FAIL: {len(failures)} selftest check(s) failed")
        return 1
    print("PASS: a build that did not happen is never reported as clean.")
    return 0


def main():
    args = sys.argv[1:]
    if args and args[0] == "--selftest":
        sys.exit(run_selftest())
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
