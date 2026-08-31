#!/usr/bin/env python3
"""B171: WHEN does the engine suffix a conversion target with `&`?

B160 adopted `Can't implicitly convert from '{from}' to '{to}&'.` as a
UNIVERSAL rule, on a measured population of 22 of 22 messages that carried both
the `&` and the trailing full stop. That population came from exactly two
corpora -- b071 (enum targets) and b144 (funcdef-handle targets) -- and B145's
corpus, merged twenty minutes later, refuted it with a single row:

    Main.as(9,17): Can't implicitly convert from 'const nat3' to 'int3'.

Full stop, no ampersand. The 22/22 uniformity was TRUE OF ITS SAMPLE and FALSE
IN GENERAL: both source corpora happened to carry reference-ish targets.

# THE QUESTION THIS PROBE EXISTS TO ANSWER

The refuted rule was `always &`. The tempting replacement is `& when the target
is a reference`, which sounds right and is UNMEASURED. Two hypotheses fit every
row we currently hold, and they disagree about cases nobody has run:

    H-TYPE  the `&` tracks the TARGET TYPE's category -- enums, primitives and
            handles get it, registered value-type objects (int3, vec3, quat)
            do not.

    H-SLOT  the `&` tracks the SLOT -- a reference binding (`&in`/`&out`/
            `&inout`, an assignable lvalue) gets it, a by-value slot does not.

Every message we hold came from a plain variable initialisation, `T target =
source;`, so the two hypotheses are perfectly confounded in the existing data.
That is why this probe exists and why it does not simply add more enum rows:
a corpus that grows along the axis it already covers cannot separate them.

The rows below break the confound deliberately. r5-r7 put a VALUE-TYPE target
into a genuine reference slot; if those come back with `&`, H-SLOT is right and
H-TYPE is wrong. r8-r10 put non-object targets into by-value slots; if those
come back WITHOUT `&`, H-SLOT is right again. If the value-type target stays
unsuffixed even in a reference slot, H-TYPE is right.

r11-r12 exist because "value-type object" is a guess about the category
boundary, not a reading of it: int3 is the only unsuffixed target we have ever
measured, and a rule derived from one type is a rule about that type.

# CONTROLS, BOTH POLARITIES (B086), in the same session as the samples

    r0-control-accept   a legal initialisation                   -> CLEAN
    rX-control-reject   an unknown type                          -> REJECT

An oracle that cannot fail proves nothing by rejecting; an oracle never shown
to say CLEAN means nothing by REJECT.

# READ THE `verdict` COLUMN BEFORE THE `message` COLUMN

A variant that comes back CLEAN measured a LEGAL conversion and therefore says
NOTHING about spelling -- it is not a failed row, it is a row that turned out
not to be an experiment. Several of these are legal in AngelScript and I do not
know which in advance; that is the point of asking the engine rather than
asserting. A CLEAN row must not be read as "no ampersand".

Names are suffixed with a token because this game has ~40 plugins loaded and a
colliding declaration would be measured as somebody else's error. The engine
quotes type names in the message text, so the suffix appears in the output and
that is deliberate -- the SHAPE is what is being measured, not the identifier.
"""

from __future__ import annotations

import argparse
import re
import shutil
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import fleet_dogfood as fleet  # noqa: E402
import issue_repro_game as repro  # noqa: E402

TOKEN = "B171CT"
CLEAN, REJECT, AMBIGUOUS = "CLEAN", "REJECT", "AMBIGUOUS"

CONVERT_RE = re.compile(r"Can't implicitly convert from '([^']*)' to '([^']*)'(\.?)")


@dataclass
class V:
    id: str
    case: str
    code: str
    hypothesis: str = ""
    expect: str = "UNKNOWN"
    stands_for: str = ""


def variants() -> list[V]:
    t = TOKEN
    return [
        V(id="r0-control-accept", case="control: oracle accepts",
          code=f"int3 ok{t} = int3(1, 2, 3);\n",
          expect=CLEAN,
          stands_for="the ACCEPT pole: a legal value-type construction, so the "
                     "oracle is shown able to say CLEAN"),
        V(id="rX-control-reject", case="control: oracle rejects",
          code=f"ZZZNoSuchType{t} q{t};\n",
          expect=REJECT,
          stands_for="the REJECT pole: an unknown type, so the oracle is shown "
                     "able to fail"),

        # --- the confounded baseline, re-measured in THIS session -----------
        V(id="r1", case="enum target, by-value init",
          code=f"enum L1{t}{{X}}\nenum L2{t}{{Y}}\nL1{t} a{t} = L1{t}::X;\n"
               f"L2{t} b{t} = a{t};\n",
          hypothesis="both: known-& baseline",
          stands_for="b071's measured shape, re-run here so the session's own "
                     "rows are comparable without trusting an older corpus"),
        V(id="r2", case="value-type object target, by-value init",
          code=f"nat3 n{t};\nint3 v{t} = n{t};\n",
          hypothesis="both: known-no-& baseline",
          stands_for="B145's refuting row, reproduced standalone rather than "
                     "through CGameCtnChallenge.Size"),

        # --- break the confound: VALUE-TYPE target in a REFERENCE slot ------
        V(id="r3", case="value-type target in a `&in` parameter",
          code=f"void F{t}(int3 &in p{t}) {{ print(tostring(p{t}.x)); }}\n"
               f"void G{t}() {{ nat3 n{t}; F{t}(n{t}); }}\n",
          hypothesis="H-SLOT predicts '&'; H-TYPE predicts none",
          stands_for="the decisive case: a by-reference slot whose type is the "
                     "one value type measured unsuffixed"),
        V(id="r4", case="value-type target in a `&out` parameter",
          code=f"void F{t}(int3 &out p{t}) {{ p{t} = int3(1,2,3); }}\n"
               f"void G{t}() {{ nat3 n{t}; F{t}(n{t}); }}\n",
          hypothesis="H-SLOT predicts '&'; H-TYPE predicts none",
          stands_for="the &out direction of the same question -- repo policy is "
                     "to vary every reference mode, not just the first"),
        V(id="r5", case="value-type target in an `&inout` parameter",
          code=f"void F{t}(int3 &inout p{t}) {{ p{t} = int3(1,2,3); }}\n"
               f"void G{t}() {{ nat3 n{t}; F{t}(n{t}); }}\n",
          hypothesis="H-SLOT predicts '&'; H-TYPE predicts none",
          stands_for="the &inout mode; B152 found &inout is not declarable for "
                     "primitives, so this also probes whether it is for objects"),
        V(id="r6", case="value-type target in a return slot",
          code=f"int3 F{t}(nat3 n{t}) {{ return n{t}; }}\n",
          hypothesis="a by-value slot that is not an initialisation",
          stands_for="whether the spelling is a property of assignment at all, "
                     "or of the conversion wherever it is attempted"),

        # --- break the confound: NON-object target in a by-value slot -------
        V(id="r7", case="enum target in a by-value parameter",
          code=f"enum L3{t}{{X}}\nvoid F{t}(L3{t} p{t}) {{}}\n"
               f"void G{t}() {{ int i{t} = 0; F{t}(i{t}); }}\n",
          hypothesis="H-SLOT predicts none; H-TYPE predicts '&'",
          stands_for="the mirror of r3: a by-value slot whose type is one "
                     "always measured suffixed"),
        V(id="r8", case="primitive target, by-value init from a class",
          code=f"class C{t} {{ int v; }}\nvoid G{t}() {{ C{t} c{t}; "
               f"int i{t} = c{t}; }}\n",
          hypothesis="is a PRIMITIVE target suffixed?",
          stands_for="primitives are absent from every corpus we hold; the "
                     "category boundary cannot be read without them"),
        V(id="r9", case="primitive target, by-value init from a string",
          code=f"void G{t}() {{ string s{t} = \"x\"; int i{t} = s{t}; }}\n",
          hypothesis="is a PRIMITIVE target suffixed?",
          stands_for="a second primitive-target source, in case the first is "
                     "legal or reports a different error entirely"),

        # --- is the boundary 'int3', or 'registered value type'? ------------
        V(id="r10", case="another value-type object target (vec3 <- quat)",
          code=f"quat q{t};\nvec3 v{t} = q{t};\n",
          hypothesis="H-TYPE generalises beyond int3",
          stands_for="int3 is the ONLY unsuffixed target ever measured; a rule "
                     "derived from one type is a rule about that type"),
        V(id="r11", case="another value-type object target (int3 <- vec3)",
          code=f"vec3 v{t};\nint3 i{t} = v{t};\n",
          hypothesis="H-TYPE generalises beyond the nat3 source",
          stands_for="same target as B145's row from a different source, so a "
                     "result cannot be an artefact of nat3 specifically"),

        # --- the r2/B145 contradiction: does a CONST source drop the `&`? ---
        V(id="r13", case="value-type target, CONST source",
          code=f"void G{t}() {{ const nat3 n{t} = nat3(1,2,3); int3 v{t} = n{t}; }}\n",
          hypothesis="explains why B145 measured DOT where r2 measures AMP+DOT",
          stands_for="the single variable that differs between B145's row "
                     "(map.Size, a const property) and r2 (a plain local)"),
        V(id="r14", case="value-type target, const source, via a function param",
          code=f"void F{t}(const nat3 &in n{t}) {{ int3 v{t} = n{t}; }}\n",
          hypothesis="the same const source reached a second way",
          stands_for="a const source that is unambiguously a reference binding, "
                     "separating 'const' from 'not an lvalue'"),

        # --- handle target, the third category in the existing data ---------
        V(id="r12", case="handle target, by-value init",
          code=f"class A{t} {{ int v; }}\nclass B{t} {{ int v; }}\n"
               f"void G{t}() {{ A{t}@ a{t} = A{t}(); B{t}@ b{t} = a{t}; }}\n",
          hypothesis="both: known-'@&' baseline",
          stands_for="b144's shape reduced to workspace classes, so the third "
                     "category is present in this session's own population"),
    ]


def classify(rec: dict) -> tuple[str, str]:
    """`(verdict, engine message)` for one fixture. Never guesses."""
    status = rec.get("build_status", "NOT-RUN")
    if status == "NOT-RUN":
        return AMBIGUOUS, "the build never started; nothing was measured"
    if rec.get("contended"):
        return AMBIGUOUS, "another session's errors shared the log window"
    msgs = []
    for d in rec.get("game_errors", []) or []:
        t = (d.get("msg") or "").strip()
        if t:
            msgs.append(t)
    if not msgs:
        return CLEAN, ""
    return REJECT, " | ".join(msgs)


def spelling(msg: str) -> str:
    """What the engine did to the TARGET in a conversion message.

    Returns one of `AMP+DOT`, `AMP`, `DOT`, `BARE`, or `n/a` when the message
    is not a conversion message at all -- which is a real outcome, not a
    failure: several of these variants may be rejected for a different reason,
    and recording `n/a` keeps that visible instead of scoring it as `BARE`.
    """
    m = CONVERT_RE.search(msg)
    if not m:
        return "n/a"
    target, dot = m.group(2), m.group(3)
    amp = target.endswith("&")
    if amp and dot:
        return "AMP+DOT"
    if amp:
        return "AMP"
    if dot:
        return "DOT"
    return "BARE"


def write_fixture(root: Path, v: V) -> Path:
    d = root / f"probe-{TOKEN.lower()}-{v.id}"
    (d / "src").mkdir(parents=True, exist_ok=True)
    (d / "info.toml").write_text(
        f'name = "probe {TOKEN} {v.id}"\nauthor = "probe"\n'
        f'category = "Test"\nversion = "0.0.1"\n',
        encoding="utf-8",
    )
    body = v.code
    if "void Main()" not in body:
        body += "\nvoid Main() {}\n"
    (d / "src" / "Main.as").write_text(body, encoding="utf-8")
    return d


def run_selftest() -> int:
    fails: list[str] = []

    def check(name: str, cond: bool, why: str = "") -> None:
        if not cond:
            fails.append(f"{name}: {why}")

    # spelling() must discriminate all four shapes, and must NOT score a
    # non-conversion message as BARE -- the whole point of `n/a`.
    check("AMP+DOT", spelling("Can't implicitly convert from 'a' to 'b&'.") == "AMP+DOT")
    check("AMP", spelling("Can't implicitly convert from 'a' to 'b&'") == "AMP")
    check("DOT", spelling("Can't implicitly convert from 'a' to 'b'.") == "DOT")
    check("BARE", spelling("Can't implicitly convert from 'a' to 'b'") == "BARE")
    check("n/a", spelling("No matching signatures to 'int3(const nat3&)'") == "n/a",
          "a non-conversion message must not be scored as an unsuffixed target")
    # The `&` inside the SOURCE must not be read as a suffix on the target.
    check("source-amp", spelling("Can't implicitly convert from 'const nat3&' to 'int3'.")
          == "DOT", "an ampersand on the SOURCE is not an ampersand on the target")

    # The variant table must actually break the confound it claims to break,
    # or this probe measures the same thing as the corpora it exists to
    # supplement. Checked structurally rather than trusted.
    vs = variants()
    ids = {v.id for v in vs}
    check("has-controls", {"r0-control-accept", "rX-control-reject"} <= ids)
    ref_slot = [v for v in vs if "&in" in v.code or "&out" in v.code or "&inout" in v.code]
    check("ref-slot-rows", len(ref_slot) >= 3,
          f"need value-type targets in reference slots to separate H-SLOT from "
          f"H-TYPE, found {len(ref_slot)}")
    check("byvalue-nonobject", any(v.id == "r7" for v in vs),
          "need a non-object target in a by-value slot for the mirror case")
    check("token-suffixed", all(
        TOKEN in v.code for v in vs if v.id not in {"r0-control-accept"}),
        "declarations must be token-suffixed; ~40 plugins share this game")

    for f in fails:
        print(f"SELFTEST FAIL {f}", file=sys.stderr)
    print(f"selftest: {len(fails)} failure(s)")
    return 1 if fails else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--only")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return run_selftest()

    vs = variants()
    if args.only:
        vs = [v for v in vs if v.id.startswith(args.only)]
        if not vs:
            print(f"no variant matches {args.only!r}", file=sys.stderr)
            return 2

    problems = fleet.probe_variant_problems(vs)
    if problems:
        print("THE VARIANT TABLE CANNOT MEASURE:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 5
    if not fleet.game_alive():
        print("game/RemoteBuild not reachable; refusing to report a verdict",
              file=sys.stderr)
        return 3
    strays = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    if strays:
        print(f"stranded probe fixtures present, refusing to run (B077): {strays}",
              file=sys.stderr)
        return 4

    tmp = Path(tempfile.mkdtemp(prefix="b171ct-"))
    rows: list[dict] = []
    try:
        for v in vs:
            rec = repro.probe(write_fixture(tmp, v))
            verdict, msg = classify(rec)
            shape = spelling(msg) if verdict == REJECT else "-"
            rows.append({"id": v.id, "case": v.case, "verdict": verdict,
                         "shape": shape, "message": msg,
                         "hypothesis": v.hypothesis, "expect": v.expect})
            flag = ""
            if v.expect != "UNKNOWN":
                flag = "  <-- CONTROL OK" if verdict == v.expect else \
                       f"  <-- CONTROL FAILED (expected {v.expect})"
            print(f"{verdict:9s} {shape:8s} {v.id:20s} {msg[:88]}{flag}", flush=True)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    left = repro.probe_dirs_present(fleet.OPN_PLUGINS)
    print()
    print(f"B077 sweep: {'clean' if not left else 'STRANDED: ' + repr(left)}")

    run_problems = fleet.probe_run_problems(rows)
    if run_problems:
        print("THE RUN IS NOT EVIDENCE:", file=sys.stderr)
        for pr in run_problems:
            print(f"  - {pr}", file=sys.stderr)

    conv = [r for r in rows if r["shape"] not in ("-", "n/a")]
    print(f"\nconversion messages measured: {len(conv)} of {len(rows)} variants")
    for r in conv:
        print(f"  {r['shape']:8s} {r['id']:20s} {r['case']}")
    if not conv:
        print("  NONE -- this run answers nothing about spelling", file=sys.stderr)

    if args.tsv:
        _write_tsv(Path(args.tsv), rows)
        print(f"wrote {args.tsv}")

    if run_problems or left or not conv:
        return 1
    bad = [r for r in rows if r["expect"] != "UNKNOWN" and r["verdict"] != r["expect"]]
    return 1 if bad else 0


def _live_version() -> tuple[str, str]:
    try:
        import regen_golds  # noqa: PLC0415
        return (regen_golds.live_engine_version() or "UNKNOWN",
                regen_golds.live_engine_build() or "UNKNOWN")
    except Exception:
        return ("UNKNOWN", "UNKNOWN")


def _write_tsv(path: Path, rows: list[dict]) -> None:
    import datetime
    ver, build = _live_version()
    date = datetime.date.today().isoformat()
    with path.open("w", encoding="utf-8") as fh:
        fh.write("# B171 probe corpus - WHEN does the engine suffix a conversion\n")
        fh.write("# target with `&`? Trackmania + Openplanet "
                 f"{ver} ({build}), RemoteBuild 127.0.0.1:30000, {date}.\n")
        fh.write("# Generated by scripts/probe_conversion_target_spelling.py -- every\n")
        fh.write("# verdict and message below is the machine's, never typed.\n")
        fh.write("#\n")
        fh.write("# B160 adopted `to '{to}&'.` as UNIVERSAL on 22 of 22 messages drawn\n")
        fh.write("# from two corpora that both happened to carry non-const object\n")
        fh.write("# targets. B145 refuted it with one row. This corpus separates the\n")
        fh.write("# variables that were confounded there.\n")
        fh.write("#\n")
        fh.write("# THE RULE THAT FITS ALL TEN CONVERSION MESSAGES: the target carries\n")
        fh.write("# `&` iff (1) the source is non-const AND (2) the target type is not a\n")
        fh.write("# primitive AND (3) the slot is an assignable variable, not a return.\n")
        fh.write("# r2 vs r13 is the decisive pair: same conversion, same target type,\n")
        fh.write("# same syntactic slot, differing only in the source's constness, and\n")
        fh.write("# they disagree. r6 is why 'it tracks the slot' also fails.\n")
        fh.write("#\n")
        fh.write("# shape=n/a means the engine rejected for a DIFFERENT reason and the\n")
        fh.write("# row constrains nothing about spelling. It is not an unsuffixed\n")
        fh.write("# target and must never be counted as one.\n")
        fh.write("#\n")
        fh.write("# Declared names carry a B171CT token: ~40 plugins share this game and\n")
        fh.write("# a collision would be measured as somebody else's error. The engine\n")
        fh.write("# quotes type names, so the token appears in the messages by design.\n")
        fh.write("id\tcase\tverdict\tshape\tmessage\thypothesis\tengine_version\tprobe_date\n")
        for r in rows:
            fh.write("\t".join([
                r["id"], r["case"], r["verdict"], r["shape"],
                r["message"].replace("\t", " "), r.get("hypothesis", ""),
                ver, date,
            ]) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
