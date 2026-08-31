#!/usr/bin/env python3
"""B117 — what does the engine do with `private` / `protected` members at each
ACCESS SITE?

    scripts/probe_member_visibility.py [--tsv out.tsv] [--json out.json]
                                       [--only <id-prefix>]

Requires Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.

The question
------------

B115 gave the parser visibility flags and B114 pooled them; nothing reads them.
B117 is the consumer: member completion must stop offering members the engine
would refuse. Getting that wrong in the STRICT direction is worse than today's
permissiveness -- a completion that hides a member you are allowed to use is a
false negative the user cannot diagnose, where today's over-offering is at
least uniform and predictable.

`private` in AngelScript is not necessarily C++'s `private`, so the filter's
rule is measured here rather than assumed from another language.

The axes, and why each one is present
-------------------------------------

* ACCESS SITE is the whole problem: inside the declaring class, inside a
  DERIVED class, and from an unrelated function. The derived site is the ONLY
  one where `protected` and `private` differ, so without it a filter that
  conflates the two keywords passes everything.
* MEMBER KIND -- field, method, property, constructor -- because the parser
  records visibility on three different AST nodes and the pool stores it
  through two different helpers (`state_modifiers`, `fn_modifiers`). A rule
  true of methods and false of fields would otherwise ship.
* THE PUBLIC ROW AT EVERY SITE, because a filter that hides too much passes a
  suite that only checks that things disappear.
* CROSS-FILE, this repo's recurring shape (B116, B120). An AngelScript class is
  module-scoped rather than file-scoped, so the expectation is that the file
  boundary is irrelevant -- which is exactly the kind of expectation worth one
  cheap row rather than an assumption.

Engine members are NOT probed and cannot be: B131 measured that the typedb
carries no visibility data at all (0 of 1002 methods in 1.29.5, 0 of 975 in
1.29.14; the strings "private" and "protected" appear nowhere in either dump).
Visibility filtering is a workspace-half-only feature by measurement.

The oracle
----------

Compile-only, so this follows `probe_const_receiver.py` and
`probe_shared_modifiers.py`: one plugin folder per variant (B068), staged via
`issue_repro_game.probe` for B077 cleanup, verdict delegated to B118's
four-way `build_status` so a build that never ran cannot read as an ACCEPT.
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
# B139: IMPORTED, not copied. This file previously carried a
# hand-rolled duplicate of `classify` whose docstring claimed to
# delegate. A copy sits outside the mutation coverage the shared
# one has (`just probe-cleanup-mutants`), so a future fix to the
# classifier would silently not reach this probe -- the same
# two-copies-of-one-primitive shape B118 found in the log readers.
from probe_const_receiver import classify  # noqa: E402

TOKEN = "B117MV"


@dataclass
class V:
    id: str
    code: str                 # goes in Main.as; %(S)s is the unique suffix
    extra: str = ""           # optional second file (Other.as)
    question: str = ""
    expect: str = "UNKNOWN"
    stands_for: str = ""
    fid: str = ""


# `%(S)s` is appended to every declared name so nothing collides with the ~40
# plugins loaded in the shared game.
MEMBERS = {
    # kind      declaration                          access expression
    "field":    ("{vis}int mem%(S)s;",                "o.mem%(S)s = 1;"),
    "method":   ("{vis}void mem%(S)s() {}",           "o.mem%(S)s();"),
    "property": ("{vis}int mem%(S)s { get { return 1; } }", "int t = o.mem%(S)s;"),
}
VIS = {"private": "private ", "protected": "protected ", "public": ""}


def owner(vis: str, kind: str) -> str:
    decl = MEMBERS[kind][0].replace("{vis}", VIS[vis])
    return "class Own%(S)s {\n    " + decl + "\n}\n"


def access(kind: str, recv: str) -> str:
    return MEMBERS[kind][1].replace("o.", f"{recv}.")


def variants() -> list[V]:
    v: list[V] = []

    # ── controls ────────────────────────────────────────────────────────
    v.append(V(
        id="ctl-public-unrelated",
        code=owner("public", "method") + "void Use%(S)s(Own%(S)s@ o) { " + access("method", "o") + " }\n",
        question="control", expect="ACCEPT",
        stands_for="a public member from an unrelated function",
    ))
    v.append(V(
        id="ctl-bogus",
        code="class Own%(S)s {}\nvoid Use%(S)s(Own%(S)s@ o) { o.nope%(S)s(); }\n",
        question="control", expect="REJECT",
        stands_for="a member that does not exist; proves the oracle sees errors",
    ))

    # ── site 1: inside the DECLARING class ──────────────────────────────
    # A private member must remain usable where it was declared. This is the
    # negative control the ticket names: a filter that hides it here is worse
    # than no filter.
    for kind in MEMBERS:
        for vis in ("private", "protected"):
            body = MEMBERS[kind][1].replace("o.", "this.")
            v.append(V(
                id=f"own-{vis}-{kind}",
                code=(
                    "class Own%(S)s {\n    "
                    + MEMBERS[kind][0].replace("{vis}", VIS[vis])
                    + "\n    void Touch%(S)s() { " + body + " }\n}\n"
                ),
                question="site:own", expect="ACCEPT",
                stands_for=f"a {vis} {kind} used inside its own class",
            ))

    # ── site 2: inside a DERIVED class ──────────────────────────────────
    # The only site where `protected` and `private` can differ.
    for kind in MEMBERS:
        for vis in ("private", "protected", "public"):
            body = MEMBERS[kind][1].replace("o.", "this.")
            v.append(V(
                id=f"derived-{vis}-{kind}",
                code=(
                    "class Base%(S)s {\n    "
                    + MEMBERS[kind][0].replace("{vis}", VIS[vis])
                    + "\n}\n"
                    "class Derived%(S)s : Base%(S)s {\n"
                    "    void Touch%(S)s() { " + body + " }\n}\n"
                ),
                question="site:derived",
                stands_for=f"an inherited {vis} {kind} from a subclass",
            ))

    # ── site 3: an UNRELATED function ───────────────────────────────────
    for kind in MEMBERS:
        for vis in ("private", "protected", "public"):
            v.append(V(
                id=f"unrelated-{vis}-{kind}",
                code=(
                    owner(vis, kind)
                    + "void Use%(S)s(Own%(S)s@ o) { " + access(kind, "o") + " }\n"
                ),
                question="site:unrelated",
                expect="ACCEPT" if vis == "public" else "UNKNOWN",
                stands_for=f"a {vis} {kind} from an unrelated function",
            ))

    # ── another INSTANCE of the same class, from inside the class ───────
    # C++ allows this; whether AngelScript scopes `private` per-class or
    # per-object is not something to assume.
    v.append(V(
        id="sibling-instance-private-field",
        code=(
            "class Own%(S)s {\n    private int mem%(S)s;\n"
            "    void Touch%(S)s(Own%(S)s@ other) { other.mem%(S)s = 1; }\n}\n"
        ),
        question="site:sibling-instance",
        stands_for="a private field of ANOTHER instance, from inside the class",
    ))

    # ── confirming the derived/private FIELD result ─────────────────────
    # `derived-private-field` ACCEPTS while `derived-private-method` REJECTS,
    # which is counter-intuitive enough to be worth three more rows rather
    # than one. If it were an artifact of the write-through-`this` spelling,
    # these would disagree with it.
    v.append(V(
        id="derivedx-private-field-read",
        code=(
            "class Base%(S)s {\n    private int mem%(S)s;\n}\n"
            "class Derived%(S)s : Base%(S)s {\n"
            "    int Touch%(S)s() { return this.mem%(S)s; }\n}\n"
        ),
        question="site:derived-confirm",
        stands_for="READING an inherited private field, not writing it",
    ))
    v.append(V(
        id="derivedx-private-field-bare",
        code=(
            "class Base%(S)s {\n    private int mem%(S)s;\n}\n"
            "class Derived%(S)s : Base%(S)s {\n"
            "    int Touch%(S)s() { return mem%(S)s; }\n}\n"
        ),
        question="site:derived-confirm",
        stands_for="the bare-name spelling, without `this.`",
    ))
    v.append(V(
        id="derivedx-private-field-via-base-handle",
        code=(
            "class Base%(S)s {\n    private int mem%(S)s;\n}\n"
            "class Derived%(S)s : Base%(S)s {\n"
            "    int Touch%(S)s(Base%(S)s@ b) { return b.mem%(S)s; }\n}\n"
        ),
        question="site:derived-confirm",
        stands_for="through an explicit Base@ handle, from inside the subclass",
    ))
    v.append(V(
        id="derivedx-protected-method-bare",
        code=(
            "class Base%(S)s {\n    protected void mem%(S)s() {}\n}\n"
            "class Derived%(S)s : Base%(S)s {\n"
            "    void Touch%(S)s() { mem%(S)s(); }\n}\n"
        ),
        question="site:derived-confirm", expect="ACCEPT",
        stands_for="the protected-method control for the bare spelling",
    ))

    # ── CROSS-FILE ──────────────────────────────────────────────────────
    # An AngelScript class is module-scoped, so the file boundary should not
    # matter. One cheap row per discriminating case rather than an assumption.
    for kind in ("field", "method"):
        for vis in ("private", "protected", "public"):
            v.append(V(
                id=f"crossfile-{vis}-{kind}",
                code="void Use%(S)s(Own%(S)s@ o) { " + access(kind, "o") + " }\n",
                extra=owner(vis, kind),
                question="site:cross-file",
                expect="ACCEPT" if vis == "public" else "UNKNOWN",
                stands_for=f"a {vis} {kind} declared in ANOTHER file of the same plugin",
            ))

    # ── constructors ────────────────────────────────────────────────────
    v.append(V(
        id="ctor-private-from-unrelated",
        code=(
            "class Own%(S)s {\n    private Own%(S)s() {}\n}\n"
            "void Use%(S)s() { Own%(S)s@ o = Own%(S)s(); }\n"
        ),
        question="site:unrelated",
        stands_for="a private constructor called from an unrelated function",
    ))
    v.append(V(
        id="ctor-public-from-unrelated",
        code=(
            "class Own%(S)s {\n    Own%(S)s() {}\n}\n"
            "void Use%(S)s() { Own%(S)s@ o = Own%(S)s(); }\n"
        ),
        question="site:unrelated", expect="ACCEPT",
        stands_for="the public-constructor control for the row above",
    ))
    return v


def write_fixture(root: Path, v: V) -> Path:
    suffix = TOKEN + v.id.replace("-", "")
    pretty = f"B117mv {v.id}"
    v.fid = repro.PROBE_PREFIX + fleet.folder_id(pretty)
    plugin = root / v.id
    plugin.mkdir(parents=True)
    plugin.joinpath("info.toml").write_text(
        "[meta]\n"
        f'name     = "{pretty}"\n'
        'author   = "clankercode"\n'
        'category = "Tests"\n'
        'version  = "0.1.0"\n'
        "\n[script]\n"
    )
    plugin.joinpath("Main.as").write_text(
        f"// B117 member-visibility probe :: {v.id}\n"
        f"// question: {v.question}\n"
        f"// stands in for: {v.stands_for}\n"
        + (v.code % {"S": suffix})
        + "\nvoid Main() {\n"
        f'    print("{TOKEN} {v.id} ready");\n'
        "}\n"
    )
    if v.extra:
        plugin.joinpath("Other.as").write_text(
            f"// B117 probe :: {v.id} (the declaring file)\n"
            + (v.extra % {"S": suffix})
        )
    return plugin


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--json")
    ap.add_argument("--only")
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

    tmp = Path(tempfile.mkdtemp(prefix="b117mv-"))
    rows = []
    try:
        for v in vs:
            rec = repro.probe(write_fixture(tmp, v))
            verdict, reason = classify(rec)
            rows.append({
                "id": v.id, "question": v.question, "expect": v.expect,
                "verdict": verdict, "build_status": rec.get("build_status", ""),
                "reason": reason, "stands_for": v.stands_for,
            })
            flag = ""
            if v.expect != "UNKNOWN":
                flag = "  <-- CONTROL OK" if verdict == v.expect else \
                       f"  <-- CONTROL FAILED (expected {v.expect})"
            print(f"{verdict:10s} {v.id:34s} {reason[:62]}{flag}", flush=True)
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
        cols = ["id", "question", "expect", "verdict", "build_status", "reason",
                "stands_for"]
        with p.open("w") as fh:
            fh.write("\t".join(cols) + "\n")
            for r in rows:
                fh.write("\t".join(str(r[c]).replace("\t", " ") for c in cols) + "\n")
        print(f"wrote {p}")
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=2))
    return 1 if (problems or left) else 0


if __name__ == "__main__":
    raise SystemExit(main())
