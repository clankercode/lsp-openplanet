#!/usr/bin/env python3
"""Prove `scripts/clippy_delta.py` is non-vacuous (B083). `just clippy-delta-mutants`.

A differential gate that reports "no new lints" because it failed to parse
clippy's output is the exact failure this batch keeps finding, and it is worse
than no gate because it is believed. So the gate is required to go red under
two kinds of attack.

**A — verdict guards.** Neuter one guard in a throwaway copy of
`clippy_delta.py`; its `--selftest` must go red. Copies, never the checkout, so
an interrupted run cannot leave a neutered guard behind.

**B — a real lint, in the real tree.** Write a Rust file that clippy really
does lint, run the real gate against the real baseline, and require it to name
the lint. This is the half that proves the MEASUREMENT works: the selftest
drives `evaluate` with synthetic counters and would pass happily if the JSON
parse were broken. Two injections, because they exercise different comparisons:

  * a lint kind absent from the whole baseline  -> a NEW fingerprint;
  * one more of a lint the same file already has -> a COUNT increase, which a
    plain set difference would miss entirely.

Not injected for real: a swap that holds the total constant. Arranging one in
real Rust means removing a lint and adding a different one in the same commit,
and the gate's behaviour on it is settled by the synthetic case in family A.
Saying so is the point — an unrun check should not read as a passing one.

Scoring follows B077: the baseline is gated first, and an anchor that is
missing OR ambiguous scores as a MISS rather than being skipped, so a mutation
that quietly stopped applying cannot masquerade as one that was caught.

Family B writes into `tests/`. Every write is undone in a `finally` and again
from an `atexit` hook, and the run ends by asserting `git status` is clean for
the paths it touched.
"""

from __future__ import annotations

import atexit
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "clippy_delta.py"
BASELINE = REPO_ROOT / "docs" / "clippy-baseline.json"

# A new test target: clippy checks this one unit and replays everything else
# from cache, so an injection costs seconds rather than a full re-check.
NEW_FILE = REPO_ROOT / "tests" / "zz_b083_lint_probe.rs"
NEW_FILE_SRC = """// TEMPORARY, written by scripts/check_clippy_delta_nonvacuous.py and removed
// again in the same run. If you are reading this in a commit, the harness died
// between writing and restoring; delete the file.
#[test]
fn b083_lint_probe() {
    let v: Vec<u8> = Vec::new();
    assert!(probe_is_empty(&v));
}

fn probe_is_empty(v: &Vec<u8>) -> bool {
    if v.len() == 0 {
        true
    } else {
        false
    }
}
"""

_TO_RESTORE: dict[Path, bytes | None] = {}
# sha256 of every file this harness touched, taken BEFORE the first mutation.
# The restore check compares against these rather than asking git, because
# `.git` is not synced to the remote build host — a git-based check would
# quietly stop checking the moment this harness ran over there, which is the
# "the guard silently stopped applying" shape B077 scores as a MISS.
_ORIGINAL_HASH: dict[Path, str | None] = {}


def _restore_all() -> None:
    for path, content in list(_TO_RESTORE.items()):
        try:
            if content is None:
                path.unlink(missing_ok=True)
            else:
                path.write_bytes(content)
        except OSError as exc:  # pragma: no cover - best-effort teardown
            print(f"RESTORE FAILED for {path}: {exc}", file=sys.stderr)
        _TO_RESTORE.pop(path, None)


atexit.register(_restore_all)


def digest(path: Path) -> str | None:
    """sha256 of a file's bytes, or None when it does not exist."""
    if not path.exists():
        return None
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot(path: Path) -> None:
    _TO_RESTORE[path] = path.read_bytes() if path.exists() else None
    _ORIGINAL_HASH.setdefault(path, digest(path))


def restore(path: Path) -> None:
    if path in _TO_RESTORE:
        content = _TO_RESTORE.pop(path)
        if content is None:
            path.unlink(missing_ok=True)
        else:
            path.write_bytes(content)


def run_gate(script: Path = SCRIPT, extra: list[str] | None = None):
    proc = subprocess.run(
        [sys.executable, str(script), *(extra or [])],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=3600,
    )
    return proc.returncode, proc.stdout + proc.stderr


# --------------------------------------------------------------------------
# Family A — neuter a verdict guard, --selftest must go red
# --------------------------------------------------------------------------

GUARD_MUTATIONS: list[tuple[str, str, str]] = [
    (
        "added-fingerprint detection",
        "    added = [(fp, current[fp] - base.get(fp, 0)) for fp in current if current[fp] > base.get(fp, 0)]",
        "    added = []",
    ),
    (
        # `if units == 0:` appears twice (evaluate, and --record's refusal), so
        # the anchor carries the following line to name the one meant.
        "zero-workspace-units guard",
        "    if units == 0:\n        problems.append(\n"
        '            "clippy checked zero workspace units,',
        "    if False:\n        problems.append(\n"
        '            "clippy checked zero workspace units,',
    ),
    (
        "zero-findings-against-a-real-baseline guard",
        '    if baseline.get("findings") and not current:',
        "    if False:",
    ),
    (
        "empty-baseline guard",
        '    if not baseline.get("findings") and not baseline.get("_empty_is_deliberate"):',
        "    if False:",
    ),
    (
        "baseline command mismatch guard",
        '    if method.get("command") != CLIPPY_CMD:',
        "    if False:",
    ),
    (
        "toolchain mismatch guard",
        "    if recorded_toolchain != current_toolchain:",
        "    if False:",
    ),
    (
        "`-D warnings` spellings (the truncation the gate must refuse)",
        'DENY_SPELLINGS = ("-Dwarnings", "-D warnings", "--deny warnings", "--deny=warnings")',
        "DENY_SPELLINGS = ()",
    ),
    (
        "fingerprint drops line numbers (a gate that reports every edit is unread)",
        '    return (spans[0]["file_name"] if spans else "(no span)", code, text)',
        '    return (spans[0]["file_name"] if spans else "(no span)", code, text,\n'
        '            str(spans[0].get("line_start")) if spans else "")',
    ),
    (
        "severity filter (a `note` is not a finding)",
        '    if message.get("level") not in ("warning", "error"):',
        "    if False:",
    ),
    (
        "cargo end-of-run tally filter",
        '    if text.startswith("`") and "generated" in text and "warning" in text:',
        "    if False:",
    ),
]


def family_a(work: Path) -> tuple[int, int]:
    target = work / "clippy_delta.py"
    pristine = SCRIPT.read_text()
    target.write_text(pristine)

    rc, out = run_gate(target, ["--selftest"])
    if rc != 0:
        print("BASELINE FAILED — guard mutations prove nothing", file=sys.stderr)
        print(out[-2000:], file=sys.stderr)
        return 0, len(GUARD_MUTATIONS)
    print("  baseline: --selftest PASSES on the pristine copy")

    caught = missed = 0
    for label, old, new in GUARD_MUTATIONS:
        if old not in pristine:
            print(f"  ANCHOR MISSING: {label}", file=sys.stderr)
            missed += 1
            continue
        if pristine.count(old) != 1:
            print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}", file=sys.stderr)
            missed += 1
            continue
        target.write_text(pristine.replace(old, new))
        rc, _out = run_gate(target, ["--selftest"])
        if rc != 0:
            print(f"  caught: {label}")
            caught += 1
        else:
            print(f"  MISSED: {label}  <-- the selftest is vacuous here", file=sys.stderr)
            missed += 1
        target.write_text(pristine)
    return caught, missed


# --------------------------------------------------------------------------
# Family B — a real lint, injected into the real tree
# --------------------------------------------------------------------------


def pick_count_increase_site(baseline: dict) -> tuple[Path, str] | None:
    """A `tests/*.rs` file the baseline already records `needless_borrow` in.

    Read off the baseline rather than hard-coded, so this harness cannot go on
    "passing" against a file or a lint that is no longer there — and a test
    target keeps the re-check to one unit.
    """
    for finding in baseline.get("findings", []):
        if finding["code"] == "clippy::needless_borrow" and finding["file"].startswith(
            "tests/"
        ):
            path = REPO_ROOT / finding["file"]
            if path.exists():
                return path, finding["file"]
    return None


NEEDLESS_BORROW_SRC = """

// TEMPORARY, written by scripts/check_clippy_delta_nonvacuous.py.
#[test]
fn b083_extra_needless_borrow_probe() {
    let s = String::from("x");
    fn takes(v: &str) -> usize { v.len() }
    assert_eq!(takes(&&s), 1);
}
"""


def method_mismatch_case(baseline: dict) -> tuple[int, int]:
    """Record a REAL baseline the wrong way and require the gate to refuse it.

    The fixture is `--all-targets`. Two of master's three `unused_imports`
    live under `cfg(test)` (`src/server/symbols.rs`, `src/typedb/index.rs`),
    so dropping the flag drops them — along with nine other findings — and the
    difference is completely invisible in a count.

    The materiality check comes FIRST and is not optional: if the two captures
    happened to agree, a refusal would be untested against any real difference
    and this case would be scoring itself. That is the same "the mutation
    stopped applying" hazard B077 scores as a MISS, one level up.
    """
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
        wrong = Path(handle.name)
    try:
        rc = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--record",
                "--baseline",
                str(wrong),
                # Capture WITHOUT --all-targets by overriding the command.
                "--command-override",
                "cargo,clippy,--locked,--message-format=json,-j2",
            ],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            timeout=3600,
        )
        if rc.returncode != 0 or not wrong.exists():
            print(
                f"  ANCHOR MISSING: could not record a no---all-targets baseline: "
                f"{rc.stdout[-300:]}{rc.stderr[-300:]}",
                file=sys.stderr,
            )
            return 0, 1
        other = json.loads(wrong.read_text())
        here = {(f["file"], f["code"], f["message"]) for f in baseline["findings"]}
        there = {(f["file"], f["code"], f["message"]) for f in other["findings"]}
        if here == there:
            print(
                "  ANCHOR MISSING: capturing with and without --all-targets gave "
                "the SAME fingerprint set, so refusing to compare them proves "
                "nothing on this tree",
                file=sys.stderr,
            )
            return 0, 1
        only_all_targets = len(here - there)
        rc2, out = run_gate(extra=["--baseline", str(wrong)])
        if rc2 == 0:
            print(
                "  MISSED: a baseline captured without --all-targets was accepted",
                file=sys.stderr,
            )
            return 0, 1
        if "METHOD MISMATCH" not in out or "REFUSED" not in out:
            print(
                f"  MISSED: went red but did not refuse — it differenced two "
                f"incomparable captures",
                file=sys.stderr,
            )
            return 0, 1
        if "ADDED [" in out:
            print(
                f"  MISSED: the refusal still printed a delta; {only_all_targets} "
                f"phantom additions would read as a code change",
                file=sys.stderr,
            )
            return 0, 1
        print(
            f"  caught: a baseline captured without --all-targets "
            f"({only_all_targets} fingerprints differ) — refused, no delta shown"
        )
        return 1, 0
    finally:
        wrong.unlink(missing_ok=True)


def family_b(baseline: dict) -> tuple[int, int]:
    rc, out = run_gate()
    if rc != 0:
        print("BASELINE FAILED — the injections below prove nothing", file=sys.stderr)
        print(out[-3000:], file=sys.stderr)
        return 0, 2
    print("  baseline: the real tree adds nothing over docs/clippy-baseline.json")

    caught = missed = 0

    # 1. A lint kind absent from the whole baseline -> a NEW fingerprint.
    absent = {f["code"] for f in baseline.get("findings", [])}
    wanted = [c for c in ("clippy::len_zero", "clippy::needless_bool") if c not in absent]
    if not wanted:
        print(
            "  ANCHOR MISSING: both probe lints are already in the baseline, so a "
            "new-fingerprint injection cannot be staged",
            file=sys.stderr,
        )
        missed += 1
    else:
        snapshot(NEW_FILE)
        try:
            NEW_FILE.write_text(NEW_FILE_SRC)
            rc, out = run_gate()
        finally:
            restore(NEW_FILE)
        if rc == 0:
            print("  MISSED: a brand-new lint — the gate stayed green", file=sys.stderr)
            missed += 1
        elif not any(code in out for code in wanted):
            print(
                f"  MISSED: a brand-new lint — went red but named none of {wanted}",
                file=sys.stderr,
            )
            missed += 1
        else:
            named = next(c for c in wanted if c in out)
            print(f"  caught: a brand-new lint ({named}) in a new file")
            caught += 1

    # 2. One MORE of a lint the file already has -> a COUNT increase. A plain
    #    set difference cannot see this one, which is why it is here.
    site = pick_count_increase_site(baseline)
    if site is None:
        print(
            "  ANCHOR MISSING: the baseline records no needless_borrow in tests/, "
            "so a count-increase injection cannot be staged",
            file=sys.stderr,
        )
        missed += 1
    else:
        path, rel = site
        snapshot(path)
        try:
            path.write_text(path.read_text() + NEEDLESS_BORROW_SRC)
            rc, out = run_gate()
        finally:
            restore(path)
        if rc == 0:
            print(
                f"  MISSED: an extra needless_borrow in {rel} — the gate stayed green",
                file=sys.stderr,
            )
            missed += 1
        elif rel not in out or "->" not in out:
            print(
                f"  MISSED: an extra needless_borrow in {rel} — went red but did "
                f"not report it as a count increase on that file",
                file=sys.stderr,
            )
            missed += 1
        else:
            print(f"  caught: one more needless_borrow in {rel} (count increase)")
            caught += 1

    # 3. A baseline captured by a DIFFERENT METHOD must be refused, not
    #    differenced. The live fixture is `--all-targets`: two of the three
    #    `unused_imports` on master exist only under `cfg(test)`, so its
    #    presence or absence changes the diagnostic set — invisibly, in a count.
    caught_m, missed_m = method_mismatch_case(baseline)
    caught += caught_m
    missed += missed_m

    rc, _out = run_gate()
    if rc != 0:
        print("  RESTORE FAILED: the gate is still red after the injections", file=sys.stderr)
        missed += 1
    else:
        print("  restore verified: the gate is green again")

    # Byte-identity of exactly the files touched. Stronger than repo
    # cleanliness, and it works on a machine that has no `.git`.
    for path, before in _ORIGINAL_HASH.items():
        after = digest(path)
        if after != before:
            print(
                f"  RESTORE FAILED: {path} is not byte-identical to before "
                f"({before} -> {after})",
                file=sys.stderr,
            )
            missed += 1
    if _ORIGINAL_HASH:
        print(f"  restore verified by hash on {len(_ORIGINAL_HASH)} file(s)")
    else:
        # No file was ever snapshotted, so the check above passed over nothing.
        print(
            "  ANCHOR MISSING: no file was snapshotted, so the restore check "
            "verified nothing",
            file=sys.stderr,
        )
        missed += 1

    # Git, when it is here. Not the primary check: it is absent on the remote
    # build host, and a check that can silently stop applying is not a check.
    if (REPO_ROOT / ".git").exists():
        dirty = subprocess.run(
            ["git", "status", "--porcelain", "--", "tests", "src"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if dirty:
            print(f"  RESTORE FAILED: git reports\n{dirty}", file=sys.stderr)
            missed += 1

    return caught, missed


def main() -> int:
    if not BASELINE.exists():
        sys.exit(f"no baseline at {BASELINE} — record one first")
    baseline = json.loads(BASELINE.read_text())

    with tempfile.TemporaryDirectory(prefix="b083-mut-") as tmp:
        work = Path(tmp)
        shutil.copy2(SCRIPT, work / "clippy_delta.py")
        print("==> A: verdict guards (neutered in a copy; --selftest must go red)")
        a_caught, a_missed = family_a(work)
        print()
        print("==> B: a real lint, injected into the real tree")
        b_caught, b_missed = family_b(baseline)

    caught, missed = a_caught + b_caught, a_missed + b_missed
    print(f"\n{caught}/{caught + missed} mutations caught, {missed} missed")
    return 0 if missed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
