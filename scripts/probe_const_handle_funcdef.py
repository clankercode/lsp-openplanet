#!/usr/bin/env python3
"""B144 — is a CONST HANDLE parameter (`T@ const &in`) compatible with `T@ &in`
at a funcdef slot, as far as the ENGINE is concerned?

    scripts/probe_const_handle_funcdef.py [--tsv out.tsv] [--json out.json]
                                          [--b165-primitive]
                                          [--only <id-prefix>]

Requires Trackmania + Openplanet with RemoteBuild on 127.0.0.1:30000.
There is ONE game on ONE port and lanes share it — coordinate before running.

The question
------------

Max reported, against a shipped plugin that compiles in the game:

    src/Interface.as:641:31: error: No matching signatures to 'array::Sort(SortGhosts)'
    bool SortGhosts(const Json::Value@ const &in a, const Json::Value@ const &in b)

The discriminator is the TRAILING `const` — a const HANDLE — not the leading
one, which is a handle to a const OBJECT. A shipped plugin compiling is strong
evidence but it is not a measurement, so this asks the engine directly.

Q1  does a function whose params are `T@ const &in` bind to a funcdef
    declaring `T@ &in`?                                  (the reported case)
Q2  does it bind the OTHER way — `T@ &in` function at a `T@ const &in`
    funcdef? **If the engine is asymmetric, a symmetric fix is wrong.**
Q3  is the LEADING const (`const T@` vs `T@`) still EXACT, as B110
    established? This is the control that stops the fix widening into B110's
    territory. `const T@` and `T@` differ in what may be done through the
    handle, so a fix that relaxed BOTH would be a real regression.
Q4  the same for `&out`, `&inout` and by-value.
Q5  the same for a RETURN type.
Q6  the reported call path itself: `array<T>::Sort` with such a comparator.
Q9  does primitive width or signedness participate exactly in a funcdef
    signature? B165 asks both directions for int/int64 and uint/uint64,
    signed/unsigned crossings at both widths, and the real `startnew` int64
    userdata slot that motivated the question.

Declarability is asked separately (`decl-*`), because "the engine rejected it"
must be attributable to the BINDING and not to the spelling being illegal in a
parameter list at all. Without those rows a REJECT on `q4-out` would be
uninterpretable.

Controls, without which no row is evidence
------------------------------------------

* `ctl-valid`   an exactly-matching funcdef assignment -> ACCEPT.
                Proves the channel can report success.
* `ctl-bogus`   a call to a function that does not exist -> REJECT.
                Proves the channel can SEE an error; without it every ACCEPT
                below could be a dead instrument.
* `ctl-arity`   a 1-param function at a 2-param funcdef -> REJECT.
* `ctl-ret`     `int` function at a `bool` funcdef -> REJECT.
* `ctl-elem`    `string` param at a `Json::Value@` funcdef -> REJECT.
                Those three prove the engine enforces funcdef signatures AT
                ALL, so an ACCEPT on a const row means "const handles are not
                part of the signature", not "the engine matches anything".
* `ctl-sort-ok` the reported call path with a plain comparator -> ACCEPT.
                Proves `array::Sort` binding works here before asking whether
                a const handle breaks it.

Oracle and cleanup are B119's, imported rather than re-derived: `classify`
delegates to `issue_repro_game.probe`'s four-way `build_status` (B118), whose
verdict chain is mutation-tested. A hand-rolled copy here would not be.
"""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import fleet_dogfood as fleet  # noqa: E402
import issue_repro_game as repro  # noqa: E402
from probe_const_receiver import V, classify  # noqa: E402

TOKEN = "B144CHF"
B165_TOKEN = "B165PF"

# Written at the top of the TSV. The engine version is the fact a bare table
# loses first, and this repo cites three generations concurrently (1.29.5,
# 1.29.9, 1.29.14) — see docs/probes/MANIFEST.tsv on why an unqualified diff
# across them reports drift that is really a version change.
PREAMBLE = """\
# B144 const-handle funcdef compatibility, probed against the live game.
#
# Openplanet 1.29.14 (next, Public, 6fd4200f), 2026-09-01.
# Generator: python3 scripts/probe_const_handle_funcdef.py --tsv <this file>
#
# THE QUESTION. AngelScript spells two different qualifiers with the same
# keyword: a LEADING const (`const T@`) is a handle to a const OBJECT, a
# TRAILING one (`T@ const`) is a const HANDLE to a mutable object, and both
# may appear at once. Which of them are interchangeable at a funcdef slot?
#
# THE ANSWER: none of them. The engine is EXACT on every const position, in
# both directions, at every param mode and in the return type. What made a
# shipped plugin look wrong to this checker is that the engine's OWN
# comparator for `array<T>::Sort` carries BOTH consts whenever the element
# type is a HANDLE --
#
#     Where 'less' is 'bool Json::Value@[]::less(const Json::Value@const&in,
#                                                const Json::Value@const&in)'
#
# -- so `const Json::Value@ &in`, the spelling this checker demanded, is the
# one the engine rejects, and `const Json::Value@ const &in`, the spelling it
# rejected, is the one the engine wants. For a NON-handle element type the
# comparator is plain `const T&in`, and `SortNonConst`'s is plain `T&in`.
#
# The `sig-*` rows exist to make the engine PRINT that signature per element
# type: a deliberate mismatch makes the compiler emit its own `less`
# declaration, which is a better oracle than inferring the substitution rule
# from accept/reject.
#
# Trailing `const` is legal ONLY after a `@` (see q4-prim / q4-valueclass:
# the parser refuses it on `int` and `vec3`), and `T@ const &inout` is not
# declarable at all.
#
# `expect` is ACCEPT/REJECT for CONTROL rows and UNKNOWN for question rows;
# a control whose verdict does not match its expectation invalidates the run.
"""

B165_PREAMBLE = """\
# B165 primitive funcdef exactness, probed against the live game.
#
# Openplanet 1.29.14 (next, Public, 6fd4200f), 2026-09-01.
# Generator: python3 scripts/probe_const_handle_funcdef.py --b165-primitive --tsv <this file>
#
# THE QUESTION. Does a primitive handler bind a funcdef slot through ordinary
# numeric conversion, or is a funcdef signature exact about primitive width
# and signedness? The real motivating seam is `void H(int)` offered to
# `startnew`, whose integer-userdata callback is `void(int64)`.
#
# THE ANSWER. Exact. The real startnew row rejects, as do int/int64 and
# uint/uint64 in both directions and signed/unsigned crossings at both 32 and
# 64 bits. The exact int64/int64 control accepts and the bogus-symbol control
# rejects, proving both oracle polarities were live.
#
# FLEET EXPOSURE. Before preserving B110's exact checker rule, a temporary
# marker re-ran its old-vs-exact disagreement over 151 shipped plugin dirs plus
# five in-repo fixture plugins: 408 comparisons were exposed, all four known
# overload-sibling pairs (int64<->uint64 and either ->double). No int<->int64
# hit and no reference-data defect appeared. The exact sibling still binds at
# each call, which B110's typedb-backed startnew tripwire holds shut.
#
# `expect` is ACCEPT/REJECT for CONTROL rows and UNKNOWN for question rows;
# a control whose verdict does not match its expectation invalidates the run.
"""


def write_fixture(root: Path, v: V, token: str = TOKEN) -> Path:
    """One plugin folder per variant (B068: one construct per compilation unit)."""
    suffix = token + v.id.replace("-", "")
    pretty = f"{token} {v.id}"
    v.fid = repro.PROBE_PREFIX + fleet.folder_id(pretty)
    plugin = root / v.id
    plugin.mkdir(parents=True)
    (plugin / "info.toml").write_text(
        "[meta]\n"
        f'name     = "{pretty}"\n'
        'author   = "clankercode"\n'
        'category = "Tests"\n'
        'version  = "0.1.0"\n'
        "\n[script]\n"
    )
    (plugin / "Main.as").write_text(
        f"// {token} funcdef probe :: {v.id}\n"
        f"// question: {v.question}\n"
        f"// stands in for: {v.stands_for}\n"
        + (v.code % {"S": suffix})
        + "\nvoid Main() {\n"
        f'    print("{token} {v.id} ready");\n'
        "}\n"
    )
    return plugin


# A funcdef-binding variant: declare a funcdef, declare a function, assign.
# `Use` is called from Main so nothing is dead-stripped before analysis.
def bind(fd_params: str, fn_params: str, *, ret: str = "bool",
         fd_ret: str = "bool", body: str = "return true;",
         extra: str = "") -> str:
    return (
        f"{extra}"
        f"funcdef {fd_ret} FD%(S)s({fd_params});\n"
        f"{ret} F%(S)s({fn_params}) {{ {body} }}\n"
        "void Use%(S)s() {\n"
        "    FD%(S)s@ h = F%(S)s;\n"
        '    if (h is null) print("never");\n'
        "}\n"
    )


CLS = "class C%(S)s { int v; }\n"
IFACE = "interface I%(S)s { void M(); }\n"


def variants() -> list[V]:
    v: list[V] = []

    # ---- controls: the channel works, and signature matching is enforced --
    v.append(V(
        id="ctl-valid",
        code=bind("const Json::Value@ &in a, const Json::Value@ &in b",
                  "const Json::Value@ &in a, const Json::Value@ &in b"),
        question="control", expect="ACCEPT",
        stands_for="an exactly-matching funcdef assignment",
    ))
    v.append(V(
        id="ctl-bogus",
        code="void Use%(S)s() { NoSuchFunction%(S)s(); }\n",
        question="control", expect="REJECT",
        stands_for="a definitely-invalid call; proves the oracle sees errors",
    ))
    v.append(V(
        id="ctl-arity",
        code=bind("const Json::Value@ &in a, const Json::Value@ &in b",
                  "const Json::Value@ &in a"),
        question="control", expect="REJECT",
        stands_for="wrong arity must still fail",
    ))
    v.append(V(
        id="ctl-ret",
        code=bind("const Json::Value@ &in a, const Json::Value@ &in b",
                  "const Json::Value@ &in a, const Json::Value@ &in b",
                  ret="int", body="return 1;"),
        question="control", expect="REJECT",
        stands_for="wrong return type must still fail",
    ))
    v.append(V(
        id="ctl-elem",
        code=bind("const Json::Value@ &in a, const Json::Value@ &in b",
                  "const string &in a, const string &in b"),
        question="control", expect="REJECT",
        stands_for="wrong parameter type must still fail",
    ))
    v.append(V(
        id="ctl-sort-ok",
        code=("bool Cmp%(S)s(const Json::Value@ &in a, const Json::Value@ &in b)"
              " { return true; }\n"
              "void Use%(S)s() {\n"
              "    Json::Value@[] arr;\n"
              "    arr.Sort(Cmp%(S)s);\n"
              "}\n"),
        question="Q6", expect="UNKNOWN",
        stands_for="the reported call path with a PLAIN comparator — the "
                   "spelling the CHECKER currently demands. Written as an "
                   "ACCEPT control and it came back REJECT, which is the "
                   "whole finding: see docs/probes/b144-const-handle-funcdef.tsv",
    ))

    # ---- declarability: is the spelling legal at all, before binding? -----
    for did, decl in [
        ("decl-in", "void D%(S)s(Json::Value@ const &in a) {}\n"),
        ("decl-lead-in", "void D%(S)s(const Json::Value@ const &in a) {}\n"),
        ("decl-out", "void D%(S)s(Json::Value@ const &out a) {}\n"),
        ("decl-inout", "void D%(S)s(Json::Value@ const &inout a) {}\n"),
        ("decl-val", "void D%(S)s(Json::Value@ const a) {}\n"),
        ("decl-ret", "Json::Value@ const G%(S)s() { return null; }\n"),
    ]:
        v.append(V(
            id=did,
            code=decl + "void Use%(S)s() { }\n",
            question="Q0-declarable", expect="UNKNOWN",
            stands_for=f"is `{did}`'s spelling legal at all (separates illegal "
                       "syntax from an incompatible signature)",
        ))

    # ---- Q1: trailing const on the FUNCTION, plain funcdef ---------------
    v.append(V(
        id="q1-in-eng",
        code=bind("Json::Value@ &in a, Json::Value@ &in b",
                  "Json::Value@ const &in a, Json::Value@ const &in b"),
        question="Q1", expect="UNKNOWN",
        stands_for="engine handle: `T@ const &in` fn at a `T@ &in` funcdef",
    ))
    v.append(V(
        id="q1-in-eng-lead",
        code=bind("const Json::Value@ &in a, const Json::Value@ &in b",
                  "const Json::Value@ const &in a, const Json::Value@ const &in b"),
        question="Q1", expect="UNKNOWN",
        stands_for="THE REPORTED CASE: `const T@ const &in` at `const T@ &in`",
    ))
    v.append(V(
        id="q1-in-cls",
        code=bind("C%(S)s@ &in a, C%(S)s@ &in b",
                  "C%(S)s@ const &in a, C%(S)s@ const &in b", extra=CLS),
        question="Q1", expect="UNKNOWN",
        stands_for="workspace class handle",
    ))
    v.append(V(
        id="q1-in-iface",
        code=bind("I%(S)s@ &in a, I%(S)s@ &in b",
                  "I%(S)s@ const &in a, I%(S)s@ const &in b", extra=IFACE),
        question="Q1", expect="UNKNOWN",
        stands_for="interface handle",
    ))
    v.append(V(
        id="q1-in-arr",
        code=bind("array<int>@ &in a, array<int>@ &in b",
                  "array<int>@ const &in a, array<int>@ const &in b"),
        question="Q1", expect="UNKNOWN",
        stands_for="`array<T>@` handle",
    ))
    v.append(V(
        id="q1-in-dict",
        code=bind("dictionary@ &in a, dictionary@ &in b",
                  "dictionary@ const &in a, dictionary@ const &in b"),
        question="Q1", expect="UNKNOWN",
        stands_for="`dictionary@` handle",
    ))

    # ---- Q2: the OTHER direction. Asymmetry would make a symmetric fix wrong
    v.append(V(
        id="q2-in-eng",
        code=bind("Json::Value@ const &in a, Json::Value@ const &in b",
                  "Json::Value@ &in a, Json::Value@ &in b"),
        question="Q2", expect="UNKNOWN",
        stands_for="`T@ &in` fn at a `T@ const &in` funcdef (reverse)",
    ))
    v.append(V(
        id="q2-in-cls",
        code=bind("C%(S)s@ const &in a, C%(S)s@ const &in b",
                  "C%(S)s@ &in a, C%(S)s@ &in b", extra=CLS),
        question="Q2", expect="UNKNOWN",
        stands_for="reverse, workspace class handle",
    ))

    # ---- Q3: B110's territory. LEADING const must stay EXACT --------------
    v.append(V(
        id="q3-lead-add",
        code=bind("C%(S)s@ &in a, C%(S)s@ &in b",
                  "const C%(S)s@ &in a, const C%(S)s@ &in b", extra=CLS),
        question="Q3", expect="UNKNOWN",
        stands_for="CONTROL: handle-to-const fn at a handle-to-mutable funcdef",
    ))
    v.append(V(
        id="q3-lead-drop",
        code=bind("const C%(S)s@ &in a, const C%(S)s@ &in b",
                  "C%(S)s@ &in a, C%(S)s@ &in b", extra=CLS),
        question="Q3", expect="UNKNOWN",
        stands_for="CONTROL: the reverse; a fix must not move either row",
    ))
    v.append(V(
        id="q3-lead-add-eng",
        code=bind("Json::Value@ &in a, Json::Value@ &in b",
                  "const Json::Value@ &in a, const Json::Value@ &in b"),
        question="Q3", expect="UNKNOWN",
        stands_for="CONTROL: the same on an engine handle",
    ))

    # ---- Q4: the other param modes, and by-value -------------------------
    v.append(V(
        id="q4-out",
        code=bind("C%(S)s@ &out a", "C%(S)s@ const &out a",
                  body="@a = C%(S)s(); return true;", extra=CLS),
        question="Q4", expect="UNKNOWN",
        stands_for="`&out` with a const handle",
    ))
    v.append(V(
        id="q4-inout",
        code=bind("C%(S)s@ &inout a", "C%(S)s@ const &inout a", extra=CLS),
        question="Q4", expect="UNKNOWN",
        stands_for="`&inout` with a const handle",
    ))
    v.append(V(
        id="q4-val",
        code=bind("C%(S)s@ a", "C%(S)s@ const a", extra=CLS),
        question="Q4", expect="UNKNOWN",
        stands_for="by value with a const handle",
    ))
    v.append(V(
        id="q4-val-rev",
        code=bind("C%(S)s@ const a", "C%(S)s@ a", extra=CLS),
        question="Q4", expect="UNKNOWN",
        stands_for="by value, reverse direction",
    ))
    v.append(V(
        id="q4-valueclass",
        code=bind("const vec3 &in a", "vec3 const &in a", body="return true;"),
        question="Q4", expect="UNKNOWN",
        stands_for="a VALUE class: `vec3 const &in` vs `const vec3 &in`",
    ))
    v.append(V(
        id="q4-prim",
        code=bind("const int &in a", "int const &in a"),
        question="Q4", expect="UNKNOWN",
        stands_for="a primitive: `int const &in` vs `const int &in`",
    ))

    # ---- Q5: RETURN type -------------------------------------------------
    v.append(V(
        id="q5-ret-constheld",
        code=bind("int a", "int a", ret="C%(S)s@ const", fd_ret="C%(S)s@",
                  body="return null;", extra=CLS),
        question="Q5", expect="UNKNOWN",
        stands_for="`C@ const` return at a `C@` funcdef",
    ))
    v.append(V(
        id="q5-ret-lead",
        code=bind("int a", "int a", ret="const C%(S)s@", fd_ret="C%(S)s@",
                  body="return null;", extra=CLS),
        question="Q5", expect="UNKNOWN",
        stands_for="CONTROL: `const C@` return at a `C@` funcdef (leading const)",
    ))

    # ---- Q6: the reported call path, `array<T>::Sort` ---------------------
    v.append(V(
        id="q6-sort-reported",
        code=("bool Cmp%(S)s(const Json::Value@ const &in a,"
              " const Json::Value@ const &in b) { return true; }\n"
              "void Use%(S)s() {\n"
              "    Json::Value@[] arr;\n"
              "    arr.Sort(Cmp%(S)s);\n"
              "}\n"),
        question="Q6", expect="UNKNOWN",
        stands_for="EXACTLY what Max reported: SortGhosts on a Json::Value@[]",
    ))
    v.append(V(
        id="q6-sort-trailing",
        code=("bool Cmp%(S)s(Json::Value@ const &in a, Json::Value@ const &in b)"
              " { return true; }\n"
              "void Use%(S)s() {\n"
              "    Json::Value@[] arr;\n"
              "    arr.Sort(Cmp%(S)s);\n"
              "}\n"),
        question="Q6", expect="UNKNOWN",
        stands_for="trailing const only, no leading const",
    ))
    v.append(V(
        id="q6-sort-3arg",
        code=("bool Cmp%(S)s(const Json::Value@ const &in a,"
              " const Json::Value@ const &in b) { return true; }\n"
              "void Use%(S)s() {\n"
              "    Json::Value@[] arr;\n"
              "    arr.Sort(Cmp%(S)s, 0, 0);\n"
              "}\n"),
        question="Q6", expect="UNKNOWN",
        stands_for="the 3-arg `Sort(cb, start, count)` form",
    ))
    v.append(V(
        id="q6-sort-cls",
        code=(CLS +
              "bool Cmp%(S)s(C%(S)s@ const &in a, C%(S)s@ const &in b)"
              " { return true; }\n"
              "void Use%(S)s() {\n"
              "    C%(S)s@[] arr;\n"
              "    arr.Sort(Cmp%(S)s);\n"
              "}\n"),
        question="Q6", expect="UNKNOWN",
        stands_for="the same through a workspace class array",
    ))
    # ---- Q7: make the engine PRINT its own `less` funcdef, per element type
    #
    # A deliberate mismatch makes the compiler emit
    #   INFO: Where 'less' is 'bool T[]::less(<params>)'
    # which is the engine's synthesised comparator signature in ITS OWN words.
    # That is a far better oracle than inferring the substitution rule from
    # accept/reject, and it is exactly the text B102's synthesis must match.
    for eid, decl, extra in [
        ("sig-int", "int[] arr;", ""),
        ("sig-uint", "uint[] arr;", ""),
        ("sig-string", "string[] arr;", ""),
        ("sig-vec3", "vec3[] arr;", ""),
        ("sig-enum", "E%(S)s[] arr;", "enum E%(S)s { A, B }\n"),
        ("sig-clshandle", "C%(S)s@[] arr;", CLS),
        ("sig-clsvalue", "C%(S)s[] arr;", CLS),
        ("sig-ifacehandle", "I%(S)s@[] arr;", IFACE),
        ("sig-jsonhandle", "Json::Value@[] arr;", ""),
        ("sig-arrayhandle", "array<int>@[] arr;", ""),
        ("sig-dicthandle", "dictionary@[] arr;", ""),
    ]:
        v.append(V(
            id=eid,
            code=(extra +
                  "bool Cmp%(S)s(bool a, bool b) { return true; }\n"
                  "void Use%(S)s() {\n"
                  f"    {decl}\n"
                  "    arr.Sort(Cmp%(S)s);\n"
                  "}\n"),
            question="Q7-signature", expect="REJECT",
            stands_for=f"deliberate mismatch on {eid[4:]}[] so the engine prints "
                       "its own `less` signature",
        ))

    # The Openplanet-only NON-const slot, same trick.
    for eid, decl in [
        ("signc-int", "int[] arr;"),
        ("signc-jsonhandle", "Json::Value@[] arr;"),
    ]:
        v.append(V(
            id=eid,
            code=("bool Cmp%(S)s(bool a, bool b) { return true; }\n"
                  "void Use%(S)s() {\n"
                  f"    {decl}\n"
                  "    arr.SortNonConst(Cmp%(S)s);\n"
                  "}\n"),
            question="Q7-signature", expect="REJECT",
            stands_for=f"the `SortNonConst` slot's own signature for {eid[6:]}[]",
        ))

    # ---- Q8: the row `b102-array-sort-correct-callbacks` records as
    # UNMEASURED. That fixture pins `bool LessIntNonConst(int &in, int &in)`
    # as a required SILENCE, explicitly because "whether the engine accepts
    # this at the const slot is UNMEASURED — see B097, game probing is
    # unavailable". B097 is fixed, so measure it and retire the note.
    v.append(V(
        id="q8-nonconst-prim",
        code=("bool Cmp%(S)s(int &in a, int &in b) { return true; }\n"
              "void Use%(S)s() {\n"
              "    int[] arr;\n"
              "    arr.Sort(Cmp%(S)s);\n"
              "}\n"),
        question="Q8", expect="UNKNOWN",
        stands_for="`int &in` (no const) at the const `less` slot — the row the "
                   "b102 correct-callbacks fixture pins as UNMEASURED",
    ))
    v.append(V(
        id="q8-nonconst-prim-nc",
        code=("bool Cmp%(S)s(int &in a, int &in b) { return true; }\n"
              "void Use%(S)s() {\n"
              "    int[] arr;\n"
              "    arr.SortNonConst(Cmp%(S)s);\n"
              "}\n"),
        question="Q8", expect="UNKNOWN",
        stands_for="the same callback at the NON-const slot",
    ))
    v.append(V(
        id="q8-const-prim-nc",
        code=("bool Cmp%(S)s(const int &in a, const int &in b) { return true; }\n"
              "void Use%(S)s() {\n"
              "    int[] arr;\n"
              "    arr.SortNonConst(Cmp%(S)s);\n"
              "}\n"),
        question="Q8", expect="UNKNOWN",
        stands_for="a CONST callback at the NON-const slot (the reverse)",
    ))

    # ---- Q9 / B165: primitive width and signedness ----------------------
    #
    # One prefix intentionally contains both controls and every question row,
    # so `--only b165-primitive` remains an evidential run rather than a
    # question-only sample whose oracle was never shown accepting or rejecting.
    v.append(V(
        id="b165-primitive-ctl-valid",
        code=bind("int64 v", "int64 v"),
        question="control", expect="ACCEPT",
        stands_for="an exactly matching primitive funcdef signature",
    ))
    v.append(V(
        id="b165-primitive-ctl-bogus",
        code="void Use%(S)s() { NoSuchFunction%(S)s(); }\n",
        question="control", expect="REJECT",
        stands_for="a definitely-invalid call; proves the oracle sees errors",
    ))

    # The real built-in seam behind B110's isolated row. `startnew`'s integer
    # userdata overload takes an int64 callback, while the literal selects the
    # corresponding userdata family. Keep this beside the local funcdef rows:
    # the local declarations discriminate both directions, which the built-in
    # overload family cannot express.
    v.append(V(
        id="b165-primitive-startnew-int-int64",
        code=("void F%(S)s(int v) { }\n"
              "void Use%(S)s() { startnew(F%(S)s, 1); }\n"),
        question="Q9-primitive", expect="UNKNOWN",
        stands_for="the reported bare int handler at startnew's int64 userdata slot",
    ))

    primitive_pairs = [
        ("int-at-int64", "int64", "int"),
        ("int64-at-int", "int", "int64"),
        ("uint-at-uint64", "uint64", "uint"),
        ("uint64-at-uint", "uint", "uint64"),
        ("int-at-uint", "uint", "int"),
        ("uint-at-int", "int", "uint"),
        ("int64-at-uint64", "uint64", "int64"),
        ("uint64-at-int64", "int64", "uint64"),
    ]
    for pair_id, slot_ty, handler_ty in primitive_pairs:
        v.append(V(
            id=f"b165-primitive-{pair_id}",
            code=bind(f"{slot_ty} v", f"{handler_ty} v"),
            question="Q9-primitive", expect="UNKNOWN",
            stands_for=(f"primitive signature direction: `{handler_ty}` handler "
                        f"at a `{slot_ty}` funcdef slot"),
        ))
    return v


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--json")
    ap.add_argument(
        "--b165-primitive",
        action="store_true",
        help="run B165's primitive-signature suite instead of B144's const suite",
    )
    ap.add_argument("--only", help="run only variants whose id starts with this")
    args = ap.parse_args()

    vs = variants()
    if args.b165_primitive:
        vs = [v for v in vs if v.id.startswith("b165-primitive-")]
    else:
        vs = [v for v in vs if not v.id.startswith("b165-primitive-")]
    if args.only:
        vs = [v for v in vs if v.id.startswith(args.only)]
        if not vs:
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

    token = B165_TOKEN if args.b165_primitive else TOKEN
    tmp = Path(tempfile.mkdtemp(prefix=f"{token.lower()}-"))
    rows = []
    try:
        for v in vs:
            fixture = write_fixture(tmp, v, token)
            rec = repro.probe(fixture)
            verdict, reason = classify(rec)
            # The MESSAGE is the part hand-transcription loses and the part
            # that moves between engine versions. Record it verbatim.
            msgs = [f"({d['line']},{d['col']}) {d['sev']}: {d['msg']}"
                    for d in rec.get("diagnostics", [])]
            rows.append({"id": v.id, "question": v.question, "expect": v.expect,
                         "verdict": verdict,
                         "build_status": rec.get("build_status") or "-",
                         "reason": reason,
                         "engine_messages": msgs,
                         "stands_for": v.stands_for})
            print(f"{v.id:22} {v.question:14} expect={v.expect:8} -> {verdict:10} {reason}",
                  file=sys.stderr)
            for m in msgs:
                print(f"    {m}", file=sys.stderr)
    finally:
        import shutil
        shutil.rmtree(tmp, ignore_errors=True)

    # B077: a stranded fixture turns every LATER probe's rows into AMBIGUOUS.
    left = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    print()
    print(f"B077 sweep: {'clean' if not left else 'STRANDED: ' + repr(left)}")

    problems = fleet.probe_run_problems(rows)
    if problems:
        print("THE RUN IS NOT EVIDENCE:", file=sys.stderr)
        for pr in problems:
            print(f"  - {pr}", file=sys.stderr)

    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=1))
    if args.tsv:
        Path(args.tsv).write_text(
            (B165_PREAMBLE if args.b165_primitive else PREAMBLE)
            + "id\tquestion\texpect\tverdict\tbuild_status\tengine_messages\tstands_for\n"
            + "".join(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n".format(
                    r["id"], r["question"], r["expect"], r["verdict"],
                    r["build_status"],
                    " || ".join(r["engine_messages"]) or "-",
                    r["stands_for"],
                )
                for r in rows))
    return 1 if (problems or left) else 0


if __name__ == "__main__":
    raise SystemExit(main())
