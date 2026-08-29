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
  `e` block, a member enum with a real type name, a Core `enums[]` entry, or a
  MODULE-LEVEL enum in `Openplanet.h`), because a bare reference to those
  resolves legitimately in-game.

B046 added the last of those sources. Two module-level enums are reachable only
through the header, and three names the game accepts (`DoubleJump`, `Jump`,
`TechJump`) belong to one of them — they were in this list as known false
positives until the LSP learned to read the header.

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
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TYPEDB = REPO / "tests/fixtures/typedb"
FIXTURE = REPO / "tests/fixtures/issue-repros/b044-anon-enum-value-names/Main.as"

ANON_TYPE_NAMES = {None, "", "UnnamedEnum"}

HEADER_FILE = TYPEDB / "Openplanet.h"

IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
INTEGER = re.compile(r"^[+-]?\d+$")


def strip_comments(line: str, in_block_comment: list[bool]) -> str:
    """Match the Rust parser's comment removal before brace tracking."""
    out = ""
    rest = line
    while True:
        if in_block_comment[0]:
            end = rest.find("*/")
            if end < 0:
                return out
            in_block_comment[0] = False
            rest = rest[end + 2 :]
            continue
        block = rest.find("/*")
        line_comment = rest.find("//")
        if line_comment >= 0 and (block < 0 or line_comment < block):
            return out + rest[:line_comment]
        if block >= 0:
            out += rest[:block]
            in_block_comment[0] = True
            rest = rest[block + 2 :]
            continue
        return out + rest


def enum_decl(line: str) -> tuple[str, bool] | None:
    if not line.startswith("enum class "):
        return None
    rest = line[len("enum class ") :]
    if "{" in rest:
        name = rest.split("{", 1)[0].strip()
        return (name, True) if name else None
    if ";" not in rest and rest.strip():
        return rest.strip(), False
    return None


def block_decl_kind(prefix: str) -> str | None:
    prefix = prefix.strip()
    if ";" in prefix:
        return None
    if prefix.startswith("template") and ">" in prefix:
        prefix = prefix.rsplit(">", 1)[1].strip()
    for keyword in ("struct", "class", "union"):
        if prefix == keyword or (prefix.startswith(keyword) and prefix[len(keyword) :].startswith((" ", "\t"))):
            return "aggregate"
    if prefix == "namespace" or (prefix.startswith("namespace") and prefix[len("namespace") :].startswith((" ", "\t"))):
        return "namespace"
    return None


def enum_value(text: str) -> tuple[str, int] | None:
    if "=" not in text:
        return None
    name, value = (part.strip() for part in text.split("=", 1))
    value = value.rstrip(",").strip()
    if not IDENTIFIER.fullmatch(name) or not INTEGER.fullmatch(value):
        return None
    parsed = int(value)
    if not -(2**63) <= parsed <= 2**63 - 1:
        return None
    return name, parsed


def enum_values_after_open(after_open: str, lines: object) -> tuple[list[tuple[str, int]], str]:
    if "}" in after_open:
        body, suffix = after_open.split("}", 1)
        return (
            [
                value
                for entry in body.split(",")
                if (value := enum_value(entry.strip())) is not None
            ],
            suffix,
        )
    values: list[tuple[str, int]] = []
    for inner in lines:
        inner = inner.strip()
        if "}" in inner:
            body, suffix = inner.split("}", 1)
            values.extend(
                value
                for entry in body.split(",")
                if (value := enum_value(entry.strip())) is not None
            )
            return values, suffix
        value = enum_value(inner)
        if value is not None:
            values.append(value)
    return values, ""


def header_module_enums(text: str) -> list[tuple[str, list[tuple[str, int]]]]:
    """Rust-parity parser for module-level header enums, used by B044."""
    parsed: list[tuple[str, list[tuple[str, int]]]] = []
    in_block_comment = [False]
    lines = iter(strip_comments(line, in_block_comment) for line in text.splitlines())
    aggregate_depth = 0
    blocks: list[str] = []
    pending_block: str | None = None
    pending_enum: str | None = None
    for line in lines:
        trimmed = line.strip()
        if pending_enum is not None:
            name = pending_enum
            pending_enum = None
            if trimmed.startswith("{"):
                values, suffix = enum_values_after_open(trimmed, lines)
                if aggregate_depth == 0 and "::" not in name:
                    parsed.append((name, values))
                line = suffix
                trimmed = line.strip()
        declaration = enum_decl(trimmed)
        if declaration is not None:
            name, has_open_brace = declaration
            if not has_open_brace:
                pending_enum = name
                continue
            values, suffix = enum_values_after_open(trimmed.split("{", 1)[1], lines)
            if aggregate_depth == 0 and "::" not in name:
                parsed.append((name, values))
            line = suffix
            trimmed = line.strip()

        first_open = True
        prefix = line.split("{", 1)[0]
        declared_kind = block_decl_kind(prefix)
        if "{" not in line:
            if declared_kind is not None:
                pending_block = declared_kind
            elif trimmed:
                pending_block = None
        for char in line:
            if char == "{":
                if first_open:
                    first_open = False
                    kind = declared_kind or pending_block or "other"
                    pending_block = None
                else:
                    kind = "other"
                if kind == "aggregate":
                    aggregate_depth += 1
                blocks.append(kind)
            elif char == "}" and blocks:
                if blocks.pop() == "aggregate":
                    aggregate_depth = max(0, aggregate_depth - 1)
    return parsed


def header_module_enum_values(text: str) -> set[str]:
    """Value names of every MODULE-LEVEL enum in `Openplanet.h` (B046).

    Mirrors `src/typedb/header_format.rs`: a declaration at namespace level
    whose name is unqualified. Struct-nested and qualified declarations are
    skipped — the dump is the authority for those.
    """
    return {name for _, enum_values in header_module_enums(text) for name, _ in enum_values}


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
    if HEADER_FILE.is_file():
        named |= header_module_enum_values(HEADER_FILE.read_text(encoding="utf-8"))
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
// Game verdict (RemoteBuild probe, OP 1.29.5, 2026-08-30): all {count}
// rejected. The same probe accepted 3 more names this list used to carry —
// `DoubleJump`, `Jump` and `TechJump`, values of the module-level enum
// `ESceneCharVisNotice`. They were marked `KNOWN FP` here until B046 taught
// the loader to read `Openplanet.h`, which is the only source that names that
// enum; they now resolve legitimately and this list excludes them.
'''


def statement(name: str) -> str:
    return f'    print("" + {name});\n'


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
    parser.add_argument(
        "--parse-header",
        action="store_true",
        help="read header text on stdin and emit Rust-parity enum data as JSON",
    )
    args = parser.parse_args()

    if args.parse_header:
        print(json.dumps(header_module_enums(sys.stdin.read())))
        return 0

    core = json.loads((TYPEDB / "OpenplanetCore.json").read_text())
    nadeo = json.loads((TYPEDB / "OpenplanetNext.json").read_text())
    anon, named, enums = anonymous_enum_values(nadeo)
    for enum in core.get("enums", []):
        named.update(enum.get("values", {}).keys())
    if HEADER_FILE.is_file():
        named |= header_module_enum_values(HEADER_FILE.read_text(encoding="utf-8"))
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
