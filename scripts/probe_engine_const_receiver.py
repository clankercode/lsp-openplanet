#!/usr/bin/env python3
"""B130 — does the engine enforce const receivers for ENGINE (typedb) types?

    scripts/probe_engine_const_receiver.py [--tsv out.tsv] [--json out.json]
                                           [--only <id-prefix>]

Requires Trackmania + Openplanet with RemoteBuild on 127.0.0.1:30000.
There is ONE game on ONE port and lanes share it — coordinate before running.

The question, and why the fleet could not answer it
---------------------------------------------------

B116 excluded the whole engine-receiver population from
`NonConstMethodOnConstReceiver` by provenance, arguing partly that no
const-qualifier metadata existed. B124 showed that was stale and B125 made the
data arrive, so B130 asks whether to consume it.

The B130 fleet sweep settled the false-positive question and found NOTHING to
fire on: 807 unique engine-receiver call sites across 152 plugins, and the
typedb calls all 807 of them CONST. Zero would-fire, and — the stronger half —
zero UNKNOWN, so the typedb has an answer at every site.

That is the whole false-positive surface measured at zero, but it leaves the
other half open, and the fleet cannot close it: shipped plugins compile, so a
true positive cannot appear in them. Whether the rule would be RIGHT when it
does fire is a question only the engine can answer.

So this probe asks the engine directly, on the exact shapes the rule would
newly diagnose.

Q-E1  does the engine reject a NON-const ENGINE method called through a
      `const T@` handle (reference type)?
Q-E2  the same through `const T &in` (value type)?
Q-E3  does it still ACCEPT a const engine method on a const receiver — the
      other direction, which a rule that fires on everything would also pass?

Controls, without which no row is evidence
------------------------------------------

* `ctl-valid`   a const method on a const WORKSPACE receiver -> ACCEPT.
                Proves the channel can report success.
* `ctl-bogus`   a call to a method that does not exist -> REJECT.
                Proves the channel can SEE an error; without it every ACCEPT
                below could be a dead instrument.
* `ctl-own-nonconst`  a non-const method on the receiver's OWN workspace
                class -> REJECT. Proves the engine enforces const receivers
                AT ALL, so an ACCEPT on an engine row means "engine types are
                different", not "the oracle is broken".

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

TOKEN = "B130ECR"


def write_fixture(root: Path, v: V) -> Path:
    """One plugin folder per variant (B068: one construct per compilation unit).

    Its own TOKEN and pretty name, not B119's — ~40 plugins share the game and
    a folder-id collision would produce a REJECT that is not about constness.
    """
    suffix = TOKEN + v.id.replace("-", "")
    pretty = f"B130ecr {v.id}"
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
        f"// B130 engine const-receiver probe :: {v.id}\n"
        f"// question: {v.question}\n"
        f"// stands in for: {v.stands_for}\n"
        + (v.code % {"S": suffix})
        + "\nvoid Main() {\n"
        f'    print("{TOKEN} {v.id} ready");\n'
        "}\n"
    )
    return plugin


def variants() -> list[V]:
    v: list[V] = []

    # ---- controls -------------------------------------------------------
    v.append(V(
        id="ctl-valid",
        code=("class C%(S)s { void Peek() const {} }\n"
              "void Use%(S)s(const C%(S)s@ c) { c.Peek(); }\n"),
        question="control", expect="ACCEPT",
        stands_for="a correctly-written const method on a const receiver",
    ))
    v.append(V(
        id="ctl-bogus",
        code=("class C%(S)s { void Peek() const {} }\n"
              "void Use%(S)s(C%(S)s@ c) { c.NoSuchMethod%(S)s(); }\n"),
        question="control", expect="REJECT",
        stands_for="a definitely-invalid call; proves the oracle sees errors",
    ))
    v.append(V(
        id="ctl-own-nonconst",
        code=("class C%(S)s { void Mutate() {} }\n"
              "void Use%(S)s(const C%(S)s@ c) { c.Mutate(); }\n"),
        question="control", expect="REJECT",
        stands_for="the rule the checker already implements, on a workspace type",
    ))

    # ---- Q-E1: engine REFERENCE type through a const handle -------------
    # `MemoryBuffer::Seek` is `void Seek(uint64 pos, int dir = 0)` — no
    # trailing const in the dump, so this is exactly a site variant E would
    # newly diagnose.
    v.append(V(
        id="eng-ref-nonconst",
        code="void Use%(S)s(const MemoryBuffer@ b) { b.Seek(0); }\n",
        question="Q-E1", expect="UNKNOWN",
        stands_for="a non-const ENGINE method through a const handle",
    ))
    v.append(V(
        id="eng-ref-const",
        code="void Use%(S)s(const MemoryBuffer@ b) { b.GetSize(); }\n",
        question="Q-E3", expect="UNKNOWN",
        stands_for="`uint64 GetSize() const` — must stay ACCEPT",
    ))

    # ---- Q-E1b: a second engine reference type, different family --------
    v.append(V(
        id="eng-json-nonconst",
        code=("void Use%(S)s(const Json::Value@ j) { j.Add(Json::Value()); }\n"),
        question="Q-E1", expect="UNKNOWN",
        stands_for="`void Add(Json::Value@)` — non-const on a const handle",
    ))
    v.append(V(
        id="eng-json-const",
        code="void Use%(S)s(const Json::Value@ j) { j.GetType(); }\n",
        question="Q-E3", expect="UNKNOWN",
        stands_for="`Json::Type GetType() const` — must stay ACCEPT",
    ))

    # ---- Q-E1c: templated Core type — the row that flips a B116 test ----
    v.append(V(
        id="eng-tmpl-nonconst",
        code="void Use%(S)s(const MwFastBuffer<uint>@ b) { b.Add(1); }\n",
        question="Q-E1", expect="UNKNOWN",
        stands_for="`void Add(const T&in value)` — const is on the PARAM, not trailing",
    ))
    v.append(V(
        id="eng-tmpl-const",
        code="void Use%(S)s(const MwFastBuffer<uint>@ b) { b.Length(); }\n",
        question="Q-E3", expect="UNKNOWN",
        stands_for="`uint Length() const` — must stay ACCEPT",
    ))

    # ---- Q-E4: NADEO type — no constness data exists at all -------------
    v.append(V(
        id="nadeo-nonconst",
        code="void Use%(S)s(const CMwNod@ n) { n.MwAddRef(); }\n",
        question="Q-E4", expect="UNKNOWN",
        stands_for="the Nadeo half, where is_const is hardcoded false = UNKNOWN",
    ))

    # ---- Q-E2: engine VALUE type through `const T &in` ------------------
    v.append(V(
        id="eng-val-nonconst",
        code="void Use%(S)s(const string &in s) { s += \"x\"; }\n",
        question="Q-E2", expect="UNKNOWN",
        stands_for="`string& opAddAssign(const string&in)` — non-const, value type",
    ))
    v.append(V(
        id="eng-val-const",
        code=("void Use%(S)s(const string &in s) { string t = s.ToLower(); }\n"),
        question="Q-E3", expect="UNKNOWN",
        stands_for="`string ToLower() const` — must stay ACCEPT",
    ))
    return v


def run_selftest() -> int:
    """Offline: can this probe's table answer anything? No game needed (B141).

    Thin BY DESIGN. The shared checks live in `fleet.probe_variant_problems`
    and `fleet.probe_run_problems`, because three probes hand-rolling one gate
    is how the fourth copy gets written. What is probe-specific stays here:
    that every fixture id is a legal, sweepable probe id, and that `classify`
    is the SHARED one rather than a copy that only claims to be.
    """
    problems = fleet.probe_variant_problems(variants())

    # B077: every staged directory must be recognisable as probe-owned, or the
    # post-run sweep cannot reclaim it and the next probe inherits it.
    for v in variants():
        fid = repro.PROBE_PREFIX + fleet.folder_id(f"B130ecr {v.id}")
        if not repro._valid_probe_id(fid):
            problems.append(
                f"variant {v.id!r} derives fixture id {fid!r}, which the B077 "
                f"sweep would not recognise as probe-owned"
            )

    # B139: the classifier must be the shared one. `probe_member_visibility`
    # carried a copy whose docstring said it delegated, which put it outside
    # the mutation coverage it appeared to have.
    import probe_const_receiver as _shared
    if classify is not _shared.classify:
        problems.append(
            "`classify` is not the shared implementation — a copy sits outside "
            "`just probe-cleanup-mutants`, so a fix to the classifier would "
            "silently not reach this probe"
        )

    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    if problems:
        print(f"FAIL: {len(problems)} problem(s) with this probe's table",
              file=sys.stderr)
        return 1
    print(f"PASS: {len(variants())} variants, controls of both polarities, "
          f"every row names what it stands for.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsv")
    ap.add_argument("--json")
    ap.add_argument("--only", help="run only variants whose id starts with this")
    ap.add_argument("--selftest", action="store_true",
                    help="offline checks on the variant table; no game needed")
    args = ap.parse_args()
    if args.selftest:
        return run_selftest()

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

    tmp = Path(tempfile.mkdtemp(prefix="b130ecr-"))
    rows = []
    try:
        for v in vs:
            fixture = write_fixture(tmp, v)
            rec = repro.probe(fixture)
            verdict, reason = classify(rec)
            rows.append({"id": v.id, "question": v.question, "expect": v.expect,
                         "verdict": verdict, "reason": reason,
                         "stands_for": v.stands_for})
            print(f"{v.id:22} {v.question:8} expect={v.expect:8} -> {verdict:10} {reason}",
                  file=sys.stderr)
    finally:
        import shutil
        shutil.rmtree(tmp, ignore_errors=True)

    # B077: this probe had NO post-run sweep, so it could exit 0 having left a
    # fixture installed -- and a stranded fixture turns every LATER probe's
    # rows into skipped/AMBIGUOUS. One lane's infrastructure failure becoming
    # the next lane's "measurement" is the same class as the rest of B139.
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
            "id\tquestion\texpect\tverdict\treason\n"
            + "".join(f"{r['id']}\t{r['question']}\t{r['expect']}\t{r['verdict']}\t{r['reason']}\n"
                      for r in rows))
    return 1 if (problems or left) else 0


if __name__ == "__main__":
    raise SystemExit(main())
