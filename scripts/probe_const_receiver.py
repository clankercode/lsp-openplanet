#!/usr/bin/env python3
"""B119 — does the engine reject a non-const method on a const receiver when
the method is INHERITED, and does `const T@[]@` make its ELEMENTS const?

    scripts/probe_const_receiver.py [--tsv out.tsv] [--json out.json]
                                    [--only <id-prefix>]

Requires Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.

The question, and why it needed the game
----------------------------------------

`NonConstMethodOnConstReceiver` is own-class-only: it matches a method
declared on the receiver's OWN class and never walks parents. B116 measured
that this is true on both sides of the file boundary and deliberately left it
alone, pinning the two halves as EQUAL rather than as zero
(`inherited_nonconst_method_agrees_across_the_file_boundary`).

The B116 fleet sweep found exactly ONE site in 151 plugins where a
parent-walking gate would change a verdict:

    tm-map-together/src/Socket.as:467   skins[i].WriteToNetworkBuffer(buf)

`skins` is `const Editor::SetSkinSpec@[]@`; `WriteToNetworkBuffer` is
non-const on `NetworkSerializable`, two levels up.

That single site admits two readings which want OPPOSITE fixes:

1. a real missed error -- the rule should walk parents; or
2. `const T@[]@` ELEMENT constness does not propagate the way the checker
   models it, so `skins[i]` is not a const receiver and there is nothing to
   fire on. The outer handle being const says nothing on its own about the
   elements.

Nothing in the repo distinguishes them, and picking one and then writing
tests that ratify it is the failure mode this probe exists to avoid.

The two questions are INDEPENDENT, which the ticket's either/or framing
obscures, so both are measured:

  Q-A  does the engine enforce const-receiver against an INHERITED
       non-const method at all (depth 1, 2, 4)?
  Q-B  does `const T@[]@` / `const T[]` make ELEMENT access const?

`arr-own-nonconst` is what separates them. It puts a non-const method
declared on the element's OWN class behind a `const T@[]@` subscript. Its
verdict is Q-B's answer with Q-A held fixed, because the own-class rule is
the one the checker already implements and `ctl-own-nonconst` proves the
engine enforces it. Without that row, an ACCEPT anywhere in the array family
is unattributable between "elements are not const" and "parents are not
walked".

The oracle
----------

Compile-only, so this follows `probe_shared_modifiers.py` rather than the
runtime `Unbound function called` oracle: one plugin folder per variant
(B068 -- a batched parse abort would mark every other construct in the unit
ACCEPTED, a false green on a question about acceptance), staged through
`issue_repro_game.probe` for B077 cleanup, and classified from the log
window bracketed by this folder's own build markers.

ACCEPT requires BOTH `Starting build for "<fid>"` and `Loaded plugin
'<fid>'`, read from an UNFILTERED window. `fleet.read_log_window` drops
`[ TRAC]` lines and `Loaded plugin` IS a TRAC line, so the inherited oracle's
`compiled_clean` means only "no ERR attributed to me" -- absence of evidence.
On a question whose subject is acceptance that is a false-green channel: a
build that never ran reads identically to a clean one. (This is the channel
B118 is fixing in the shared harness; this probe does not depend on that fix
landing, because it injects its own unfiltered window through the existing
`read_log_window_fn` seam and demands the positive markers.)

Controls, without which no row here is evidence
-----------------------------------------------

Three, and they fail in different directions on purpose:

* `ctl-valid`      a const method on a const receiver -> ACCEPT.
                   Proves the channel can report success at all.
* `ctl-bogus`      a call to a method that does not exist -> REJECT.
                   Proves the channel can SEE an error. Without it, every
                   ACCEPT below could be a dead instrument.
* `ctl-own-nonconst`  a non-const method declared on the receiver's OWN
                   class, through a const handle -> REJECT.
                   THE load-bearing control: it proves the engine enforces
                   const receivers at all. If this ACCEPTS, the checker's
                   EXISTING rule is a false-positive generator and B119 is
                   not the ticket to open.

`inh-*-const` guards the other direction: a fix that fires on every
inherited method regardless of its qualifier would pass a suite that only
checks that things start firing.

Naming
------

Every class, function and module name is suffixed with the variant id. ~40
plugins are loaded in the shared game and a name collision would produce a
REJECT that is not about constness.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import fleet_dogfood as fleet  # noqa: E402
import issue_repro_game as repro  # noqa: E402

TOKEN = "B119CR"


@dataclass
class V:
    id: str
    # AngelScript placed above Main(). `%(S)s` is the per-variant suffix.
    code: str
    question: str          # which question this row answers
    expect: str            # ACCEPT / REJECT for controls, UNKNOWN otherwise
    stands_for: str
    fid: str = ""


def variants() -> list[V]:
    v: list[V] = []

    # ---- controls -------------------------------------------------------
    v.append(V(
        id="ctl-valid",
        code=(
            "class C%(S)s { void Peek() const {} }\n"
            "void Use%(S)s(const C%(S)s@ c) { c.Peek(); }\n"
        ),
        question="control",
        expect="ACCEPT",
        stands_for="any correctly-written const method on a const receiver",
    ))
    v.append(V(
        id="ctl-bogus",
        code=(
            "class C%(S)s { void Peek() const {} }\n"
            "void Use%(S)s(C%(S)s@ c) { c.NoSuchMethod%(S)s(); }\n"
        ),
        question="control",
        expect="REJECT",
        stands_for="a definitely-invalid call; proves the oracle sees errors",
    ))
    v.append(V(
        id="ctl-own-nonconst",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const C%(S)s@ c) { c.Mutate(); }\n"
        ),
        question="control",
        expect="REJECT",
        stands_for="the rule the checker already implements (own-class, non-const)",
    ))

    # ---- Q-A: inherited non-const, by depth ------------------------------
    v.append(V(
        id="inh-d1-nonconst",
        code=(
            "class B%(S)s { void Mutate() {} }\n"
            "class D%(S)s : B%(S)s {}\n"
            "void Use%(S)s(const D%(S)s@ d) { d.Mutate(); }\n"
        ),
        question="Q-A",
        expect="UNKNOWN",
        stands_for="the exact shape of the flip test in checker.rs",
    ))
    v.append(V(
        id="inh-d2-nonconst",
        code=(
            "class A%(S)s { void Mutate() {} }\n"
            "class B%(S)s : A%(S)s {}\n"
            "class D%(S)s : B%(S)s {}\n"
            "void Use%(S)s(const D%(S)s@ d) { d.Mutate(); }\n"
        ),
        question="Q-A",
        expect="UNKNOWN",
        stands_for="Editor::SetSkinSpec -> NetworkSerializable, two levels up",
    ))
    v.append(V(
        id="inh-d4-nonconst",
        code=(
            "class A1%(S)s { void Mutate() {} }\n"
            "class A2%(S)s : A1%(S)s {}\n"
            "class A3%(S)s : A2%(S)s {}\n"
            "class A4%(S)s : A3%(S)s {}\n"
            "void Use%(S)s(const A4%(S)s@ d) { d.Mutate(); }\n"
        ),
        question="Q-A",
        expect="UNKNOWN",
        stands_for="MLFeed's 4-deep hierarchies (B116 measured 2 sites there)",
    ))
    v.append(V(
        id="inh-d2-const",
        code=(
            "class A%(S)s { void Peek() const {} }\n"
            "class B%(S)s : A%(S)s {}\n"
            "class D%(S)s : B%(S)s {}\n"
            "void Use%(S)s(const D%(S)s@ d) { d.Peek(); }\n"
        ),
        question="Q-A-guard",
        expect="ACCEPT",
        stands_for="an inherited CONST method; a fix must not fire on this",
    ))

    # ---- Q-A: receiver spellings, depth 1 -------------------------------
    for rid, recv in [
        ("recv-constref", "const D%(S)s &in d"),
        ("recv-consthandleref", "const D%(S)s@ &in d"),
        ("recv-constvalue", "const D%(S)s d"),
    ]:
        v.append(V(
            id=rid,
            code=(
                "class B%(S)s { void Mutate() {} }\n"
                "class D%(S)s : B%(S)s {}\n"
                f"void Use%(S)s({recv}) {{ d.Mutate(); }}\n"
            ),
            question="Q-A-spelling",
            expect="UNKNOWN",
            stands_for="the receiver spellings B116 already pairs",
        ))

    # ---- Q-B: element constness -----------------------------------------
    # The discriminator: own-class non-const method behind a const array.
    # ctl-own-nonconst fixes the own-class answer, so a disagreement here is
    # element constness and nothing else.
    v.append(V(
        id="arr-own-nonconst",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { arr[0].Mutate(); }\n"
        ),
        question="Q-B-discriminator",
        expect="UNKNOWN",
        stands_for="isolates element constness from the parent walk",
    ))
    v.append(V(
        id="arr-inherited-nonconst",
        code=(
            "class A%(S)s { void Mutate() {} }\n"
            "class B%(S)s : A%(S)s {}\n"
            "class D%(S)s : B%(S)s {}\n"
            "void Use%(S)s(const D%(S)s@[]@ arr) { arr[0].Mutate(); }\n"
        ),
        question="Q-B",
        expect="UNKNOWN",
        stands_for="tm-map-together Socket.as:467, the one fleet site, exactly",
    ))
    v.append(V(
        id="arr-value-array",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const C%(S)s[] arr) { arr[0].Mutate(); }\n"
        ),
        question="Q-B",
        expect="UNKNOWN",
        stands_for="`const T[]` -- the value-array spelling",
    ))
    v.append(V(
        id="arr-generic-spelling",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const array<C%(S)s@>@ arr) { arr[0].Mutate(); }\n"
        ),
        question="Q-B",
        expect="UNKNOWN",
        stands_for="`const array<T@>@` -- must agree with the `T@[]@` shorthand",
    ))
    # ---- Q-C: does the const on the ARRAY still bite anywhere? ----------
    # Without these, an ACCEPT above is unattributable between "the element
    # is not const" and "the const on the array is being ignored wholesale".
    # If `arr-mutate-array` REJECTS, the array itself IS const and the
    # element ACCEPTs are genuinely about ELEMENT constness.
    v.append(V(
        id="arrq-mutate-array",
        code=(
            "class C%(S)s {}\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { arr.InsertLast(null); }\n"
        ),
        question="Q-C-anchor",
        expect="UNKNOWN",
        stands_for="mutating the const array ITSELF; anchors that const is live",
    ))
    v.append(V(
        id="arrq-assign-element",
        code=(
            "class C%(S)s {}\n"
            "void Use%(S)s(const C%(S)s@[]@ arr, C%(S)s@ c) { @arr[0] = c; }\n"
        ),
        question="Q-C",
        expect="UNKNOWN",
        stands_for="handle-assign INTO a const handle-array element slot",
    ))
    v.append(V(
        id="arrq-write-field",
        code=(
            "class C%(S)s { int x; }\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { arr[0].x = 1; }\n"
        ),
        question="Q-C",
        expect="UNKNOWN",
        stands_for="writing a FIELD through a const handle-array element",
    ))
    v.append(V(
        id="arrq-assign-value-element",
        code=(
            "class C%(S)s {}\n"
            "void Use%(S)s(const C%(S)s[] arr) { arr[0] = C%(S)s(); }\n"
        ),
        question="Q-C",
        expect="UNKNOWN",
        stands_for="assigning into a const VALUE-array element slot",
    ))
    # The element's STATIC TYPE, which is what decides whether the parser's
    # `const`-binding for the `T@[]@` shorthand is right. If the elements were
    # `const C@` (handle-to-const, which is what the parser currently builds),
    # binding one to a plain `C@` would strip const and the engine would
    # refuse. Reading it into a `const C@` must always work.
    v.append(V(
        id="arrq-elem-to-mutable-handle",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { C%(S)s@ e = arr[0]; e.Mutate(); }\n"
        ),
        question="Q-D-elem-type",
        expect="UNKNOWN",
        stands_for="does an element bind to a NON-const handle (const not on pointee)?",
    ))
    v.append(V(
        id="arrq-elem-to-const-handle",
        code=(
            "class C%(S)s { void Peek() const {} }\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { const C%(S)s@ e = arr[0]; e.Peek(); }\n"
        ),
        question="Q-D-elem-type",
        expect="ACCEPT",
        stands_for="reading an element into a const handle must always work",
    ))
    # `const T@[]` -- a const handle-array with NO outer handle. B119's
    # rebind changes this spelling too, and the first pass REASONED it from
    # the `const T@[]@` result rather than measuring it. Measured now.
    v.append(V(
        id="arrq-noouter-elem-method",
        code=(
            "class C%(S)s { void Mutate() {} }\n"
            "void Use%(S)s(const C%(S)s@[] arr) { arr[0].Mutate(); }\n"
        ),
        question="Q-E-no-outer-handle",
        expect="UNKNOWN",
        stands_for="`const T@[]`: is the ELEMENT pointee mutable, as with `T@[]@`?",
    ))
    v.append(V(
        id="arrq-noouter-mutate-array",
        code=(
            "class C%(S)s {}\n"
            "void Use%(S)s(const C%(S)s@[] arr) { arr.InsertLast(null); }\n"
        ),
        question="Q-E-anchor",
        expect="UNKNOWN",
        stands_for="`const T@[]`: is the ARRAY itself const? anchors the row above",
    ))
    v.append(V(
        id="arr-const-method",
        code=(
            "class C%(S)s { void Peek() const {} }\n"
            "void Use%(S)s(const C%(S)s@[]@ arr) { arr[0].Peek(); }\n"
        ),
        question="Q-B-guard",
        expect="ACCEPT",
        stands_for="a const method through a const array must always compile",
    ))
    return v


def write_fixture(root: Path, v: V) -> Path:
    """One plugin folder per variant (B068: one construct per compilation unit)."""
    suffix = TOKEN + v.id.replace("-", "")
    pretty = f"B119cr {v.id}"
    # `probe()` derives the folder id as PROBE_PREFIX + folder_id(name), so the
    # name must not already carry the prefix.
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
        f"// B119 const-receiver probe :: {v.id}\n"
        f"// question: {v.question}\n"
        f"// stands in for: {v.stands_for}\n"
        + (v.code % {"S": suffix})
        + "\nvoid Main() {\n"
        f'    print("{TOKEN} {v.id} ready");\n'
        "}\n"
    )
    return plugin


def classify(rec: dict) -> tuple[str, str]:
    """ACCEPT / REJECT / BUILD-ERR / AMBIGUOUS, from B118's four-way status.

    This deliberately delegates to `issue_repro_game.probe`'s own
    `build_status` rather than re-deriving a verdict here. The first version
    of this probe DID re-derive one, requiring `Starting build` plus
    `Loaded plugin` from an unfiltered window -- and that was still wrong,
    because the engine logs `Loaded plugin` even after
    `Script compilation failed!`, so the load line is NECESSARY BUT NOT
    SUFFICIENT. B118 added that check (`fleet.BUILD_FAILED`) along with the
    NOT-RUN / BUILD-ERR split, and its verdict chain is mutation-tested
    (`just probe-cleanup-mutants`, 21 mutations, 0 missed) where a
    hand-rolled copy here would not be.

    The mapping keeps every distinction B118 draws:

    * CLEAN     -> ACCEPT      the engine compiled and loaded it.
    * REJECTED  -> REJECT      a located error, or `Script compilation failed!`.
    * BUILD-ERR -> BUILD-ERR   an engine error that is NOT a verdict about the
                               construct (missing dependency, no load line).
                               Never folded into REJECT: that collapse is the
                               original B086 citation failure.
    * NOT-RUN   -> AMBIGUOUS   the build never happened, so silence is not
                               evidence of acceptance. On THIS question that
                               is the dangerous direction, because ACCEPT is
                               one of the two readings being distinguished.
    """
    if rec.get("skipped"):
        return "AMBIGUOUS", rec["skipped"]
    if rec.get("contended"):
        return "AMBIGUOUS", (
            f"log window contended: {len(rec.get('foreign_errors', []))} foreign "
            f"and {len(rec.get('unattributed_errors', []))} unattributed ERR"
        )
    status = rec.get("build_status")
    if status == "REJECTED":
        errs = rec.get("game_errors", [])
        if errs:
            return "REJECT", "; ".join(
                f"{e['file']}({e['line']},{e['col']}): {e['msg']}" for e in errs[:3]
            )
        return "REJECT", "Script compilation failed! (no located diagnostic)"
    if status == "BUILD-ERR":
        raw = rec.get("raw_build_errors") or []
        detail = "; ".join(r.split("]", 3)[-1].strip() for r in raw[:2])
        return "BUILD-ERR", detail or "build error with no verdict about the code"
    if status == "NOT-RUN":
        return "AMBIGUOUS", (
            "the build never ran, so an absence of errors is not an ACCEPT"
        )
    if status == "CLEAN":
        return "ACCEPT", "compiled and loaded, no error attributed to this folder"
    return "AMBIGUOUS", f"unrecognised build_status {status!r}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--json")
    ap.add_argument("--only", help="run only variants whose id starts with this")
    args = ap.parse_args()

    vs = variants()
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

    tmp = Path(tempfile.mkdtemp(prefix="b119cr-"))
    rows = []
    try:
        for v in vs:
            fixture = write_fixture(tmp, v)
            # No injected window: `probe()` now defaults to
            # `fleet.read_log_window_raw`, so calling it plainly exercises the
            # REAL reader. An injected one would test only the channel handed
            # in -- which is exactly how B118's selftest missed that the
            # default was still the filtered reader.
            rec = repro.probe(fixture)
            verdict, reason = classify(rec)
            rows.append({
                "id": v.id, "question": v.question, "expect": v.expect,
                "verdict": verdict, "reason": reason,
                "build_status": rec.get("build_status", ""),
                "folder_id": rec.get("folder_id", ""),
                "restored": rec.get("restored"),
                "stands_for": v.stands_for,
            })
            flag = ""
            if v.expect != "UNKNOWN":
                flag = "  <-- CONTROL OK" if verdict == v.expect else \
                       f"  <-- CONTROL FAILED (expected {v.expect})"
            print(f"{verdict:10s} {v.id:24s} {v.question:20s} "
                  f"{reason[:70]}{flag}", flush=True)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    left = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    print()
    print(f"B077 sweep: {'clean' if not left else 'STRANDED: ' + repr(left)}")

    controls = [r for r in rows if r["expect"] != "UNKNOWN"]
    bad = [r for r in controls if r["verdict"] != r["expect"]]
    if bad:
        print("CONTROLS FAILED -- the run is not evidence:", file=sys.stderr)
        for r in bad:
            print(f"  {r['id']}: got {r['verdict']}, expected {r['expect']}"
                  f" :: {r['reason']}", file=sys.stderr)

    if args.tsv:
        p = Path(args.tsv)
        p.parent.mkdir(parents=True, exist_ok=True)
        cols = ["id", "question", "expect", "verdict", "build_status",
                "reason", "stands_for"]
        with p.open("w") as fh:
            fh.write("\t".join(cols) + "\n")
            for r in rows:
                fh.write("\t".join(str(r[c]).replace("\t", " ") for c in cols) + "\n")
        print(f"wrote {p}")
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=2))

    return 1 if (bad or left) else 0


if __name__ == "__main__":
    raise SystemExit(main())
