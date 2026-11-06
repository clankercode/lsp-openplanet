#!/usr/bin/env python3
"""Measure property increments and compound assignments in the live game.

Uses disposable leaf plugins and the existing issue-repro lifecycle driver.
Requires Trackmania/Openplanet; performs no Rust build or gameplay action.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import tempfile
import time
from pathlib import Path

import issue_repro_game as driver


def source_for(operations: list[str]) -> tuple[str, list[dict]]:
    lines = [
        "int backing; int get_G() { return backing; } void set_G(int value) { backing = value; }",
        "namespace N { int backing; int get_P() { return backing; } void set_P(int value) { backing = value; } }",
        "class C { int backing; int get_P() { return backing; } void set_P(int value) { backing = value; }",
    ]
    cases = []
    for index, operation in enumerate(operations):
        for target in ["P", "this.P"]:
            lines.append(f"void Implicit{index}_{len(cases)}() {{ {operation.replace('P', target)}; }}")
            cases.append(dict(line=len(lines), operation=operation, target=target))
    lines.extend([
        "}",
        "class Block { int backing; int P { get { return backing; } set { backing = value; } } }",
    ])
    for index, operation in enumerate(operations):
        for target, prefix in [("G", ""), ("N::P", ""), ("c.P", "C c; "), ("b.P", "Block b; ")]:
            lines.append(f"void Outside{index}_{len(cases)}() {{ {prefix}{operation.replace('P', target)}; }}")
            cases.append(dict(line=len(lines), operation=operation, target=target))
    lines.append("void Main() {}")
    return "\n".join(lines) + "\n", cases


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tsv", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, default=Path("target/gh73-property-operators-evidence.json"))
    args = parser.parse_args()
    groups = [
        ("increments", ["P++", "P--", "++P", "--P"]),
        ("compounds", ["P += 1", "P -= 1", "P *= 2", "P /= 2", "P %= 2", "P &= 1", "P |= 1", "P ^= 1", "P <<= 1", "P >>= 1"]),
    ]
    observations = []
    rows = []
    date = time.strftime("%Y-%m-%d")
    # Keep spike development under the plugin workspace, with its own root.
    with tempfile.TemporaryDirectory(prefix="lsp-gh73-property-", dir=driver.fleet.MY_PLUGINS) as temporary:
        root = Path(temporary)
        (root / "src").mkdir()
        (root / "info.toml").write_text('[meta]\nname = "GH73 property operator probe 20261107"\nauthor = "Codex probe"\nversion = "0.0.0"\n[script]\n')
        for group, operations in groups:
            source, cases = source_for(operations)
            (root / "src/Main.as").write_text(source)
            hashes = {}

            def route(action, data, timeout=20):
                if action == "load_plugin":
                    destination = driver.fleet.OPN_PLUGINS / data["id"]
                    for path in [root / "info.toml", root / "src/Main.as"]:
                        if path.read_bytes() != (destination / path.name).read_bytes():
                            raise RuntimeError(f"staging changed {path.name}")
                        hashes[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
                return driver.fleet.game_route(action, data, timeout=timeout)

            start = driver.fleet.OP_LOG.stat().st_size
            result = driver.probe(root, rb_route_fn=route)
            end = driver.fleet.OP_LOG.stat().st_size
            version = driver.fleet_engine_version()
            with driver.fleet.OP_LOG.open("rb") as log:
                log.seek(start)
                window = log.read(end - start).decode(errors="replace")
            result.update(group=group, cases=cases, source=source, source_sha256=hashes,
                          log_start=start, log_end=end, log_window=window,
                          engine_version=version, probe_date=date)
            observations.append(result)
            args.evidence.parent.mkdir(parents=True, exist_ok=True)
            args.evidence.write_text(json.dumps(observations, indent=2) + "\n")
            if not version or result.get("skipped") or not result.get("restored") or result.get("contended"):
                raise RuntimeError(f"unmeasured, unrestored or contended {group}; see {args.evidence}")
            errors = result.get("game_errors", [])
            by_line = {case["line"]: [] for case in cases}
            for error in errors:
                if error["line"] not in by_line or not error["file"].endswith("/Main.as"):
                    raise RuntimeError(f"declaration/unattributed failure; see {args.evidence}")
                by_line[error["line"]].append(error["msg"])
            clean = result.get("build_status") == "CLEAN"
            if not clean and (result.get("build_status") != "REJECTED" or not all(by_line.values())):
                # A rejected unit does not prove its unreported cases legal.
                raise RuntimeError(f"partial compilation cannot adjudicate every case; see {args.evidence}")
            for case in cases:
                message = "compiled and loaded" if clean else " | ".join(by_line[case["line"]])
                rows.append([f"{group}:{case['target']}:{case['operation']}", group, case["target"], case["operation"],
                             "ACCEPT" if clean else "REJECT", message,
                             version, date, hashes["Main.as"], str(case["line"])])
            print(f"{group}: {len(cases)} cases, {result['build_status']}, restored", flush=True)
    versions = {row[6] for row in rows}
    if len(versions) != 1:
        raise RuntimeError("engine version changed during the probe")
    header = "case\tgroup\ttarget\toperation\tverdict\tmessage\tengine_version\tprobe_date\tsource_sha256\tline"
    args.tsv.parent.mkdir(parents=True, exist_ok=True)
    args.tsv.write_text(
        f"# GH73 property operators; Openplanet {next(iter(versions))}; {date}\n"
        "# Generated by scripts/probe_property_operators.py; exact staged bytes and fresh logs in --evidence.\n"
        + header + "\n" + "\n".join("\t".join(row) for row in rows) + "\n"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
