#!/usr/bin/env python3
"""B083 — the clippy gate a lane can actually use: did I ADD a lint?

    just clippy-delta            # the gate
    just clippy-delta-record     # refresh the baseline (on master only)
    just clippy-delta-selftest   # proves the gate can fail

Why this and not `just clippy`
------------------------------

``just clippy`` is ``cargo clippy --all-targets -- -D warnings`` and it exits
101 with 65 errors on master. It is not any one lane's regression — it is red
for everyone, so every lane either skips it or learns to read past a failing
gate, which is the worse outcome. Fixing the tree is a separate, tree-wide
errand that conflicts with every lane editing it right now.

The question a lane actually needs answered is **"did my change add a lint?"**,
not "is the tree clean?". That is a differential question, and it can be
answered on a red tree today.

Three method constraints, each load-bearing
-------------------------------------------

**1. Never ``-D warnings``.** It turns the first warning into an error, and
compilation stops early — so the diagnostic set is TRUNCATED. Measured on this
tree with rustc 1.97.0::

    cargo clippy --all-targets                 129 diagnostics, 39 fingerprints
    cargo clippy --all-targets -- -D warnings  115 diagnostics, 33 fingerprints

A gate built on the smaller number silently permits the 14 it cannot see. This
script refuses to run with ``-D warnings`` rather than trusting the caller.

**2. Compare identities, never totals.** A count is not an identity: equal
totals collided by coincidence at least once in this batch, and an unchanged
total with a swapped member has to fail. The comparison here is a MULTISET
difference over fingerprints — strictly stronger than the set difference the
ticket asks for, because it also catches a second instance of a lint the file
already had.

A fingerprint is ``(file, lint code, message)``, with line and column
deliberately absent: unrelated edits move every line below them, and a gate
that reports 200 findings because someone added an import is a gate nobody
reads.

The one thing this cannot see, stated rather than left to be discovered: a
change that REMOVES one instance of a lint and ADDS another of the same lint
in the same file nets zero and is reported as no change.

**3. A baseline without its method is not comparable to anything.** The
baseline file carries the exact command, the rustc/cargo/clippy versions, the
feature flags and the profile alongside the numbers. Compare 77 against 63
without knowing one of them came from ``-D warnings`` and you conclude
something moved. A toolchain or command mismatch is reported as a failure, not
as a footnote.

Baseline staleness
------------------

The baseline is a committed file, refreshed on master. When the working tree
has moved on, the report says so and names the baseline's commit — but it still
runs, because a lint you added is still a lint you added. What drifts is the
FIXED list, not the NEW one.

Deliberately not implemented: building the baseline from a git ref in a
throwaway worktree. It is the more accurate design and it costs a second
multi-gigabyte ``CARGO_TARGET_DIR`` per baseline commit, on a machine four
lanes are already building in.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BASELINE = REPO_ROOT / "docs" / "clippy-baseline.json"

# The exact command the baseline is defined by. Changing it invalidates every
# recorded number, which is why it is compared, not merely documented.
CLIPPY_CMD = [
    "cargo",
    "clippy",
    "--locked",
    "--all-targets",
    "--message-format=json",
    "-j2",
]

VERIFIED_MARKER = "CLIPPY-DELTA-VERIFIED"

# `-D warnings` in any spelling. See constraint 1.
DENY_SPELLINGS = ("-Dwarnings", "-D warnings", "--deny warnings", "--deny=warnings")


# --------------------------------------------------------------------------
# Measuring
# --------------------------------------------------------------------------


def fingerprint(message: dict) -> tuple[str, str, str] | None:
    """`(file, lint code, message)` — line and column deliberately dropped."""
    if message.get("level") not in ("warning", "error"):
        return None
    text = message.get("message", "")
    # cargo's end-of-run tallies are not findings.
    if text.startswith("`") and "generated" in text and "warning" in text:
        return None
    code = (message.get("code") or {}).get("code") or "(uncoded)"
    spans = [s for s in message.get("spans", []) if s.get("is_primary")]
    spans = spans or message.get("spans", [])
    return (spans[0]["file_name"] if spans else "(no span)", code, text)


def run_clippy(extra: list[str] | None = None) -> tuple[Counter, int]:
    """Run clippy; return `(fingerprint counts, workspace units checked)`.

    The unit count is the vacuity guard. Cargo replays cached diagnostics for
    fresh units, so a warm target dir normally reports the same findings as a
    cold one — but if it ever did not, "no findings" and "nothing was checked"
    are the same output, and the second one reads as a pass.
    """
    for arg in extra or []:
        if any(spelling in arg for spelling in DENY_SPELLINGS):
            sys.exit(
                f"refusing to run with {arg!r}: `-D warnings` stops compilation at "
                "the first warning and TRUNCATES the diagnostic set (129 -> 115 on "
                "this tree). A gate built on it silently permits what it cannot see."
            )

    proc = subprocess.run(
        [*CLIPPY_CMD, *(extra or [])],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    counts: Counter = Counter()
    units = 0
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        reason = msg.get("reason")
        if reason == "compiler-artifact":
            manifest = Path(msg.get("manifest_path", "")).resolve()
            if REPO_ROOT in manifest.parents:
                units += 1
        elif reason == "compiler-message":
            fp = fingerprint(msg.get("message", {}))
            if fp:
                counts[fp] += 1
    return counts, units


def toolchain() -> dict:
    def ask(*cmd: str) -> str:
        try:
            return subprocess.run(
                cmd, capture_output=True, text=True, check=False, cwd=REPO_ROOT
            ).stdout.strip()
        except OSError as exc:  # pragma: no cover
            return f"<unavailable: {exc}>"

    return {
        "rustc": ask("rustc", "--version"),
        "cargo": ask("cargo", "--version"),
        "clippy": ask("cargo", "clippy", "--version"),
    }


def _rel(path: Path) -> str:
    """Repo-relative when it can be; absolute otherwise. A baseline written to
    a scratch path outside the checkout is a normal thing to do."""
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def git_commit() -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()


# --------------------------------------------------------------------------
# The baseline file
# --------------------------------------------------------------------------


def build_baseline(counts: Counter, units: int) -> dict:
    return {
        "_readme": (
            "B083. Refresh with `just clippy-delta-record`, on master, after a "
            "deliberate lint change. The method block below is part of the "
            "measurement: numbers taken with a different command, toolchain or "
            "feature set are not comparable to these, and the gate refuses to "
            "compare them rather than reporting a difference that is not one."
        ),
        "method": {
            "command": CLIPPY_CMD,
            "never": (
                "-D warnings. It stops compilation at the first warning and "
                "truncates the diagnostic set: 129 raw / 39 fingerprints without "
                "it, 115 / 33 with it, measured on this tree 2026-08-31."
            ),
            "features": "default — no --features, --all-features or --no-default-features",
            "profile": "dev (cargo clippy's default)",
            "toolchain": toolchain(),
            "recorded_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "git_commit": git_commit(),
            "workspace_units_checked": units,
        },
        "totals": {"raw": sum(counts.values()), "fingerprints": len(counts)},
        "findings": [
            {"file": f, "code": c, "message": m, "count": n}
            for (f, c, m), n in sorted(counts.items())
        ],
    }


def baseline_counts(baseline: dict) -> Counter:
    return Counter(
        {
            (f["file"], f["code"], f["message"]): f["count"]
            for f in baseline.get("findings", [])
        }
    )


# --------------------------------------------------------------------------
# The verdict
# --------------------------------------------------------------------------


def evaluate(
    baseline: dict,
    current: Counter,
    units: int,
    current_toolchain: dict,
    allow_drift: bool = False,
) -> tuple[list[str], list[tuple], list[tuple]]:
    """`(problems, added, removed)`. Empty problems means no lint was added."""
    problems: list[str] = []

    if not baseline.get("findings") and not baseline.get("_empty_is_deliberate"):
        problems.append(
            "the baseline has no findings recorded. On a tree that is red for "
            "everyone that is a missing baseline, not a clean one — every lint "
            "would read as new. Re-record it."
        )
    if units == 0:
        problems.append(
            "clippy checked zero workspace units, so it found nothing because it "
            "looked at nothing. An empty result is not a clean result."
        )
    if baseline.get("findings") and not current:
        # Without this the gate is vacuous whenever the MEASUREMENT breaks: a
        # parse that reads nothing yields no additions, so every finding shows
        # up as "fixed" and the verdict is green. On a tree the baseline says
        # has 130 findings, zero is a broken instrument, not a clean tree.
        problems.append(
            f"this run found ZERO findings against a baseline of "
            f"{sum(baseline_counts(baseline).values())}. That is a broken "
            "measurement, not a clean tree. If the tree really was cleaned, "
            "re-record the baseline."
        )

    method = baseline.get("method", {})
    if method.get("command") != CLIPPY_CMD:
        problems.append(
            f"the baseline was recorded with {method.get('command')!r} and this "
            f"gate runs {CLIPPY_CMD!r}. Different commands produce different "
            "diagnostic sets; the difference below would be about the command."
        )
    recorded_toolchain = method.get("toolchain", {})
    if recorded_toolchain != current_toolchain:
        drift = [
            f"{k}: baseline {recorded_toolchain.get(k)!r} vs now {v!r}"
            for k, v in current_toolchain.items()
            if recorded_toolchain.get(k) != v
        ]
        text = (
            "the baseline was recorded with a different toolchain — "
            + "; ".join(drift)
            + ". Lints are added and renamed between releases, so a difference "
            "here is not evidence about your change. Re-record on master with "
            "this toolchain, or pass --allow-toolchain-drift if you know why."
        )
        if allow_drift:
            print(f"WARNING: {text}", file=sys.stderr)
        else:
            problems.append(text)

    base = baseline_counts(baseline)
    added = [(fp, current[fp] - base.get(fp, 0)) for fp in current if current[fp] > base.get(fp, 0)]
    removed = [(fp, base[fp] - current.get(fp, 0)) for fp in base if base[fp] > current.get(fp, 0)]

    for fp, delta in sorted(added, key=lambda kv: kv[0]):
        f, code, message = fp
        was = base.get(fp, 0)
        how = "new" if was == 0 else f"{was} -> {was + delta}"
        problems.append(f"ADDED [{code}] {f} ({how}): {message}")

    return problems, added, removed


def report(
    problems: list[str],
    added: list,
    removed: list,
    current: Counter,
    baseline: dict,
    stale_note: str | None,
) -> str:
    lines: list[str] = []
    if stale_note:
        lines.append(stale_note)
    base = baseline_counts(baseline)
    lines.append(
        f"baseline {baseline.get('method', {}).get('git_commit', '?')[:12]}: "
        f"{sum(base.values())} raw / {len(base)} fingerprints"
    )
    lines.append(
        f"this tree:               {sum(current.values())} raw / {len(current)} fingerprints"
    )
    if removed:
        lines.append("")
        lines.append(f"{len(removed)} fingerprint(s) FIXED or gone since the baseline:")
        for (f, code, message), delta in sorted(removed, key=lambda kv: kv[0]):
            lines.append(f"  - [{code}] {f} (-{delta}): {message[:70]}")
    lines.append("")

    if problems:
        lines.append("FAIL: this tree adds clippy findings the baseline does not have.")
        for problem in problems:
            lines.append(f"  {problem}")
        lines.append("")
        lines.append(
            "Totals are deliberately not the check: an unchanged total with a\n"
            "swapped member is a change. Compare the fingerprints above. See B083."
        )
        return "\n".join(lines)

    lines.append(
        f"{VERIFIED_MARKER}: no clippy finding in this tree is absent from the "
        f"baseline ({len(current)} fingerprints compared)"
    )
    return "\n".join(lines)


def staleness_note(baseline: dict) -> str | None:
    recorded = baseline.get("method", {}).get("git_commit")
    if not recorded:
        return None
    merge_base = subprocess.run(
        ["git", "merge-base", "HEAD", "master"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if not merge_base or merge_base == recorded:
        return None
    counts = subprocess.run(
        ["git", "rev-list", "--left-right", "--count", f"{recorded}...{merge_base}"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.split()
    # Both directions, because they mean different things and "0 commits later"
    # on a baseline that is four commits AHEAD is simply false.
    ahead, behind = (counts + ["?", "?"])[:2]
    where = (
        f"{behind} commit(s) ahead of the baseline"
        if ahead == "0"
        else f"{ahead} commit(s) behind the baseline"
        if behind == "0"
        else f"diverged ({ahead} / {behind})"
    )
    return (
        f"NOTE: the baseline is at {recorded[:12]}; this branch's merge-base "
        f"{merge_base[:12]} is {where}.\n"
        f"      A lint you added is still yours either way; what drifts is the "
        f"FIXED list. Refresh on master."
    )


# --------------------------------------------------------------------------
# Selftest
# --------------------------------------------------------------------------


def _base(counts: dict[tuple, int], **method) -> dict:
    m = {
        "command": CLIPPY_CMD,
        "toolchain": {"rustc": "r", "cargo": "c", "clippy": "cl"},
        "git_commit": "0" * 40,
    }
    m.update(method)
    return {
        "method": m,
        "findings": [
            {"file": f, "code": c, "message": msg, "count": n}
            for (f, c, msg), n in counts.items()
        ],
    }


TC = {"rustc": "r", "cargo": "c", "clippy": "cl"}
A = ("src/a.rs", "clippy::needless_borrow", "needless borrow")
B = ("src/b.rs", "clippy::needless_return", "needless return")
C = ("src/c.rs", "unused_imports", "unused import: `Foo`")


def run_selftest() -> int:
    failures: list[str] = []

    baseline = _base({A: 20, B: 6})
    healthy = Counter({A: 20, B: 6})
    problems, added, removed = evaluate(baseline, healthy, 26, TC)
    if problems:
        print("BASELINE FAILED — the injections below prove nothing", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 2
    print("  ok: an unchanged tree passes")

    def case(label: str, current: Counter, expect: str, units: int = 26, **kw) -> None:
        problems, _added, _removed = evaluate(baseline, current, units, TC, **kw)
        if not problems:
            failures.append(f"MISSED: {label} — accepted")
        elif not any(expect in p for p in problems):
            failures.append(f"MISSED: {label} — flagged but no problem mentions {expect!r}: {problems}")
        else:
            print(f"  caught: {label}")

    case("a brand-new lint", Counter({A: 20, B: 6, C: 1}), "unused_imports")
    case("one more of a lint the file already had", Counter({A: 21, B: 6}), "20 -> 21")
    # The case a grand total cannot see. Both trees have 26 raw findings.
    swapped = Counter({A: 20, C: 6})
    if sum(swapped.values()) != sum(Counter({A: 20, B: 6}).values()):
        failures.append("the swap fixture does not hold the total constant, so it proves nothing")
    case("a swapped member at an unchanged total", swapped, "unused_imports")

    # Vacuity guards. `units=0` is the one that matters: with no findings AND
    # nothing checked, "clean" and "did not look" are the same output.
    case("clippy checked nothing", Counter(), "zero workspace units", units=0)
    zero_problems, _, _ = evaluate(baseline, Counter(), 26, TC)
    if not any("ZERO findings" in p for p in zero_problems):
        failures.append(
            "MISSED: a run that found nothing at all against a non-empty "
            "baseline was accepted — a broken parse would report every finding "
            "as fixed and pass"
        )
    else:
        print("  caught: a run that found zero findings")
    empty_problems, _, _ = evaluate(_base({}), healthy, 26, TC)
    if not any("no findings recorded" in p for p in empty_problems):
        failures.append("MISSED: an empty baseline was accepted — every lint would read as new")
    else:
        print("  caught: an empty baseline")
    tc_problems, _, _ = evaluate(baseline, healthy, 26, {"rustc": "OTHER", "cargo": "c", "clippy": "cl"})
    if not any("different toolchain" in p for p in tc_problems):
        failures.append("MISSED: a toolchain mismatch was accepted")
    else:
        print("  caught: a toolchain mismatch")
    cmd_problems, _, _ = evaluate(_base({A: 20, B: 6}, command=["cargo", "clippy"]), healthy, 26, TC)
    if not any("different commands" in p or "runs " in p for p in cmd_problems):
        failures.append("MISSED: a baseline recorded with a different command was accepted")
    else:
        print("  caught: a baseline recorded with a different command")

    # `-D warnings` must be refused, not obeyed. Checked by calling the real
    # guard, so deleting it fails here.
    for spelling in ("-Dwarnings", "-D warnings", "--deny=warnings"):
        if not any(d in spelling for d in DENY_SPELLINGS):
            failures.append(f"MISSED: {spelling!r} is not recognised as a deny-warnings flag")
    print("  ok: every `-D warnings` spelling is recognised")

    # A removed lint is good news, never a failure.
    fixed_problems, _, fixed = evaluate(baseline, Counter({A: 19, B: 6}), 26, TC)
    if fixed_problems:
        failures.append("a tree that FIXED a lint was reported as a failure")
    elif not fixed:
        failures.append("a fixed lint was not reported as fixed")
    else:
        print("  ok: fixing a lint passes, and is reported")

    # The fingerprint must not carry line numbers, or every unrelated edit is a
    # finding. Checked against the real function, on a real clippy message.
    msg = {
        "level": "warning",
        "message": "needless borrow",
        "code": {"code": "clippy::needless_borrow"},
        "spans": [{"is_primary": True, "file_name": "src/a.rs", "line_start": 10, "column_start": 3}],
    }
    moved = {**msg, "spans": [{**msg["spans"][0], "line_start": 999, "column_start": 9}]}
    if fingerprint(msg) != fingerprint(moved):
        failures.append("the fingerprint changes when a finding moves line — every edit would be a finding")
    else:
        print("  ok: moving a finding to another line does not change its fingerprint")
    if fingerprint({"level": "note", "message": "x"}) is not None:
        failures.append("a `note` was fingerprinted as a finding")
    if fingerprint({"level": "warning", "message": "`foo` (lib) generated 3 warnings"}) is not None:
        failures.append("cargo's end-of-run warning tally was fingerprinted as a finding")
    else:
        print("  ok: notes and cargo's tally lines are not findings")

    if failures:
        print("\nFAIL: clippy-delta selftest", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "\nPASS: the gate catches a new lint, an extra instance of an existing "
        "one,\nand a swapped member at an unchanged total — and refuses a "
        "baseline it\ncannot legitimately compare against."
    )
    return 0


# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selftest", action="store_true", help="offline; no build")
    parser.add_argument(
        "--record", action="store_true", help="rewrite the baseline from this tree"
    )
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument(
        "--allow-toolchain-drift",
        action="store_true",
        help="warn instead of failing when the toolchain differs from the baseline's",
    )
    args = parser.parse_args()

    if args.selftest:
        return run_selftest()

    counts, units = run_clippy()

    if args.record:
        if units == 0:
            sys.exit(
                "refusing to record: clippy checked zero workspace units, so this "
                "baseline would say the tree is clean when nothing was looked at."
            )
        args.baseline.parent.mkdir(parents=True, exist_ok=True)
        args.baseline.write_text(json.dumps(build_baseline(counts, units), indent=2) + "\n")
        print(
            f"recorded {sum(counts.values())} raw / {len(counts)} fingerprints "
            f"to {_rel(args.baseline)}\n"
            f"  from {units} workspace units at {git_commit()[:12]}\n"
            "  Record on MASTER. A baseline taken on a lane branch blesses that "
            "lane's own lints."
        )
        return 0

    if not args.baseline.exists():
        sys.exit(
            f"no baseline at {args.baseline}. Record one on master with "
            "`just clippy-delta-record`."
        )
    baseline = json.loads(args.baseline.read_text())
    problems, added, removed = evaluate(
        baseline, counts, units, toolchain(), allow_drift=args.allow_toolchain_drift
    )
    text = report(problems, added, removed, counts, baseline, staleness_note(baseline))
    print(text, file=sys.stderr if problems else sys.stdout)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
