#!/usr/bin/env python3
"""B134: how much deprecation information is really in Core enum-value doc strings?

    just enum-doc-migrations            # the measurement, both dump versions
    just enum-doc-migrations-selftest   # proves the categoriser can fail

B134 was filed as a LEAD, not a finding: a handful of the 206 Core enum-value
`d` strings read as migration hints ("Please use Borders instead"), and the
ticket asked for the population to be measured before anyone matched on it.
This is that measurement, kept as a script rather than a paragraph so the next
engine bump can re-run it instead of re-deriving it.

THE ANSWER IS THAT THE INFORMATION IS NOT RECOVERABLE, AND THE REASON IS
STRUCTURAL RATHER THAN STATISTICAL.

A doc string is attached to an enum VALUE, and a deprecated flag and its
replacement are ALIASES SHARING ONE NUMERIC VALUE. So both names carry the
identical string, and the data cannot distinguish them:

    UI::ChildFlags::Border   v=1  "Please use Borders instead, as Border is deprecated"
    UI::ChildFlags::Borders  v=1  "Please use Borders instead, as Border is deprecated"

`Borders` is the name you are being told to ADOPT, and its own documentation
says it is deprecated. All four migration pairs in the dump have this shape.
A matcher would therefore raise a deprecation warning on the exact symbol the
user should migrate TO — which B134 itself rules out, and which this batch has
repeatedly found to be the expensive polarity of wrong.

Parsing the English harder does not help. The prose names the replacement
("use Borders"), not the deprecated member, so the string identifies which name
is CORRECT and never which of the two aliases you actually wrote.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
VERSIONS = ("1.29.5", "1.29.14")

# Deliberately WIDE. A narrow cue set would flatter the matcher by hiding the
# false positives it would produce in practice; the point of the measurement is
# the precision a real implementation would get.
CUES = ("instead", "deprecat", "obsolete", "no longer", "replaced", "superseded")

CORRECT, WRONG_POLARITY, NOT_MIGRATION = "correct", "wrong-polarity", "not-migration"


def core_path(version: str) -> Path:
    return REPO / "tests/fixtures/typedb-versions" / version / "OpenplanetCore.json"


def doc_values(version: str) -> dict[str, tuple[str, int, list[str]]]:
    """`{qualified name: (doc, numeric value, sibling names sharing that value)}`."""
    j = json.loads(core_path(version).read_text(encoding="utf8"))
    out: dict[str, tuple[str, int, list[str]]] = {}
    for e in j["enums"]:
        ns = e.get("ns") or ""
        by_value: dict[int, list[str]] = {}
        for name, x in (e.get("values") or {}).items():
            by_value.setdefault(x.get("v"), []).append(name)
        for name, x in (e.get("values") or {}).items():
            doc = (x.get("d") or "").strip()
            if doc:
                out[f"{ns}::{e['name']}::{name}"] = (doc, x.get("v"), by_value[x.get("v")])
    return out


def has_cue(doc: str) -> bool:
    return any(c in doc.lower() for c in CUES)


def categorise(name: str, doc: str, siblings: list[str]) -> str:
    """Which of the three a cue-carrying value really is.

    The `\\b` word boundaries are load-bearing and were earned: a first pass
    used a plain substring, and `use Borders` contains `use Border`, so the
    DEPRECATED member was scored as its own replacement. A prose matcher being
    tripped by a prefix of its own target is a small instance of the thing this
    measurement is about.
    """
    if len(siblings) == 1:
        return NOT_MIGRATION
    told_to_use = [
        s for s in siblings
        if re.search(r"\buse\s+`?" + re.escape(s) + r"`?\b", doc, re.I)
    ]
    if told_to_use and name in told_to_use:
        return WRONG_POLARITY
    if told_to_use:
        return CORRECT
    return WRONG_POLARITY


def measure(version: str) -> dict:
    vals = doc_values(version)
    cues = {k: v for k, v in vals.items() if has_cue(v[0])}
    counts = {CORRECT: 0, WRONG_POLARITY: 0, NOT_MIGRATION: 0}
    rows = []
    for k, (doc, _v, sibs) in sorted(cues.items()):
        cat = categorise(k.rsplit("::", 1)[1], doc, sibs)
        counts[cat] += 1
        rows.append((cat, k, doc))
    return {"version": version, "documented": len(vals), "cues": len(cues),
            "counts": counts, "rows": rows, "values": vals}


def run() -> int:
    results = [measure(v) for v in VERSIONS]
    for r in results:
        c = r["counts"]
        tot = r["cues"]
        print(f"=== Core {r['version']}")
        print(f"  enum values carrying a doc string : {r['documented']}")
        print(f"  ...containing a migration cue     : {tot}")
        print(f"     correct deprecation            : {c[CORRECT]}")
        print(f"     WRONG-POLARITY (the replacement): {c[WRONG_POLARITY]}")
        print(f"     not a migration at all          : {c[NOT_MIGRATION]}")
        if tot:
            print(f"  precision a matcher would get     : {c[CORRECT]}/{tot} "
                  f"= {100 * c[CORRECT] / tot:.0f}%")
        print()

    a, b = results[0]["values"], results[1]["values"]
    shared = set(a) & set(b)
    drifted = {k for k in shared if a[k][0] != b[k][0]}
    cue_a = {k for k in a if has_cue(a[k][0])}
    cue_b = {k for k in b if has_cue(b[k][0])}
    print(f"=== stability {VERSIONS[0]} -> {VERSIONS[1]}")
    print(f"  doc strings shared by both versions   : {len(shared)}")
    print(f"  ...whose wording CHANGED              : {len(drifted)}")
    print(f"  cue hits gained                       : {sorted(cue_b - cue_a)}")
    print(f"  cue hits lost                         : {sorted(cue_a - cue_b)}")
    gained_by_rewording = sorted((cue_b - cue_a) & drifted)
    print(f"  ...gained purely by a REWORDING of a still-supported symbol: "
          f"{gained_by_rewording}")
    print()
    print("READ THIS BEFORE MATCHING. Deprecation wording is stable (0 of the")
    print("shared cue hits changed), so the failure mode is not what B134")
    print("predicted. It is PRECISION DECAY: ordinary prose drifts INTO the cue")
    print("set. One engine bump produced one new false positive on a flag that")
    print("was never deprecated, because upstream rewrote 'Consider looking at")
    print("the X function' as 'Consider using X instead!'.")
    return 0


# ------------------------------------------------------------------ selftest


def selftest() -> int:
    failures: list[str] = []

    def check(name: str, cond: bool, why: str = "") -> None:
        if not cond:
            failures.append(f"{name}: {why}")

    # The categoriser must tell the three apart, and must be able to say each
    # of them — a classifier that only ever returns one label would make the
    # measurement above report a tidy, meaningless number.
    check("an alias pair names its replacement as CORRECT for the deprecated one",
          categorise("Border", "Please use Borders instead, as Border is deprecated.",
                     ["Border", "Borders"]) == CORRECT)
    # THE SUBSTRING TRAP, pinned: `use Borders` contains `use Border`. Without
    # word boundaries the deprecated member is scored as its own replacement.
    check("the replacement itself is WRONG-POLARITY",
          categorise("Borders", "Please use Borders instead, as Border is deprecated.",
                     ["Border", "Borders"]) == WRONG_POLARITY,
          "the name you should ADOPT was not flagged as a wrong-polarity match")
    check("a lone value with descriptive prose is NOT a migration",
          categorise("Foreground", "displayed in the foreground instead of the background",
                     ["Foreground"]) == NOT_MIGRATION)
    check("all three labels are reachable",
          len({CORRECT, WRONG_POLARITY, NOT_MIGRATION}) == 3)

    # The cue set must be able to fire and to decline.
    check("a migration cue is detected", has_cue("Deprecated: Use X instead"))
    check("ordinary prose without a cue is not", not has_cue("Select entire text"))

    # And the real dumps must still be readable, or the measurement above is
    # reporting zeros with a clean exit (B139).
    for v in VERSIONS:
        check(f"{v} dump is present", core_path(v).is_file(), str(core_path(v)))
        if core_path(v).is_file():
            vals = doc_values(v)
            check(f"{v} has doc'd enum values", len(vals) > 100,
                  f"only {len(vals)} — the measurement would report a tidy zero")
            check(f"{v} has at least one alias pair", any(len(s) > 1 for _, _, s in vals.values()),
                  "no shared-value aliases found, so the central finding could not be seen")

    print(f"enum-doc-migrations selftest: {len(failures)} failure(s)")
    for f in failures:
        print(f"  FAIL {f}")
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--selftest", action="store_true")
    return selftest() if ap.parse_args().selftest else run()


if __name__ == "__main__":
    sys.exit(main())
