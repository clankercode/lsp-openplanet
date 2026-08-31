#!/usr/bin/env python3
"""B096 claim 2 — does the engine ACCEPT `shared` interface/enum/funcdef/function?

    scripts/probe_shared_modifiers.py [--tsv out.tsv] [--only <id-prefix>]

Requires Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.

The question
------------

B087 added `is_shared` to InterfaceDecl, EnumDecl, FuncdefDecl and
FunctionDecl. Nothing about what PARSES changed — the parser already consumed
the modifier and threw it away; B087 only stopped discarding it. So no
behaviour regresses whichever way the engine answers. What is unverified is
whether those forms are legal AngelScript-under-Openplanet at all. If some are
not, the LSP records a flag for a form the game rejects — which is exactly how
B086 went wrong: a fixture documenting a shape the engine rejects, later cited
twice as evidence that the shape is valid.

The oracle, and why it is this one
----------------------------------

This is a COMPILE-ONLY question, so the claim-1 oracle (call the function and
watch for `Unbound function called`) does not apply — a rejected declaration
never reaches runtime.

`tm-control-mcp` returns the compile log INLINE, attributed to the build the
call triggered, and that attribution is the whole answer to a compile
question. It is unavailable: the game unloaded it at 23:05:43 as a dependency
cascade when MLHook and MLFeedRaceData were unloaded (`Unloading dependent
plugin "tm-control-mcp" of "MLHook"`), not a crash. Restoring it means loading
three plugins into a game another lane is using.

So this probes over RemoteBuild and reads `Openplanet.log` — but NOT by
severity over a time window, which is the contamination channel that made the
inline oracle attractive in the first place. Every compile diagnostic carries
its own ABSOLUTE SOURCE PATH:

    C:/.../Plugins/<folder>/Main.as (12, 5) : ERR : ...

`fleet.parse_game_diagnostics` extracts `<folder>/<file>.as` per line, and
`issue_repro_game.probe` attributes each ERR to the probe's own unique folder
id, counting anything else as `foreign_errors` / `unattributed_errors` and
setting `contended`. That is per-LINE attribution, equal in strength to the
inline log for this question. What it costs: the slicing is done here rather
than by the transport, so a slicing bug is ours to make, and the run depends
on the log neither rotating nor being locked mid-probe.

One correction to that harness is required here. `fleet.read_log_window`
drops every `[ TRAC]` line, and `Loaded plugin '<id>'` IS a TRAC line — so
`compiled_clean` means "no ERR attributed to me", which is absence of
evidence, not proof the build ran. On a question whose subject is ACCEPTANCE
that is a false-green channel: a build that never happened reads identically
to a clean one. This injects an UNFILTERED window through the
`read_log_window_fn` seam, so ACCEPT requires the positive
`Loaded plugin '<fid>'` and the build bracket `Starting build for "<fid>"`.
Returning raw adds only TRAC/INFO lines, so the ERR-based contention logic is
unchanged.

Contamination channels specific to THIS claim
---------------------------------------------

* B068: one construct per compilation unit. A batched parse abort marks every
  other name in the unit ACCEPTED, which is precisely a false green on a
  question about acceptance. Each variant is its own plugin folder.

* `shared` entities are shared across modules BY NAME. The installed corpus
  already contains two names declared shared in more than one plugin
  (`IInputChange`, `CheckpointIxTime`), so this is live, not theoretical. A
  collision with any of the ~40 loaded plugins yields "Shared type doesn't
  match the original declaration" — a REJECT that is not about the modifier.
  Every declared name and every module name here is uniquely suffixed.

* B077: registration-before-copy staging and the stray sweep, inherited from
  `issue_repro_game.probe`, which refuses to run beside a stranded fixture.

Fixture fidelity — what the shipped code actually looks like
-------------------------------------------------------------

The claim-1 near-miss was a fixture that did not match the plugins it
generalised to. Reading the installed corpus first, `shared` ships along TWO
independent dimensions, and both are probed:

* placement — top-level AND inside a namespace, for all four constructs.
* context — in a file listed under `[script] shared_exports`, AND in an
  ordinary plugin file that is in no export list at all.

Both contexts genuinely ship, so a fixture covering only one would misreport
the other. `Editor/FormatX.as` (`shared string Vec2`) and
`Editor/MathX.as` (`shared float Max`) are in NO export list;
`EppLines/Exports_Shared.as` has its `shared_exports` line commented out
("shared exports disabled until contract frozen") and still carries
`shared interface IGeomLeased`. Meanwhile `ai-api/AiApi_Interfaces.as`,
`map-together/Export_Shared.as`, `dips++-dev/Ex/Shared.as` and
`Editor/Editor/Shared_General.as` are all listed under `shared_exports`.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import fleet_dogfood as fleet  # noqa: E402
import issue_repro_game as repro  # noqa: E402

TOKEN = "B096C2"
NS = "B096C2Ns"


@dataclass
class V:
    id: str
    construct: str
    placement: str          # "top" | "ns"
    context: str            # "plain" | "shared_exports"
    shared: bool
    stands_for: str
    expect: str = "UNKNOWN"  # controls assert; real variants measure
    decl: str = ""
    extra_main: str = ""
    deps: tuple[str, ...] = ()
    fid: str = field(default="", init=False)


def _sym(vid: str) -> str:
    """A name no installed plugin can collide with (see the `shared` channel)."""
    return TOKEN + "_" + vid.replace("-", "_")


def _decl(construct: str, name: str, shared: bool) -> str:
    kw = "shared " if shared else ""
    if construct == "iface":
        return f"{kw}interface {name} {{ void Ping(); }}"
    if construct == "enum":
        return f"{kw}enum {name} {{ {name}_A = 0, {name}_B = 1 }}"
    if construct == "funcdef":
        return f"{kw}funcdef void {name}(int arg);"
    if construct == "func":
        return f'{kw}void {name}(const string &in msg) {{ print(msg); }}'
    raise ValueError(construct)


def _place(decl: str, placement: str) -> str:
    if placement == "top":
        return decl
    return f"namespace {NS} {{\n    {decl}\n}}"


# --- the variant set ---------------------------------------------------------
# 4 constructs x {top-level, namespaced} x {plain file, shared_exports file}.
# Every row names the shipped code it stands in for; a row that cannot name
# one has no business generalising to shipped code.

STANDS_FOR = {
    ("iface", "top", "shared_exports"):
        "ai-api/AiApi_Interfaces.as:12 `shared interface ILlmProvider`"
        " (shared_exports = [... AiApi_Interfaces.as])",
    ("iface", "top", "plain"):
        "EppLines/Exports_Shared.as:6 `shared interface IGeomLeased`"
        " (its shared_exports line is COMMENTED OUT, so the file is in no"
        " export list)",
    ("iface", "ns", "shared_exports"):
        "Editor/Editor/Shared_General.as:8 `shared interface IInvCache`"
        " and dips++-dev/Ex/Shared.as:20 `shared interface IWaiter`",
    ("iface", "ns", "plain"):
        "mcp-tm/McpTM_Types.as:17 `shared interface IToolResult`",
    ("enum", "top", "shared_exports"):
        "map-together/Export_Shared.as:1 `shared enum MapMood`"
        " and Editor/Editor/Shared_General.as:125 `shared enum"
        " BlockPlacementType`",
    ("enum", "top", "plain"):
        "EppLines/Exports_Shared.as:24 `shared enum LeasedDrawContext`"
        " and better-totd-dev/codegen/test/UnitTestMain.as:96",
    ("enum", "ns", "shared_exports"):
        "Editor/Editor/Shared_General.as:2 `shared enum InvPatchType`"
        " and Editor/Editor/MacroblockManip_Shared.as:415",
    ("enum", "ns", "plain"):
        "Editor/Editor.as:591 `shared enum EditorAction` (Editor.as is in"
        " no export list)",
    ("funcdef", "top", "shared_exports"):
        "dips++-dev/Ex/Shared.as:2 `shared funcdef void DPP_TaskCallback`"
        " and SkillpackDemoLib/Shared.as:2",
    ("funcdef", "top", "plain"):
        "Editor/Components/common/UI_Components.as:63"
        " `shared funcdef string EnumToStringF(int)` (in no export list)",
    ("funcdef", "ns", "shared_exports"):
        "Editor/Exports/Callbacks_Shared.as:3-8 `shared funcdef bool"
        " ProcessItem(...)` and tm-control-mcp/ToolPackShared.as:5",
    ("funcdef", "ns", "plain"):
        "SkillpackDemoLib/Shared.as:2-3 shape, namespaced",
    ("func", "top", "shared_exports"):
        "Editor/Exports/Item_MovingItems_Shared.as:58 `shared void"
        " SAnimFunc_SetIx(...)` (Item_MovingItems_Shared.as IS in"
        " shared_exports)",
    ("func", "top", "plain"):
        "Editor/UI/UI_Util.as:13 `shared void NotifyError(const string &in"
        " msg)` and test-draw-dev/UIHelpers.as:31 (neither in an export"
        " list)",
    ("func", "ns", "shared_exports"):
        "Editor/Editor/Math_Shared.as shape (Math_Shared.as IS in"
        " shared_exports)",
    ("func", "ns", "plain"):
        "Editor/FormatX.as:2 `shared string Vec2(vec2 &in v)` and"
        " Editor/MathX.as:18 `shared float Max(uint a, uint b)` — both"
        " namespaced, in no export list",
}


def build_variants() -> list[V]:
    out: list[V] = []

    # --- controls -----------------------------------------------------------
    # Without both of these the run is not evidence: an oracle that cannot say
    # ACCEPT cannot mean anything by REJECT, and vice versa.
    out.append(V(
        id="ctl-pos-plain", construct="none", placement="top", context="plain",
        shared=False, expect="ACCEPT",
        stands_for="positive control: a minimal valid plugin. Proves the "
                   "harness can record an ACCEPT and that the build ran.",
        decl="",
    ))
    out.append(V(
        id="ctl-neg-badtype", construct="none", placement="top",
        context="plain", shared=False, expect="REJECT",
        stands_for="negative control: a type no plugin declares. Proves the "
                   "harness can record a REJECT attributed to its own file — "
                   "without it, 'no errors' cannot be distinguished from "
                   "'never compiled'.",
        decl="",
        extra_main=f"    {TOKEN}_ZZZNoSuchType@ x = null;\n",
    ))
    out.append(V(
        id="ctl-pos-sharedexports", construct="iface", placement="top",
        context="shared_exports", shared=False, expect="ACCEPT",
        stands_for="positive control for the shared_exports plumbing itself: "
                   "a NON-shared decl in a shared_exports file. Isolates the "
                   "`shared` keyword as the only difference from the real "
                   "shared_exports rows.",
    ))

    # --- is the modifier LIVE, or merely tolerated? -------------------------
    # 16 uniform ACCEPTs is also the shape a modifier the parser SKIPS would
    # produce, and "the engine tolerates the token" is a much weaker claim
    # than "the engine implements it". If `shared` is inert, then recording
    # `is_shared` is recording a fiction, which is B086's failure mode again.
    #
    # `shared` types are engine-wide by FULLY-QUALIFIED name, so redeclaring a
    # LOADED plugin's shared type with a different body must fail if the
    # modifier means anything. ghosts-pp is loaded and publishes
    # `Ghosts_PP::IInputChange` via shared_exports = ["GPP_SharedExports_.as"].
    # The pair differs ONLY in the keyword, so it cannot be explained by the
    # name collision alone.
    # `Editor` is a PROVEN-GOOD dependency id: map-together ships
    # `dependencies = [... "Editor"]` and loads. It publishes a TOP-LEVEL
    # `shared enum BlockPlacementType { Normal, Ghost, Free }` from
    # Editor/Editor/Shared_General.as:125, which is listed in Editor's
    # `shared_exports`. Redeclaring it with different enumerators is the
    # mismatch. (The first attempt used `ghosts-pp`, which is the FOLDER name
    # but not the dependency id — the engine answered `Couldn't find required
    # plugin dependency`, so that pair measured nothing about `shared`.)
    collide = ("{kw}enum BlockPlacementType {{ ZZZ%sMismatch }}" % TOKEN)
    out.append(V(
        id="disc-collide-shared", construct="custom", placement="top",
        context="plain", shared=True,
        decl=collide.format(kw="shared "),
        stands_for="discriminator WITHOUT a dependency: redeclares Editor's "
                   "`shared enum BlockPlacementType` with a mismatched body. "
                   "Expected to ACCEPT precisely because nothing of Editor is "
                   "in scope — it is the under-powered case, kept so it "
                   "cannot be mistaken for evidence about `shared`.",
    ))
    out.append(V(
        id="disc-collide-nonshared", construct="custom", placement="top",
        context="plain", shared=False,
        decl=collide.format(kw=""),
        stands_for="paired control for disc-collide-shared: identical source "
                   "minus the `shared` keyword. A non-shared type is "
                   "module-local, so this must ACCEPT — which is what makes "
                   "the keyword, not the name, the cause of any REJECT above.",
        expect="ACCEPT",
    ))

    # The pair above has NO dependency on ghosts-pp, and Openplanet compiles a
    # `shared_exports` file into DEPENDENT plugins rather than into a global
    # engine namespace — so nothing of ghosts-pp is in scope and there is
    # nothing to collide with. Reporting its ACCEPT as "`shared` is inert"
    # would be exactly the claim-1 near-miss again: a fixture that does not
    # match the shape it generalises to. These two add the dependency, which
    # is what actually brings the type into scope.
    out.append(V(
        id="disc-dep-collide-shared", construct="custom", placement="top",
        context="plain", shared=True, deps=("Editor",),
        decl=collide.format(kw="shared "),
        stands_for="discriminator WITH the dependency that puts Editor's "
                   "shared_exports in scope: redeclares the LOADED "
                   "`shared enum BlockPlacementType` with different "
                   "enumerators. REJECT proves `shared` is live and "
                   "cross-module; ACCEPT would suggest the token is inert.",
    ))
    out.append(V(
        id="disc-dep-collide-nonshared", construct="custom", placement="top",
        context="plain", shared=False, deps=("Editor",),
        decl=collide.format(kw=""),
        stands_for="paired control: same dependency, same source, minus the "
                   "`shared` keyword. A non-shared enum is module-local, so "
                   "this isolates the keyword as the cause of any REJECT.",
    ))
    out.append(V(
        id="disc-dep-baseline", construct="custom", placement="top",
        context="plain", shared=False, deps=("Editor",), expect="ACCEPT",
        decl="",
        stands_for="control for the dependency itself: depending on Editor "
                   "with no colliding declaration must ACCEPT, so a REJECT "
                   "above cannot be blamed on the dependency edge.",
    ))

    # --- the matrix ---------------------------------------------------------
    for construct in ("iface", "enum", "funcdef", "func"):
        for placement in ("top", "ns"):
            for context in ("plain", "shared_exports"):
                vid = f"{construct}-{placement}-{context}"
                out.append(V(
                    id=vid, construct=construct, placement=placement,
                    context=context, shared=True,
                    stands_for=STANDS_FOR[(construct, placement, context)],
                ))
    return out


def write_fixture(root: Path, v: V) -> Path:
    """One plugin folder per variant: one construct per compilation unit (B068)."""
    # `probe()` derives the folder id from this name as
    # PROBE_PREFIX + folder_id(name), so the name must NOT already carry the
    # prefix or the staged directory comes out double-prefixed.
    pretty = f"B096c2 {v.id}"
    v.fid = repro.PROBE_PREFIX + fleet.folder_id(pretty)
    plugin = root / v.id
    plugin.mkdir(parents=True)

    if v.construct in ("none", "custom"):
        decl = v.decl
    else:
        decl = _place(_decl(v.construct, _sym(v.id), v.shared), v.placement)

    module = f"{TOKEN}M{v.id.replace('-', '')}"
    info = [
        "[meta]",
        f'name     = "{pretty}"',
        'author   = "clankercode"',
        'category = "Tests"',
        'version  = "0.1.0"',
        "",
        "[script]",
    ]
    if v.deps:
        info.append("dependencies = ["
                    + ", ".join(f'"{d}"' for d in v.deps) + "]")

    main_body = f'    print("{TOKEN} {v.id} ready");\n' + v.extra_main
    if v.context == "shared_exports" and v.construct != "none":
        info.append(f'module = "{module}"')
        info.append('shared_exports = ["Shared.as"]')
        (plugin / "Shared.as").write_text(
            f"// B096 claim 2 probe :: {v.id}\n"
            f"// stands in for: {v.stands_for}\n"
            f"{decl}\n"
        )
        (plugin / "Main.as").write_text(
            f"// B096 claim 2 probe :: {v.id} (entry)\n"
            f"void Main() {{\n{main_body}}}\n"
        )
    else:
        (plugin / "Main.as").write_text(
            f"// B096 claim 2 probe :: {v.id}\n"
            f"// stands in for: {v.stands_for}\n"
            f"{decl}\n\n"
            f"void Main() {{\n{main_body}}}\n"
        )
    (plugin / "info.toml").write_text("\n".join(info) + "\n")
    return plugin


def classify(v: V, rec: dict) -> tuple[str, str]:
    """ACCEPT / REJECT / BUILD-ERR / AMBIGUOUS, with the reason."""
    if rec.get("skipped"):
        return "AMBIGUOUS", rec["skipped"]
    if rec.get("contended"):
        return "AMBIGUOUS", (
            f"log window contended: {len(rec.get('foreign_errors', []))} foreign "
            f"and {len(rec.get('unattributed_errors', []))} unattributed ERR"
        )
    started = rec.get("build_ran", False)
    loaded = rec.get("build_loaded", False)
    errs = rec.get("game_errors", [])
    if errs:
        return "REJECT", "; ".join(
            f"{e['file']}({e['line']},{e['col']}): {e['msg']}" for e in errs[:3]
        )
    if not started:
        return "AMBIGUOUS", (
            "no `Starting build` line for this folder: the build never ran, so "
            "an absence of errors is not an ACCEPT"
        )
    raw = rec.get("raw_build_errors") or []
    if raw:
        # A build error with no source location. NOT reported as REJECT: it is
        # not the construct being refused, and collapsing the two would let
        # "your dependency is missing" be read as "the engine rejects this
        # form" -- which is the citation failure B086 is open for.
        return "BUILD-ERR", "; ".join(
            r.split("]", 3)[-1].strip() for r in raw[:2]
        )
    if not loaded:
        return "AMBIGUOUS", (
            "build started but no `Loaded plugin` line and no attributed ERR"
        )
    return "ACCEPT", "compiled and loaded, no error attributed to this folder"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--json")
    ap.add_argument("--only", help="run only variants whose id starts with this")
    args = ap.parse_args()

    variants = build_variants()
    if args.only:
        variants = [v for v in variants if v.id.startswith(args.only)]
        if not variants:
            print(f"no variant matches {args.only!r}", file=sys.stderr)
            return 2

    if not fleet.game_alive():
        print("game/RemoteBuild not reachable; refusing to report a verdict",
              file=sys.stderr)
        return 3

    strays = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    if strays:
        print(f"stranded probe fixtures present, refusing to run (B077): {strays}",
              file=sys.stderr)
        return 4

    tmp = Path(tempfile.mkdtemp(prefix="b096c2-"))
    rows = []
    try:
        for v in variants:
            fixture = write_fixture(tmp, v)
            # B118 landed the unfiltered window, the build bracket and the
            # BUILD-ERR/REJECT split in the harness itself, so this no longer
            # injects its own reader.
            rec = repro.probe(fixture)
            verdict, reason = classify(v, rec)
            rows.append({
                "id": v.id, "construct": v.construct, "placement": v.placement,
                "context": v.context, "shared": v.shared, "expect": v.expect,
                "verdict": verdict, "reason": reason,
                "folder_id": rec.get("folder_id", ""),
                "restored": rec.get("restored"),
                "stands_for": v.stands_for,
            })
            flag = ""
            if v.expect != "UNKNOWN":
                flag = "  <-- CONTROL OK" if verdict == v.expect else \
                       f"  <-- CONTROL FAILED (expected {v.expect})"
            print(f"{verdict:10s} {v.id:34s} {reason[:80]}{flag}", flush=True)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    left = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    print()
    print(f"B077 sweep: {'clean' if not left else 'STRANDED: ' + repr(left)}")

    problems = fleet.probe_run_problems(rows)
    if problems:
        print("THE RUN IS NOT EVIDENCE:", file=sys.stderr)
        for pr in problems:
            print(f"  - {pr}", file=sys.stderr)

    if args.tsv:
        p = Path(args.tsv)
        p.parent.mkdir(parents=True, exist_ok=True)
        cols = ["id", "construct", "placement", "context", "shared", "expect",
                "verdict", "reason", "stands_for"]
        with p.open("w") as fh:
            fh.write("\t".join(cols) + "\n")
            for r in rows:
                fh.write("\t".join(str(r[c]).replace("\t", " ") for c in cols) + "\n")
        print(f"wrote {p}")
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=2))

    return 1 if (problems or left) else 0


if __name__ == "__main__":
    sys.exit(main())
