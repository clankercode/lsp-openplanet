#!/usr/bin/env python3
"""B086 — measure how the engine binds a `[script] exports` stub to its body.

    scripts/probe_export_binding.py                  # probe, print a table
    scripts/probe_export_binding.py --tsv out.tsv    # also write the corpus
    scripts/probe_export_binding.py --only exact     # one variant, by id prefix

Requires Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.

Why this exists
---------------

``tests/fixtures/issue-repros/60-exports-correct-pattern`` paired a by-value
stub with a ``const string &in`` body and called it *the CORRECT pattern*. It
was never measured against the engine, and it was cited as evidence twice —
once in a shipped source comment, once by the coordinator approving B062's
matching rule. Both conclusions were wrong, and both were wrong **because the
fixture said so**. A fixture comment asserting engine behaviour is a claim, not
a measurement, and everybody downstream reads it as a measurement.

What is measured, and why it needs a positive control
-----------------------------------------------------

An ``import`` declaration compiles fine on both sides regardless of whether it
matches; the engine binds it when the consumer runs, and an unmatched stub dies
at **runtime** with ``Unbound function called``. So the probe has to *call* the
function, not merely compile it.

The first version of this measurement (lane-b060-b062) had only a mismatched
variant and a control naming a function that does not exist. Both failed
identically, which is an oracle that cannot discriminate — it would have read
as "the engine is strict" without having measured anything, and it would have
read exactly the same way if RemoteBuild had simply not loaded the plugin.
``exact-const-ref-in`` is the positive control that makes the rest evidence:
it differs from ``lax-by-value`` in the parameter type and nothing else.

Shape of a run
--------------

One exporter plugin holds every body and declares every stub in its strict
``[script] exports`` file. One consumer plugin per variant declares a
dependency on it and calls exactly one function, printing a token on success.
Per-variant consumers, rather than one consumer calling all of them, because an
unbound call raises and would abort the rest — and because a per-load log
window attributes the verdict to one variant with nothing to disentangle.

Every verdict is one of:

``BOUND``      the call returned and printed its token.
``UNBOUND``    the call was reached and the engine said ``Unbound function
               called``.
``COMPILE_ERR`` the game rejected the plugin; the diagnostics are recorded.
``AMBIGUOUS``  anything else — the consumer never started, the window is
               contended by another session's errors, RemoteBuild did not
               acknowledge. Never silently a pass: an unreadable run is a
               failure of the probe, not a property of the engine.

Cleanup follows B077: staging goes through ``issue_repro_game.staged``, so no
exit path — exception, SIGINT, SIGTERM, SIGHUP, ``sys.exit`` — can leave a
fixture installed in the game, and a fixture that could not be removed is
reported loudly rather than swallowed.
"""

from __future__ import annotations

import argparse
import json
import socket
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import fleet_dogfood as fleet  # noqa: E402
import issue_repro_game as repro  # noqa: E402

MODULE = "B086Probe"
EXPORTER_ID = repro.PROBE_PREFIX + "b086-exporter"
TOKEN = "B086PROBE"


@dataclass
class Variant:
    """One (stub, body) pair, and what calling it is supposed to show."""

    id: str
    fn: str
    stub: str  # the `import <sig> from "MODULE";` signature, minus `import`
    body: str  # the whole body definition, or "" for the no-body control
    call: str  # AngelScript that calls it and yields a printable `string res`
    why: str
    expect: str  # the reading this variant was designed to discriminate


def V(id, fn, stub, body, call, why, expect):
    return Variant(id, fn, stub, body, call, why, expect)


# Ordered so the two controls come first: if either misreads, nothing after it
# is evidence.
VARIANTS: list[Variant] = [
    V(
        "exact-const-ref-in",
        "Echo_exact",
        'string Echo_exact(const string &in a, const string &in b)',
        'string Echo_exact(const string &in a, const string &in b) { return a + b; }',
        'string res = %(NS)s::Echo_exact("x", "y");',
        "POSITIVE CONTROL: stub and body identical, the shape ai-api ships.",
        "BOUND",
    ),
    V(
        "control-no-body",
        "Echo_missing",
        'string Echo_missing(const string &in a, const string &in b)',
        "",
        'string res = %(NS)s::Echo_missing("x", "y");',
        "NEGATIVE CONTROL: a stub naming a function that does not exist.",
        "UNBOUND",
    ),
    V(
        "lax-by-value-over-const-ref-in",
        "Echo_lax",
        'string Echo_lax(string a, string b)',
        'string Echo_lax(const string &in a, const string &in b) { return a + b; }',
        'string res = %(NS)s::Echo_lax("x", "y");',
        "THE FIXTURE'S SHAPE: by-value stub over a `const &in` body.",
        "UNBOUND",
    ),
    V(
        "exact-by-value",
        "Echo_byval",
        'string Echo_byval(string a, string b)',
        'string Echo_byval(string a, string b) { return a + b; }',
        'string res = %(NS)s::Echo_byval("x", "y");',
        "Discriminates 'by-value stubs never bind' from 'the pair must match'.",
        "BOUND",
    ),
    V(
        "const-ref-in-stub-over-by-value-body",
        "Echo_rev",
        'string Echo_rev(const string &in a, const string &in b)',
        'string Echo_rev(string a, string b) { return a + b; }',
        'string res = %(NS)s::Echo_rev("x", "y");',
        "The reverse mismatch: is the rule symmetric?",
        "UNBOUND",
    ),
    V(
        "const-only-difference",
        "Echo_nonconst",
        'string Echo_nonconst(string &in a)',
        'string Echo_nonconst(const string &in a) { return a; }',
        'string res = %(NS)s::Echo_nonconst("x");',
        "Isolates `const` alone: same reference mode, only the qualifier moves.",
        "UNBOUND",
    ),
    V(
        "exact-out-ref",
        "Fill_out",
        'void Fill_out(string &out a)',
        'void Fill_out(string &out a) { a = "filled"; }',
        'string res; %(NS)s::Fill_out(res);',
        "`&out` matched exactly (CLAUDE.md: vary the reference mode, not just `&in`).",
        "BOUND",
    ),
    V(
        "out-stub-over-const-ref-in-body",
        "Fill_mismatch",
        'void Fill_mismatch(string &out a)',
        'void Fill_mismatch(const string &in a) { print("' + TOKEN + ' body:" + a); }',
        'string res = "unset"; %(NS)s::Fill_mismatch(res);',
        "`&out` stub over a `const &in` body — the reference mode alone differs.",
        "UNBOUND",
    ),
    V(
        "exact-int-out-ref",
        "Fill_int",
        'void Fill_int(int &out a)',
        'void Fill_int(int &out a) { a = 42; }',
        'int n = 0; %(NS)s::Fill_int(n); string res = tostring(n);',
        "A primitive `&out`, in case reference handling differs by type class.",
        "BOUND",
    ),
    V(
        "exact-handle",
        "Ident_handle",
        'Json::Value@ Ident_handle(Json::Value@ v)',
        'Json::Value@ Ident_handle(Json::Value@ v) { return v; }',
        'Json::Value@ j = Json::Parse("{\\"k\\":1}"); string res = Json::Write(%(NS)s::Ident_handle(j));',
        "A handle parameter `T@`, matched exactly.",
        "BOUND",
    ),
    V(
        "handle-stub-over-handle-ref-in-body",
        "Ident_handle_ref",
        'Json::Value@ Ident_handle_ref(Json::Value@ v)',
        'Json::Value@ Ident_handle_ref(Json::Value@ &in v) { return v; }',
        'Json::Value@ j = Json::Parse("{\\"k\\":1}"); string res = Json::Write(%(NS)s::Ident_handle_ref(j));',
        "`T@` stub over a `T@ &in` body — the handle-reference variant.",
        "UNBOUND",
    ),
    V(
        "exact-handle-ref-in",
        "Ident_href",
        'Json::Value@ Ident_href(Json::Value@ &in v)',
        'Json::Value@ Ident_href(Json::Value@ &in v) { return v; }',
        'Json::Value@ j = Json::Parse("{\\"k\\":2}"); string res = Json::Write(%(NS)s::Ident_href(j));',
        "`T@ &in` matched exactly.",
        "BOUND",
    ),
    V(
        "exact-default-arg",
        "Greet_default",
        'string Greet_default(const string &in a, const string &in b = "!")',
        'string Greet_default(const string &in a, const string &in b = "!") { return a + b; }',
        'string res = %(NS)s::Greet_default("hi");',
        "A defaulted parameter present on both sides, called with it omitted.",
        "BOUND",
    ),
    V(
        "default-in-stub-only",
        "Greet_stubdef",
        'string Greet_stubdef(const string &in a, const string &in b = "!")',
        'string Greet_stubdef(const string &in a, const string &in b) { return a + b; }',
        'string res = %(NS)s::Greet_stubdef("hi");',
        "The default exists only in the stub. MEASURED 2026-08-31: BINDS — a "
        "default argument is not part of the binding signature.",
        "BOUND",
    ),
    V(
        "default-in-body-only",
        "Greet_bodydef",
        'string Greet_bodydef(const string &in a, const string &in b)',
        'string Greet_bodydef(const string &in a, const string &in b = "!") { return a + b; }',
        'string res = %(NS)s::Greet_bodydef("hi", "!");',
        "The default exists only in the body. MEASURED 2026-08-31: BINDS, the "
        "same way round.",
        "BOUND",
    ),
    V(
        "arity-mismatch",
        "Echo_arity",
        'string Echo_arity(const string &in a, const string &in b)',
        'string Echo_arity(const string &in a) { return a; }',
        'string res = %(NS)s::Echo_arity("x", "y");',
        "Different parameter COUNT, which B062's rule does compare.",
        "UNBOUND",
    ),
    V(
        "primitive-alias",
        "Add_alias",
        'uint Add_alias(uint a)',
        'uint32 Add_alias(uint32 a) { return a + 1; }',
        'string res = tostring(%(NS)s::Add_alias(41));',
        "Same type under two spellings (`uint`/`uint32`) — the typedef-shaped case.",
        "BOUND",
    ),
    V(
        "return-type-mismatch",
        "Echo_ret",
        'string Echo_ret(const string &in a)',
        'void Echo_ret(const string &in a) { print("' + TOKEN + ' body:" + a); }',
        'string res = %(NS)s::Echo_ret("x");',
        "Parameters match; only the RETURN type differs. MEASURED 2026-08-31: "
        "does NOT bind — the return type is part of the signature.",
        "UNBOUND",
    ),
]


# --------------------------------------------------------------------------
# Building the two plugins
# --------------------------------------------------------------------------


def write_exporter(root: Path, variants: list[Variant]) -> Path:
    """One plugin: every body as a plain global, every stub in the exports file.

    The namespace wrapper is consumer-side only — `ai-api` ships exactly this
    shape, with `namespace AiApi { import ... }` in the export file over plain
    global bodies in `AiApi_Net.as`.
    """
    plugin = root / "exporter"
    (plugin / "src").mkdir(parents=True)
    (plugin / "info.toml").write_text(
        "[meta]\n"
        'name = "B086 Exporter"\n'
        'version = "0.1.0"\n'
        'category = "Library"\n'
        "\n[script]\n"
        f'module = "{MODULE}"\n'
        'exports = ["Exports.as"]\n'
    )
    stubs = "\n".join(
        f"    // {v.id}\n    import {v.stub} from \"{MODULE}\";" for v in variants
    )
    (plugin / "src" / "Exports.as").write_text(
        "// B086 probe: strict export file. Compiles into CONSUMERS only.\n"
        f"namespace {MODULE} {{\n{stubs}\n}}\n"
    )
    bodies = "\n".join(f"// {v.id}\n{v.body}" for v in variants if v.body)
    (plugin / "src" / "Bodies.as").write_text(
        "// B086 probe: the real bodies, plain globals in the defining module.\n"
        f"{bodies}\n"
        "\nvoid Main() {\n"
        f'    print("{TOKEN} exporter ready");\n'
        "}\n"
    )
    return plugin


def write_consumer(root: Path, variant: Variant) -> Path:
    """One plugin per variant: depend on the exporter, call exactly one function."""
    plugin = root / f"consumer-{variant.id}"
    (plugin / "src").mkdir(parents=True)
    (plugin / "info.toml").write_text(
        "[meta]\n"
        f'name = "B086 Consumer {variant.id}"\n'
        'version = "0.1.0"\n'
        "\n[script]\n"
        f'dependencies = ["{EXPORTER_ID}"]\n'
    )
    call = variant.call % {"NS": MODULE}
    (plugin / "src" / "Main.as").write_text(
        f"// B086 probe consumer: {variant.why}\n"
        "void Main() {\n"
        f'    print("{TOKEN} start {variant.id}");\n'
        f"    {call}\n"
        f'    print("{TOKEN} bound {variant.id} res=" + res);\n'
        "}\n"
    )
    return plugin


# --------------------------------------------------------------------------
# Running one variant against the live game
# --------------------------------------------------------------------------


def read_raw_window(mark: int) -> str:
    """Everything the log gained since `mark`, unfiltered.

    `fleet.read_log_window` drops `[ TRAC]` lines and keeps only what looks
    compile-relevant. This probe's evidence is a `print()` from a plugin and a
    runtime exception, so it reads the raw tail instead — a filter tuned for
    another question is how a probe ends up unable to see its own answer.

    B118 promoted that reasoning into `fleet.read_log_window_raw`, after the
    same filter cost a THIRD probe its answer. This now delegates: two private
    copies of one primitive is how the next lane rediscovers the defect
    instead of inheriting the fix.
    """
    return fleet.read_log_window_raw(mark)


@dataclass
class Result:
    variant: Variant
    verdict: str
    detail: str = ""
    diagnostics: list = field(default_factory=list)
    window: str = ""


UNBOUND_MSG = "Unbound function called"


def classify(variant: Variant, window: str, fid: str, load: dict) -> Result:
    diags = [
        d
        for d in fleet.parse_game_diagnostics(window)
        if d["sev"] == "ERR" and d["file"].startswith(f"{fid}/")
    ]
    if diags:
        return Result(variant, "COMPILE_ERR", diags[0]["msg"], diags, window)
    if load.get("error"):
        return Result(variant, "AMBIGUOUS", f"load failed: {load['error']}", [], window)
    # B127 EXEMPTION, stated so it is not re-derived. A clean reply cannot
    # certify a build here either -- but this oracle does not need it to. Its
    # affirmative answer is the consumer PRINTING its start token below, a
    # positive control in the same family as `probe_enum_corpus`'s sentinel. A
    # build that failed for any reason never runs, never prints, and lands in
    # the `not started` branch as AMBIGUOUS. Silence is the negative answer
    # here, so no failure mode can impersonate BOUND or UNBOUND.

    started = f"{TOKEN} start {variant.id}" in window
    bound = f"{TOKEN} bound {variant.id} " in window
    unbound = UNBOUND_MSG in window

    if not started:
        # The consumer never ran. Reading that as UNBOUND would score a probe
        # that never happened as evidence about the engine.
        return Result(
            variant,
            "AMBIGUOUS",
            "the consumer's Main() never printed its start token — it did not "
            "run, so nothing about binding was measured",
            [],
            window,
        )
    if bound and unbound:
        return Result(
            variant,
            "AMBIGUOUS",
            "the call returned AND the window contains an unbound-function "
            "error — the window is contended, most likely by another plugin",
            [],
            window,
        )
    if bound:
        line = next(
            (l for l in window.splitlines() if f"{TOKEN} bound {variant.id} " in l), ""
        )
        return Result(variant, "BOUND", line.split("res=", 1)[-1].strip(), [], window)
    if unbound:
        return Result(variant, "UNBOUND", UNBOUND_MSG, [], window)
    return Result(
        variant,
        "AMBIGUOUS",
        "the consumer started but neither returned nor reported an unbound "
        "call — the run says nothing either way",
        [],
        window,
    )


# HOISTED (B097). This file used to carry its own copy of the length-prefixed
# reader and its own tm-control-mcp client, and its docstring asserted that
# `fleet.rb_route` was the broken one. That stopped being true when fleet's was
# fixed, and a stale comment claiming a sibling is broken is worse than none.
# Two readers of one wire format, one of them right, is the shape this batch
# keeps finding -- so there is now ONE, in fleet_dogfood, and these are names
# for it. Nothing here changes behaviour; it removes the second copy.
rb_route = fleet.rb_route
control_mcp = fleet.control_mcp
measure_read_cap = fleet.measure_read_cap
READ_CAP = fleet.READ_CAP
CONTROL_MCP = fleet.CONTROL_MCP


def load_plugin_payload(fid: str) -> dict:
    return {"id": fid, "source": "user", "type": "folder"}


def load_plugin(fid: str) -> dict:
    # B096: through the ROUTER, which since B097 prefers RemoteBuild again
    # (measured live by `rb_reassembles`, not inferred from request size).
    return fleet.game_route("load_plugin", load_plugin_payload(fid))


def preflight_request_size(fid: str) -> str | None:
    """Confirm RemoteBuild still cannot be driven, and say so precisely.

    Kept even though the probe now goes through `tm-control-mcp`: the next
    person to read this file will wonder why, and a measured answer is better
    than a comment. It also fails LOUD if RemoteBuild is ever fixed, which is
    the signal to move back.

    RemoteBuild's `HandleClient` calls `Json::Parse` on each raw socket read,
    and `ReadRaw` returns at most READ_CAP (64) bytes, so a longer request is
    split into fragments that individually fail to parse. The reply is
    ``{"error":"Not implemented: "}`` — the router's message for an *unknown
    route* — which sends the reader off to check route spellings. Note what
    that means for anyone probing routes: a >64-byte probe of ANY route name,
    including a deliberately bogus one, answers "Not implemented", so a
    negative control fails through the same mechanism as the positives and
    proves nothing. No route may be called unimplemented on that evidence.

    A minimal `load_plugin` request is 72 bytes with an EMPTY id, so no shorter
    plugin id fixes it and the call has never been reachable.
    """
    needed = len(json.dumps({"route": "load_plugin", "data": load_plugin_payload(fid)}))
    if needed <= READ_CAP:
        return None
    measured = measure_read_cap()
    detail = (
        f"measured {measured} bytes on the first read just now"
        if measured is not None
        else f"could not re-measure; using the recorded cap of {READ_CAP}"
    )
    return (
        f"RemoteBuild cannot receive a request this size. A `load_plugin` call "
        f"for `{fid}` is {needed} bytes and `ReadRaw` returns at most "
        f"{READ_CAP} per call ({detail}).\n"
        f"  A minimal `load_plugin` request is 72 bytes with an EMPTY id, so no "
        f"shorter plugin id fixes it — the call has never been reachable.\n"
        f"  Corollary: any >64-byte route probe answers `Not implemented` "
        f"whatever the route name, so a bogus-route control fails for the same "
        f"reason as a real one and cannot establish that a route is missing.\n"
        f"  This probe therefore drives the game through tm-control-mcp instead."
    )


def unload_plugin(fid: str) -> dict:
    try:
        return fleet.game_route("unload_plugin", {"id": fid})
    except Exception as exc:  # pragma: no cover - network teardown
        return {"error": f"unload_plugin raised: {exc}"}


def run(variants: list[Variant], workdir: Path) -> list[Result]:
    repro._install_teardown_handlers()

    plugins_root = fleet.OPN_PLUGINS
    strays = repro.probe_dirs_present(plugins_root)
    if strays:
        sys.exit(
            f"refusing to probe: probe-owned plugin(s) already installed: "
            f"{', '.join(strays)}. A stranded fixture declares the very names "
            f"this probe asks about (B077). Remove them by hand."
        )
    if not repro.rb_alive_with_retry():
        sys.exit("RemoteBuild is not alive on 127.0.0.1:30000 — is the game running?")

    # RemoteBuild answering `get_status` is NOT evidence that it can be driven:
    # the route that matters is bigger than the biggest request it can read.
    # Recorded rather than fatal, because the probe now goes through
    # tm-control-mcp — but printed every run, so the day it stops being true
    # is visible rather than inferred.
    # B096: decide on a LIVE round trip, not on request size. B097 fixed the
    # game-side read loop to drain on `Available()`, so a request larger than
    # READ_CAP now arrives intact — but READ_CAP itself is unchanged, so the
    # old size test could never notice and would have routed to a transport
    # that is currently DOWN.
    rb_ok = fleet.rb_reassembles()
    print(f"  RemoteBuild: {'usable (reassembles >READ_CAP)' if rb_ok else 'UNUSABLE'}")
    if not rb_ok:
        # Only then does tm-control-mcp matter. Its liveness check used to be
        # defeated by `control_mcp` reporting a refused connection as an empty
        # SUCCESS; that is fixed in fleet_dogfood, so this now bails loudly.
        probe = control_mcp("getLogs", id="tm-control-mcp", maxLines=1)
        if probe.get("error"):
            sys.exit(
                "neither transport is usable: RemoteBuild does not reassemble "
                f"and tm-control-mcp is not answering: {probe['error']}"
            )
        print("  routing via tm-control-mcp")
    else:
        # Recorded because it is a real trade, not a free win: tm-control-mcp
        # returns the compile log INLINE attributed to the build it triggered,
        # while RemoteBuild leaves us scraping the shared Openplanet.log by
        # window. `classify` already treats a foreign plugin's error in the
        # window as not-ours, but the attribution is weaker.
        print("  NOTE: log attribution is by window, not inline (tm-control-mcp down)")

    exporter = write_exporter(workdir, variants)
    results: list[Result] = []

    with repro.staged(exporter, EXPORTER_ID, plugins_dir=plugins_root):
        mark = fleet.log_size()
        exp_load = load_plugin(EXPORTER_ID)
        time.sleep(2.5)
        exp_window = read_raw_window(mark)
        # B127: the exporter must have BUILT, not merely have been delivered.
        # A reply of `{"error": "", "data": "Plugin loaded"}` comes back for a
        # build whose dependency could not be resolved, so `exp_load["error"]`
        # cannot carry this. A THIRD policy on the same four values: unlike
        # `probe_enum_corpus` (where REJECTED is normal) or `issue_repro_game`
        # (where it is the answer), the exporter must be CLEAN -- every variant
        # lives in this one plugin, so anything less makes the whole run say
        # nothing.
        exp_verdict = fleet.build_verdict(exp_window, EXPORTER_ID, exp_load)
        exp_errs = exp_verdict["game_errors"]
        try:
            if exp_verdict["build_status"] != "CLEAN":
                detail = (
                    exp_load.get("error")
                    or (exp_errs[0]["msg"] if exp_errs else None)
                    or (exp_verdict["raw_build_errors"] or
                        [f"build is {exp_verdict['build_status']}"])[0]
                )
                # Every variant lives in this one plugin, so a broken exporter
                # makes the whole run say nothing. Say that, rather than
                # returning a table of UNBOUNDs that look like findings.
                return [
                    Result(v, "AMBIGUOUS", f"exporter did not load: {detail}", exp_errs, exp_window)
                    for v in variants
                ]
            if f"{TOKEN} exporter ready" not in exp_window:
                return [
                    Result(
                        v,
                        "AMBIGUOUS",
                        "the exporter loaded but never ran its Main() — its "
                        "bodies may not be live",
                        [],
                        exp_window,
                    )
                    for v in variants
                ]
            print(f"  exporter loaded ({len(variants)} stubs)\n")

            for variant in variants:
                consumer = write_consumer(workdir, variant)
                fid = repro.PROBE_PREFIX + "b086-c-" + variant.id
                mark = fleet.log_size()
                holder = repro.staged(consumer, fid, plugins_dir=plugins_root)
                try:
                    with holder:
                        load = load_plugin(fid)
                        time.sleep(2.0)
                        window = read_raw_window(mark)
                        result = classify(variant, window, fid, load)
                        unload = unload_plugin(fid)
                        if unload.get("error"):
                            holder.preserve()
                            result = Result(
                                variant,
                                "AMBIGUOUS",
                                f"unload not acknowledged: {unload['error']}",
                                [],
                                window,
                            )
                finally:
                    if not holder.removed and not holder.preserved:
                        result = Result(
                            variant,
                            "AMBIGUOUS",
                            f"teardown incomplete for {fid} — still installed",
                        )
                mark_ok = "ok " if result.verdict == result.variant.expect else "   "
                print(f"  {mark_ok}{variant.id:<40} {result.verdict:<12} {result.detail[:60]}")
                results.append(result)
        finally:
            unload_plugin(EXPORTER_ID)
            time.sleep(0.5)

    return results


# --------------------------------------------------------------------------


def run_selftest() -> int:
    """Offline: prove the oracle can tell the answers apart. No game needed.

    B086 exists because a probe was run whose two variants failed identically.
    Both readings — "the engine is strict" and "the probe never reached the
    engine" — produce the same output from an oracle like that, so it measured
    nothing while looking like it had. The checks here are that failure mode
    turned into assertions.
    """
    failures: list[str] = []
    v = VARIANTS[0]
    fid = "lsp-issue-repro-b086-c-x"

    def case(label: str, window: str, expect: str, load: dict | None = None) -> None:
        got = classify(v, window, fid, load or {"error": ""}).verdict
        if got != expect:
            failures.append(f"{label}: expected {expect}, got {got}")
        else:
            print(f"  ok: {label} -> {expect}")

    start = f"{TOKEN} start {v.id}"
    case("a call that returned", f"{start}\n{TOKEN} bound {v.id} res=xy", "BOUND")
    case("an unbound call", f"{start}\n[ERR] {UNBOUND_MSG}", "UNBOUND")
    # The three ways a run can say nothing. Each must be AMBIGUOUS, never a
    # verdict: reading any of them as UNBOUND is how a probe that never ran
    # gets written into a test as a fact about the engine.
    case("the consumer never started", "unrelated log noise", "AMBIGUOUS")
    case("started, then silence", start, "AMBIGUOUS")
    case(
        "returned AND unbound in the same window",
        f"{start}\n{TOKEN} bound {v.id} res=xy\n[ERR] {UNBOUND_MSG}",
        "AMBIGUOUS",
    )
    case("RemoteBuild refused the load", start, "AMBIGUOUS", {"error": "boom"})
    # B127: the realistic reply for a build whose dependency could not be
    # resolved is a clean success. It must not turn a consumer that never ran
    # into a measurement. This pins the exemption rather than asserting it.
    case("a clean 'Plugin loaded' reply cannot make an unrun consumer look "
         "measured", "unrelated log noise", "AMBIGUOUS",
         {"error": "", "data": "Plugin loaded"})
    case("nor can it turn a started-then-silent consumer into a verdict",
         start, "AMBIGUOUS", {"error": "", "data": "Plugin loaded"})
    # The path shape matters: `fleet.parse_game_diagnostics` attributes a
    # diagnostic by matching `Plugins/<dir>/<file>.as`, so a synthetic window
    # without that prefix is attributed to "?" and would score AMBIGUOUS. A
    # fixture that does not look like the real log tests the wrong thing.
    case(
        "the game rejected the plugin",
        f"Plugins/{fid}/Main.as (3, 5) : ERR : No matching symbol 'Nope'",
        "COMPILE_ERR",
    )
    # A foreign plugin's error in the same window must not be read as ours:
    # Openplanet.log is shared, and other sessions load their own plugins.
    case(
        "another plugin's compile error in the window",
        f"Plugins/other-plugin/Main.as (1, 1) : ERR : boom\n{start}\n"
        f"{TOKEN} bound {v.id} res=xy",
        "BOUND",
    )

    # The variant table itself must be able to discriminate. Without this the
    # checks above pass over a corpus of rows that all say the same thing.
    ids = {x.id: x for x in VARIANTS}
    if ids.get("exact-const-ref-in") is None or ids.get("control-no-body") is None:
        failures.append("both controls must exist — see the module docstring")
    elif ids["exact-const-ref-in"].expect == ids["control-no-body"].expect:
        failures.append(
            "the positive and negative controls expect the SAME verdict — that "
            "is the oracle-cannot-discriminate defect B086 is about"
        )
    else:
        print("  ok: the two controls expect opposite verdicts")

    exact, lax = ids.get("exact-const-ref-in"), ids.get("lax-by-value-over-const-ref-in")
    if exact and lax:
        if exact.body != lax.body.replace("Echo_lax", "Echo_exact"):
            failures.append(
                "exact and lax do not share a body — they differ in more than "
                "the parameter type, so a difference in verdict would not "
                "isolate the parameter type"
            )
        elif exact.stub == lax.stub.replace("Echo_lax", "Echo_exact"):
            failures.append("exact and lax have the SAME stub — nothing varies")
        else:
            print("  ok: exact vs lax differ in the stub's parameter type alone")
    else:
        failures.append("the exact/lax pair is missing")

    if len({x.id for x in VARIANTS}) != len(VARIANTS):
        failures.append("duplicate variant ids — results would overwrite each other")
    if len({x.fn for x in VARIANTS}) != len(VARIANTS):
        failures.append(
            "duplicate function names — the exporter would not compile, and two "
            "variants would measure one function"
        )
    if not failures:
        print(f"  ok: {len(VARIANTS)} variants, all ids and function names distinct")

    if failures:
        print("\nFAIL: probe-export-binding selftest", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "\nPASS: the oracle distinguishes bound from unbound from "
        "did-not-measure,\nand the variant set can discriminate."
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--selftest", action="store_true", help="offline checks; needs no game"
    )
    parser.add_argument("--only", help="probe only variants whose id starts with this")
    parser.add_argument("--tsv", type=Path, help="write the verdict corpus here")
    parser.add_argument("--json", type=Path, help="write full records (windows) here")
    args = parser.parse_args()
    if args.selftest:
        return run_selftest()

    variants = VARIANTS
    if args.only:
        variants = [v for v in VARIANTS if v.id.startswith(args.only)]
        if not variants:
            sys.exit(f"no variant id starts with {args.only!r}")
        # The controls are what make any other row evidence; say so rather than
        # letting a narrowed run read like a full one.
        if not any(v.id in ("exact-const-ref-in", "control-no-body") for v in variants):
            print(
                "NOTE: neither control is in this run. A verdict here is not "
                "evidence about the engine on its own.\n",
                file=sys.stderr,
            )

    import tempfile

    with tempfile.TemporaryDirectory(prefix="b086-probe-") as tmp:
        print(f"==> probing {len(variants)} variant(s) against the live game\n")
        results = run(variants, Path(tmp))

    if args.tsv:
        rows = ["# B086 export stub/body binding, probed against the live game"]
        rows.append("variant\tstub\tbody\tverdict\tdetail")
        for r in results:
            body = r.variant.body.split(") {")[0] + ")" if r.variant.body else "(none)"
            rows.append(
                f"{r.variant.id}\t{r.variant.stub}\t{body}\t{r.verdict}\t{r.detail}"
            )
        args.tsv.write_text("\n".join(rows) + "\n")
        print(f"\nwrote {args.tsv}")
    if args.json:
        args.json.write_text(
            json.dumps(
                [
                    {
                        "id": r.variant.id,
                        "stub": r.variant.stub,
                        "body": r.variant.body,
                        "expect": r.variant.expect,
                        "verdict": r.verdict,
                        "detail": r.detail,
                        "window": r.window[-6000:],
                    }
                    for r in results
                ],
                indent=2,
            )
        )
        print(f"wrote {args.json}")

    controls = {r.variant.id: r.verdict for r in results}
    ok = True
    if controls.get("exact-const-ref-in") not in (None, "BOUND"):
        print(
            "\nCONTROL FAILED: the positive control did not bind. Nothing in "
            "this run is evidence about the engine — an oracle that cannot say "
            "yes cannot mean anything by no.",
            file=sys.stderr,
        )
        ok = False
    if controls.get("control-no-body") not in (None, "UNBOUND"):
        print(
            "\nCONTROL FAILED: a stub naming a function that does not exist "
            "did not report UNBOUND. The probe cannot see the failure it is "
            "looking for.",
            file=sys.stderr,
        )
        ok = False

    ambiguous = [r for r in results if r.verdict == "AMBIGUOUS"]
    if ambiguous:
        print(f"\n{len(ambiguous)} variant(s) AMBIGUOUS:", file=sys.stderr)
        for r in ambiguous:
            print(f"  - {r.variant.id}: {r.detail}", file=sys.stderr)

    print(f"\n{len(results)} variant(s) probed, {len(ambiguous)} ambiguous")
    return 0 if ok and not ambiguous else 1


if __name__ == "__main__":
    sys.exit(main())
