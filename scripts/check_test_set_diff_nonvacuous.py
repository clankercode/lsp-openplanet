#!/usr/bin/env python3
"""Prove ``test_set_diff.py --selftest`` is non-vacuous (B163).

The selftest has to discriminate both ways a test becomes disabled, the three
population-empty routes, and the over-condemn case where every ordinary
addition is called a removal.  This harness removes each production guard in a
throwaway copy and requires the selftest to go red.  A missing or ambiguous
anchor is a miss: silently skipping a mutation would make this prover vacuous
in exactly the way it is meant to detect.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


MUTATIONS = [
    (
        "B158 even swap: removed names are discarded",
        "    removed = sorted(set(baseline) - set(current))",
        "    removed = []",
    ),
    (
        "positive control: additions are discarded",
        "    added = sorted(set(current) - set(baseline))",
        "    added = []",
    ),
    (
        "second disable mechanism: active-to-ignored transitions disappear",
        "    newly_ignored = sorted(n for n in both if current[n] and not baseline[n])",
        "    newly_ignored = []",
    ),
    (
        "reverse ignored transition disappears",
        "    no_longer_ignored = sorted(n for n in both if baseline[n] and not current[n])",
        "    no_longer_ignored = []",
    ),
    (
        "the ignored population is never collected from --list --ignored",
        "        ignored = list_names(exe, ignored=True)",
        "        ignored = []",
    ),
    (
        "bare integration names are no longer qualified by binary",
        '            names[f"{binary}::{name}"] = name in ignored_set',
        "            names[name] = name in ignored_set",
    ),
    (
        "measurement problems no longer refuse comparison",
        "    if problems:\n        out.append(\"\")",
        "    if False:\n        out.append(\"\")",
    ),
    (
        "OVER-CONDEMN: every non-identical set is rendered as a removal alarm",
        "    if removed or newly_ignored:\n        out.append(",
        "    if True:\n        out.append(",
    ),
    (
        "the verified marker is printed for every set",
        "    if not (removed or added or newly_ignored or no_longer_ignored):",
        "    if True:",
    ),
    (
        "both empty and truncated baseline refusals are removed",
        "    if not names:\n        problems.append(\n"
        "            f\"the baseline at {_rel(path)} holds ZERO names. Every test in \"\n"
        "            f\"this tree would read as ADDED and REMOVALS WOULD COME OUT ZERO — \"\n"
        "            f\"a perfect-looking reconciliation over a destroyed expectation, \"\n"
        "            f\"which is the exact defect this check exists to catch.\"\n"
        "        )\n"
        "    elif len(names) < MIN_PLAUSIBLE_BASELINE:",
        "    if False:\n        problems.append(\"disabled\")\n"
        "    elif False:",
    ),
    (
        "aggregate-zero guard is removed, so binaries can list nothing",
        "    if not names and not problems:",
        "    if False:",
    ),
    (
        "baseline ordering is no longer enforced",
        "        if previous is not None and name < previous:",
        "        if False:",
    ),
]


def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/test_set_diff.py", "--selftest"],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=120,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b163-mut-") as tmp:
        root = Path(tmp) / "repo"
        scripts = root / "scripts"
        scripts.mkdir(parents=True)
        for name in ("test_set_diff.py", "test_inventory.py"):
            shutil.copy2(REPO_ROOT / "scripts" / name, scripts / name)

        target = scripts / "test_set_diff.py"
        pristine = target.read_text()
        if run(root) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for label, old, new in MUTATIONS:
            count = pristine.count(old)
            if count != 1:
                why = "MISSING" if count == 0 else f"AMBIGUOUS ({count}x)"
                print(f"  ANCHOR {why}: {label}", file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            if run(root) != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}", file=sys.stderr)
                missed += 1
            target.write_text(pristine)

        print(f"\n{caught}/{len(MUTATIONS)} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
