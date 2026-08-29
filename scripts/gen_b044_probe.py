#!/usr/bin/env python3
"""
gen_b044_probe.py — regenerate the B044 exhaustive probe fixture.

B044: a bare value name of an *anonymous* (inline member) engine enum is not a
symbol in the game — the enum has no name, so nothing in source can reach its
values except through the declaring member's type. The LSP used to accept such
a bare name; this fixture is the exhaustive list of the names that rule covers.

Name list (the same computation the ticket used to reach its 261 upper bound):

  for every member of every Nadeo type that declares an inline enum whose
  member type name is absent or the `UnnamedEnum` placeholder, take the enum's
  value names; drop any name that also appears in a NAMED enum (a Nadeo nested
  `e` block, a member enum with a real type name, or a Core `enums[]` entry),
  because a bare reference to those resolves legitimately in-game.

Output is one statement per line so the game compiler — which reports one
error per statement, at the first unresolved symbol — surfaces every rejected
name on its own line, and the line number maps back to the name.

Deterministic: names are emitted in sorted order.

Usage:
    scripts/gen_b044_probe.py                 # rewrite the fixture in place
    scripts/gen_b044_probe.py --list          # print the names, one per line
    scripts/gen_b044_probe.py --check         # exit 1 if the fixture is stale
    scripts/gen_b044_probe.py --batch N M     # emit a batch of M names from N
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TYPEDB = REPO / "tests/fixtures/typedb"
FIXTURE = REPO / "tests/fixtures/issue-repros/b044-anon-enum-value-names/Main.as"

ANON_TYPE_NAMES = {None, "", "UnnamedEnum"}

# The three names in the list below that the GAME accepts (RemoteBuild probe,
# OP 1.29.5, 2026-08-30: 258 of the 261 rejected). All three are values of
# `Scene::ESceneCharVisNotice`, a MODULE-LEVEL named engine enum. The Nadeo
# dump's `ns` map holds types only, so it cannot represent a namespace-level
# enum at all and the LSP never sees those 47 enums — a pre-existing gap of 55
# value names, 52 of which already fail to resolve. These 3 were resolving by
# accident, through the very anonymous-enum scan B044 narrows.
GAME_ACCEPTS = {
    "DoubleJump": "Scene::ESceneCharVisNotice",
    "Jump": "Scene::ESceneCharVisNotice",
    "TechJump": "Scene::ESceneCharVisNotice",
}


def anonymous_enum_values(nadeo: dict) -> tuple[dict[str, list[str]], set[str], int]:
    """(value name -> declaring anonymous enums, value names of NAMED enums, count)."""
    anon: dict[str, list[str]] = {}
    named: set[str] = set()
    declared_anon = 0
    for ns_name in sorted(nadeo["ns"]):
        types = nadeo["ns"][ns_name]
        for type_name in sorted(types):
            entry = types[type_name]
            for ordinal, member in enumerate(entry.get("m", [])):
                inline = member.get("e")
                if inline is None:
                    continue
                values = inline.get("v", [])
                declared = member.get("t")
                declared = declared if isinstance(declared, str) else None
                if declared in ANON_TYPE_NAMES:
                    declared_anon += 1
                    name = member.get("n") or f"UnnamedEnum#{ordinal}"
                    key = f"{ns_name}::{type_name}::{name}"
                    for value in values:
                        anon.setdefault(value, []).append(key)
                else:
                    named.update(values)
            for nested in entry.get("e") or []:
                named.update(nested.get("v", []))
    return anon, named, declared_anon


def probe_names() -> list[str]:
    core = json.loads((TYPEDB / "OpenplanetCore.json").read_text())
    nadeo = json.loads((TYPEDB / "OpenplanetNext.json").read_text())
    anon, named, _ = anonymous_enum_values(nadeo)
    for enum in core.get("enums", []):
        named.update(enum.get("values", {}).keys())
    return sorted(name for name in anon if name not in named)


HEADER = '''\
// B044: bare value names of ANONYMOUS (inline member) engine enums.
//
// The engine dump declares these enums inline on a member and gives them no
// name. Nothing in source can spell such an enum, so nothing in source can
// reach its values by a bare name — the game answers `No matching symbol`.
// Reaching the same values through the declaring member's type stays legal;
// see `b016-anon-member-enums` for that half of the rule.
//
// GENERATED — do not hand-edit. Regenerate with `scripts/gen_b044_probe.py`
// after refreshing `tests/fixtures/typedb`.
//
// One statement per line: AngelScript reports one error per statement, at the
// first unresolved symbol, so every rejected name gets its own line number.
//
// The list is every value name of the {enums} anonymous member enums in the
// fixture type DB, minus the {shared} that also appear in a named enum (a bare
// reference to those resolves legitimately, e.g. `VCenter2`). {count} names.
//
// Game verdict (RemoteBuild probe, OP 1.29.5, 2026-08-30): 258 rejected,
// 3 accepted. The 3 are marked `KNOWN FP` below — each is a value of the
// MODULE-LEVEL enum `Scene::ESceneCharVisNotice`, which the Nadeo dump cannot
// represent (its `ns` map holds types only), so the LSP has never known those
// 47 module-level enums. 52 of their value names already fail to resolve; these
// 3 were resolving only by accident, through the scan B044 narrows. Fixing that
// gap is its own ticket, and when it lands these 3 lines go quiet.
'''


def statement(name: str) -> str:
    line = f'    print("" + {name});'
    owner = GAME_ACCEPTS.get(name)
    if owner:
        line += f"  // KNOWN FP: the game accepts this — {owner} value"
    return line + "\n"


def render(names: list[str], enums: int, shared: int) -> str:
    body = "".join(statement(name) for name in names)
    return (
        HEADER.format(enums=enums, shared=shared, count=len(names))
        + "\nvoid Probe() {\n"
        + body
        + "}\n\nvoid Main() {\n    Probe();\n}\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true", help="print the names only")
    parser.add_argument("--check", action="store_true", help="fail if the fixture is stale")
    parser.add_argument("--batch", nargs=2, type=int, metavar=("START", "COUNT"))
    args = parser.parse_args()

    core = json.loads((TYPEDB / "OpenplanetCore.json").read_text())
    nadeo = json.loads((TYPEDB / "OpenplanetNext.json").read_text())
    anon, named, enums = anonymous_enum_values(nadeo)
    for enum in core.get("enums", []):
        named.update(enum.get("values", {}).keys())
    names = sorted(name for name in anon if name not in named)
    shared = len(anon) - len(names)

    if args.list:
        print("\n".join(names))
        return 0
    if args.batch:
        start, count = args.batch
        print(render(names[start : start + count], enums, shared), end="")
        return 0

    text = render(names, enums, shared)
    if args.check:
        current = FIXTURE.read_text() if FIXTURE.exists() else ""
        if current != text:
            print(f"stale: {FIXTURE}", file=sys.stderr)
            return 1
        print(f"up to date: {len(names)} names")
        return 0
    FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    FIXTURE.write_text(text)
    print(f"wrote {FIXTURE} — {len(names)} names from {enums} anonymous enums")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
