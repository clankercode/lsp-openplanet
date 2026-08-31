#!/usr/bin/env python3
"""B148/B151: diff two probe corpora and say WHY each row differs.

B129's runner compares whole files and reports CLEAN or DRIFT. That is the
right granularity for "did anything change", and the wrong one for acting on
it, because a message-text corpus can differ for four causes that need four
different responses:

  BEHAVIOUR-CHANGED   the VERDICT differs. The engine decides this case
                      differently than it used to. Every conclusion drawn from
                      the old row is suspect. This is the finding.

  WORDING-CHANGED     the verdict is the SAME and the message text differs.
                      The engine reworded itself. Nothing semantic moved, but
                      anything matching on the string -- this probe's own
                      discriminator, and any diagnostic that mirrors engine
                      prose -- must be updated.

  INSTRUMENT-BROKEN   the new verdict is UNRECOGNISED or AMBIGUOUS. The probe
                      could not classify what it saw. THIS IS NOT A FINDING
                      ABOUT THE LANGUAGE, and reporting it as one is the
                      specific failure this tool exists to prevent: b067
                      derives its verdict by matching two engine message
                      spellings, so an engine rewording would otherwise
                      present as all 63 rows flipping at once.

  ROW-ADDED / ROW-REMOVED
                      the row sets differ, so there is nothing to compare.

WHY THE ORDER MATTERS. INSTRUMENT-BROKEN is checked FIRST. A row whose new
verdict is UNRECOGNISED also has a different verdict from the old one, so a
naive implementation reports it as BEHAVIOUR-CHANGED -- the alarming reading --
when the truthful one is that the instrument stopped fitting. Reading a
cosmetic engine rewording as a language change, at 63 rows at once, is a
worse outcome than missing a real change, because it is loud, plausible, and
points at nothing.

Usage:
    diff_probe_corpus.py OLD.tsv NEW.tsv --key name --verdict verdict --message message
"""

from __future__ import annotations

import argparse
import csv
import sys
from pathlib import Path

BEHAVIOUR = "BEHAVIOUR-CHANGED"
WORDING = "WORDING-CHANGED"
INSTRUMENT = "INSTRUMENT-BROKEN"
ADDED, REMOVED, SAME = "ROW-ADDED", "ROW-REMOVED", "SAME"

# Verdicts that mean "this probe did not classify what it saw". They are never
# statements about the language.
NON_VERDICTS = {"UNRECOGNISED", "AMBIGUOUS", "NOT-RUN", "UNKNOWN", ""}


def read(path: Path) -> list[dict]:
    lines = [l for l in path.read_text(encoding="utf8").splitlines()
             if l.strip() and not l.startswith("#")]
    if not lines:
        raise SystemExit(f"{path} has no data rows")
    return list(csv.DictReader(lines, delimiter="\t"))


def classify_row(old: dict | None, new: dict | None,
                 verdict_col: str, message_col: str) -> tuple[str, str]:
    """Why does this row differ? Returns `(category, detail)`."""
    if old is None:
        return ADDED, f"new row, verdict {new.get(verdict_col, '')!r}"
    if new is None:
        return REMOVED, f"was {old.get(verdict_col, '')!r}"

    ov, nv = old.get(verdict_col, ""), new.get(verdict_col, "")
    om, nm = old.get(message_col, ""), new.get(message_col, "")

    # FIRST, before any verdict comparison: did the probe fail to classify?
    # This row also has a changed verdict, so checking it second would report
    # an instrument failure as a behaviour change.
    if nv in NON_VERDICTS and ov not in NON_VERDICTS:
        return INSTRUMENT, (
            f"verdict became {nv!r}; the probe could not classify {nm!r}. "
            f"Fix the discriminator before reading this as a change in the engine."
        )
    if ov != nv:
        return BEHAVIOUR, f"{ov!r} -> {nv!r}"
    if om != nm:
        return WORDING, f"verdict still {nv!r}; message {om!r} -> {nm!r}"
    return SAME, ""


class DuplicateKey(RuntimeError):
    pass


def index(rows: list[dict], key: str, which: str) -> dict[str, dict]:
    """Key the rows, REFUSING a key that is not unique.

    A duplicated key silently overwrites one row with another, so the diff
    compares fewer rows than the files contain and still reports a tidy
    result. b067 is exactly this shape: `CMwNod`, `CGameAction` and
    `CAudioListener` each appear twice -- once as a batched SAMPLE and once as
    an isolated CONTROL -- so keying on `name` alone drops the three rows that
    are the most interesting in the file, and reports SAME for 60 of 63
    without saying that 3 were never compared.

    Pass a composite key (`--key name,set`) when rows are only unique together.
    """
    cols = [c.strip() for c in key.split(",")]
    out: dict[str, dict] = {}
    dupes: list[str] = []
    for r in rows:
        k = "\t".join(r.get(c, "") for c in cols)
        if k in out:
            dupes.append(k)
        out[k] = r
    if dupes:
        raise DuplicateKey(
            f"{which}: key {key!r} is not unique -- {len(dupes)} duplicate(s), e.g. "
            f"{dupes[0]!r}. Keying on it would compare {len(out)} of {len(rows)} rows "
            f"and report the rest as if they matched. Use a composite key."
        )
    return out


def diff(old_rows: list[dict], new_rows: list[dict], key: str,
         verdict_col: str, message_col: str) -> list[tuple[str, str, str]]:
    o = index(old_rows, key, "old")
    n = index(new_rows, key, "new")
    out = []
    for k in sorted(set(o) | set(n)):
        cat, detail = classify_row(o.get(k), n.get(k), verdict_col, message_col)
        out.append((k, cat, detail))
    return out


def selftest() -> int:
    failures = []

    def check(name, cond, why=""):
        if not cond:
            failures.append(f"{name}: {why}")

    def row(v, m):
        return {"name": "x", "verdict": v, "message": m}

    c = lambda o, n: classify_row(o, n, "verdict", "message")[0]  # noqa: E731

    check("identical is SAME", c(row("SCOPE", "m"), row("SCOPE", "m")) == SAME)
    check("verdict change is BEHAVIOUR",
          c(row("SCOPE", "m"), row("NOT_SCOPE", "m2")) == BEHAVIOUR)
    check("message-only change is WORDING",
          c(row("SCOPE", "old text"), row("SCOPE", "new text")) == WORDING)

    # THE CENTRAL CASE. An UNRECOGNISED new verdict differs from the old one,
    # so an implementation checking verdicts first calls this BEHAVIOUR. It is
    # not: the probe stopped being able to read the engine.
    check("unrecognised is INSTRUMENT, not BEHAVIOUR",
          c(row("SCOPE", "m"), row("UNRECOGNISED", "reworded")) == INSTRUMENT,
          "an instrument failure was reported as a change in the engine")
    check("ambiguous is INSTRUMENT",
          c(row("SCOPE", "m"), row("AMBIGUOUS", "")) == INSTRUMENT)
    check("empty verdict is INSTRUMENT",
          c(row("SCOPE", "m"), row("", "")) == INSTRUMENT)

    # ...but a row that was ALREADY unclassified and now classifies is a real
    # improvement, not an instrument failure.
    check("unrecognised -> real verdict is BEHAVIOUR",
          c(row("UNRECOGNISED", "x"), row("SCOPE", "m")) == BEHAVIOUR,
          "a row that started classifying was reported as broken")

    check("added row", c(None, row("SCOPE", "m")) == ADDED)
    check("removed row", c(row("SCOPE", "m"), None) == REMOVED)

    # A whole-corpus rewording must read as an instrument failure across the
    # board, never as 63 behaviour changes.
    old = [{"name": f"n{i}", "verdict": "SCOPE", "message": "old"} for i in range(63)]
    new = [{"name": f"n{i}", "verdict": "UNRECOGNISED", "message": "new"} for i in range(63)]
    cats = {c for _, c, _ in diff(old, new, "name", "verdict", "message")}
    check("mass rewording is INSTRUMENT-BROKEN", cats == {INSTRUMENT},
          f"got {cats}; a reworded engine would have been reported as a language change")

    # A NON-UNIQUE KEY MUST REFUSE, not silently compare fewer rows.
    dup = [{"name": "x", "set": "A", "verdict": "SCOPE", "message": "m"},
           {"name": "x", "set": "B", "verdict": "NOT_SCOPE", "message": "m"}]
    raised = False
    try:
        index(dup, "name", "test")
    except DuplicateKey:
        raised = True
    check("duplicate key refuses", raised,
          "a non-unique key silently dropped a row instead of refusing")
    # ...and the composite key that makes them unique is accepted.
    check("composite key works", len(index(dup, "name,set", "test")) == 2,
          "a composite key did not separate the rows")
    # The count is the tell: a diff must compare every row it was given.
    got = diff(dup, dup, "name,set", "verdict", "message")
    check("composite diff compares both rows", len(got) == 2, f"compared {len(got)}")

    print(f"diff_probe_corpus selftest: {len(failures)} failure(s)")
    for f in failures:
        print(f"  FAIL {f}")
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("old", nargs="?")
    ap.add_argument("new", nargs="?")
    ap.add_argument("--key", default="name")
    ap.add_argument("--verdict", default="verdict")
    ap.add_argument("--message", default="message")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    if not a.old or not a.new:
        ap.error("old and new are required")

    try:
        rows = diff(read(Path(a.old)), read(Path(a.new)), a.key, a.verdict, a.message)
    except DuplicateKey as e:
        print(f"REFUSING TO DIFF: {e}", file=sys.stderr)
        return 2
    counts: dict[str, int] = {}
    for _, cat, _ in rows:
        counts[cat] = counts.get(cat, 0) + 1

    for k, cat, detail in rows:
        if cat != SAME:
            print(f"{cat:18s} {k:52s} {detail}")

    print()
    print("  " + "  ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    if counts.get(INSTRUMENT):
        print()
        print("INSTRUMENT-BROKEN rows are NOT a finding about the engine's behaviour.")
        print("The probe could not classify what it saw. Fix the discriminator first.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
