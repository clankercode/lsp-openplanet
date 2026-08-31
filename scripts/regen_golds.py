#!/usr/bin/env python3
"""B129: regenerate every engine-derived gold artifact and report the drift.

One command enumerates every committed engine-derived artifact, reruns its
generator against the live game, writes fresh outputs and prints a per-artifact
drift report. NOTHING IS AUTO-COMMITTED: the run leaves a dirty tree and a
report, and a human decides.

  just regen-golds              audit only, no game needed, changes nothing
  just regen-golds --regen      rerun every generator against the live game
  just regen-golds --only b117  restrict to artifacts matching a substring
  just regen-golds-selftest     offline checks of this runner's own logic

THE VERDICT SET IS FOUR-VALUED, AND NOT-RUN IS NEVER A VERDICT (B118).

  CLEAN      regenerated, and identical to what is committed
  DRIFT      regenerated, and different
  UNCHECKED  could not be checked, WITH A REASON. Never silence.
  ERROR      the generator ran and failed

An audit run reports every corpus UNCHECKED(not-run), because a runner that
reported CLEAN for an artifact it never regenerated would be asserting the
game agrees with a file it did not ask the game about. That is exactly the
defect B127 found in `verdict`, which reported PARITY-CLEAN for builds that
never happened.

WHY THE RUN CAN FAIL WITH NOTHING BROKEN (B139 + the over-condemn rule).
A gate written as "no failures" cannot distinguish "everything passed" from
"nothing ran", and this runner has precisely that shape: every artifact
UNCHECKED is a report with no failures in it. So a `--regen` run that measured
NOTHING exits non-zero and says NOTHING-MEASURED, and every run asserts the
PRESENCE of the artifacts it was asked to check before reporting on them.

WHY THE VERSION COLUMN IS LOAD-BEARING.
Three engine generations (1.29.5, 1.29.9, 1.29.14) are cited concurrently in
this repo, and two corpora probed on the SAME DAY record different versions.
So the runner REFUSES to diff an artifact whose recorded version is UNKNOWN or
differs from the live engine, rather than reporting a drift it cannot
attribute. A version-blind report would call "the game moved on" and "the file
is stale" the same thing, and the first real run after a game update is
exactly when that distinction matters.

WHY THE DIFF IS A STRING DIFF, AND WHERE THAT IS AND IS NOT A LIMITATION.
For CORPUS artifacts the committed content IS compiler message text, so a
string comparison is exact rather than a rendered approximation - b067's whole
discriminator is two message spellings. There is no structure underneath to be
blind to. That is NOT true everywhere in this repo: B136 found `const C@[]@`
and `const array<C@>@` rendering identically from different parse trees, but
that blindness lives at the CONSUMER, in the parser and TypeRepr, not in these
corpora. TYPEDB is JSON and is diffed field-wise. Each artifact declares its
own mode in the manifest rather than the runner assuming one.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "docs/probes/MANIFEST.tsv"

COLUMNS = [
    "artifact", "kind", "question", "generator",
    "engine_version", "probe_date", "rows", "diff", "dependents", "volatility",
]

CLEAN, DRIFT, UNCHECKED, ERROR = "CLEAN", "DRIFT", "UNCHECKED", "ERROR"
KNOWN_KINDS = {"CORPUS", "SIDECAR", "TYPEDB", "PRIMARY", "VERSIONS"}
KNOWN_DIFFS = {"STRING", "STRUCTURED", "MANUAL"}

# B151. How to READ this artifact's drift, not new machinery: the axes are
# already separated by scripts/diff_probe_corpus.py at comparison time.
SURFACE, SEMANTICS, HYBRID = "SURFACE", "SEMANTICS", "HYBRID"
KNOWN_VOLATILITY = {SURFACE, SEMANTICS, HYBRID}

# Where SURFACE artifacts keep their retained per-version history. TWO stores,
# because they hold different things: the type DUMPS the game ships, and the
# CORPORA this repo probes out of it. One store cannot stand in for the other
# -- a report claiming b052's surface was retained because the typedb store
# held 1.29.14 was false, and is what prompted `is_retained` to check for an
# actual copy rather than a matching version label.
VERSION_STORES = ("tests/fixtures/typedb-versions", "docs/probes/versions")


@dataclass
class Row:
    artifact: str
    kind: str
    question: str
    generator: str
    engine_version: str
    probe_date: str
    rows: str
    diff: str
    dependents: str
    volatility: str


@dataclass
class Result:
    row: Row
    status: str
    reason: str
    detail: str = ""
    dependents_unchecked: list[str] = field(default_factory=list)


class ManifestError(RuntimeError):
    pass


def parse_manifest(path: Path = MANIFEST) -> list[Row]:
    """Parse the manifest. Never returns an empty list silently."""
    if not path.exists():
        raise ManifestError(f"no manifest at {path}")
    lines = [
        l for l in path.read_text(encoding="utf8").splitlines()
        if l.strip() and not l.startswith("#")
    ]
    if not lines:
        raise ManifestError(f"{path} has no rows at all")
    header = lines[0].split("\t")
    if header != COLUMNS:
        raise ManifestError(f"{path} columns are {header}, expected {COLUMNS}")
    out: list[Row] = []
    for n, line in enumerate(lines[1:], start=2):
        cells = line.split("\t")
        if len(cells) != len(COLUMNS):
            raise ManifestError(
                f"{path}:{n} has {len(cells)} columns, expected {len(COLUMNS)}"
            )
        row = Row(*cells)
        if row.kind not in KNOWN_KINDS:
            raise ManifestError(f"{path}:{n} unknown kind {row.kind!r}")
        if row.diff not in KNOWN_DIFFS:
            raise ManifestError(f"{path}:{n} unknown diff mode {row.diff!r}")
        if row.volatility not in KNOWN_VOLATILITY:
            raise ManifestError(
                f"{path}:{n} unknown volatility {row.volatility!r}; expected one of "
                f"{sorted(KNOWN_VOLATILITY)}"
            )
        out.append(row)
    if not out:
        raise ManifestError(f"{path} parsed to zero data rows")
    return out


def data_rows(path: Path) -> int:
    """Data rows in a corpus: excludes the `#` preamble AND the column header."""
    lines = [
        l for l in path.read_text(encoding="utf8").splitlines()
        if l.strip() and not l.startswith("#")
    ]
    return max(0, len(lines) - 1)


def unregistered_corpora(rows: list[Row]) -> list[str]:
    """`.tsv` files in docs/probes/ that the manifest does not name.

    Parsed from the DIRECTORY, never from a hand-copied list: B131's struct
    audit covered 7 of 11 structs while passing, because it compared against a
    list somebody maintained by hand.
    """
    registered = {r.artifact for r in rows}
    found = []
    for p in sorted((REPO / "docs/probes").glob("*.tsv")):
        if p.name == "MANIFEST.tsv":
            continue
        rel = p.relative_to(REPO).as_posix()
        if rel not in registered:
            found.append(rel)
    return found


VERSION_LINE = re.compile(r"Openplanet:\s*(\d+\.\d+\.\d+)\s*\(([^)]*)\)")


def live_engine_version(log: Path | None = None) -> str | None:
    """The Openplanet version of the running game, or None if unreadable.

    Read from the header Openplanet writes at startup:

        [Platform] [LOG] [..]  Openplanet: 1.29.14 (next, Public, 6fd4200f)

    The LAST such line is used, because the log accumulates across sessions
    and only the most recent one describes the engine a probe would hit now.

    Returns None rather than a guess. An artifact diffed against an ASSUMED
    version is the exact failure the version column exists to prevent, so
    every path that cannot read a version refuses to diff instead of falling
    back to "probably current".
    """
    if log is None:
        try:
            sys.path.insert(0, str(REPO / "scripts"))
            import fleet_dogfood as fleet  # noqa: PLC0415

            log = Path(fleet.OP_LOG)
        except Exception:
            return None
    if not log.exists():
        return None
    try:
        text = log.read_text(encoding="utf8", errors="replace")
    except OSError:
        return None
    found = VERSION_LINE.findall(text)
    return found[-1][0] if found else None


def live_engine_build(log: Path | None = None) -> str | None:
    """The parenthesised build descriptor, e.g. `next, Public, 6fd4200f`."""
    if log is None:
        try:
            sys.path.insert(0, str(REPO / "scripts"))
            import fleet_dogfood as fleet  # noqa: PLC0415

            log = Path(fleet.OP_LOG)
        except Exception:
            return None
    if not log.exists():
        return None
    found = VERSION_LINE.findall(log.read_text(encoding="utf8", errors="replace"))
    return found[-1][1] if found else None


def retained_versions() -> list[str]:
    """Versions held in any SURFACE version store.

    Read from the DIRECTORIES, never from a list: both stores document an
    "adding a version" procedure done by hand, and a hand-kept index of a
    hand-filled directory is two places to fall out of step.
    """
    out: set[str] = set()
    for name in VERSION_STORES:
        store = REPO / name
        if store.is_dir():
            out |= {d.name for d in store.iterdir() if d.is_dir()}
    return sorted(out)


def archived_versions_of(row: Row) -> list[str]:
    """Versions of THIS artifact that a store actually holds a copy of.

    Deliberately per-artifact and not per-store. Three successive versions of
    this report were wrong in the same direction, each looser than the last:
    "a store exists", then "a store holds the live version", then "a store
    holds some version". All three said b052's surface was safe because the
    TYPEDB store held 1.29.14 -- a claim about a different artifact entirely.
    The guarantee the policy actually makes is narrow: regenerating this
    artifact must not destroy the answers it holds RIGHT NOW, which is true
    only if THIS artifact's CURRENT version is archived somewhere.
    """
    out: list[str] = []
    src = REPO / row.artifact
    for store_name in VERSION_STORES:
        store = REPO / store_name
        if not store.is_dir():
            continue
        for d in sorted(store.iterdir()):
            if not d.is_dir():
                continue
            held = {f.name for f in d.iterdir()}
            if src.is_dir():
                hit = bool({f.name for f in src.iterdir()} & held)
            else:
                hit = src.name in held
            if hit:
                out.append(d.name)
    return sorted(set(out))


def is_retained(row: Row) -> bool:
    """Does the version store actually hold a copy of THIS artifact?

    Not "is the store non-empty" and not "does the store know this version".
    The store holds Openplanet type DUMPS, so it retains
    `tests/fixtures/typedb` and retains nothing about a corpus that merely
    DESCRIBES the surface. Asserting the weaker thing produced a report saying
    b052's surface "IS retained" when regenerating it would lose the 1.29.5
    answers entirely.
    """
    if row.kind == "VERSIONS":
        return True
    names: set[str] = set()
    for store_name in VERSION_STORES:
        store = REPO / store_name
        if not store.is_dir():
            continue
        names |= {f.name for d in store.iterdir() if d.is_dir() for f in d.iterdir()}
    if not names:
        return False
    src = REPO / row.artifact
    if src.is_dir():
        return bool({f.name for f in src.iterdir()} & names)
    return src.name in names


def surface_retention_problems(rows: list[Row], live: str | None) -> list[str]:
    """Is SURFACE history actually being kept, or only declared?

    A retention POLICY that nothing checks is a comment. The policy says a
    SURFACE artifact's previous version must survive a refresh of the primary
    fixture, so the question with teeth is whether the store holds BOTH the
    version the artifact currently carries AND the live one. If it does, a
    refresh loses nothing and the primary fixture's pin is a deliberate choice;
    if it does not, refreshing would destroy surface no probe can recover --
    the same irreversibility as B148's preamble, at a much larger scale.
    """
    problems: list[str] = []
    held = set(retained_versions())
    if not held:
        return [f"{' and '.join(VERSION_STORES)} hold no versions at all; SURFACE "
                f"retention is declared by the manifest and implemented by nothing"]
    for r in rows:
        if r.volatility != SURFACE or r.kind == "VERSIONS":
            continue
        mine = archived_versions_of(r)
        if not mine:
            problems.append(
                f"{r.artifact} is SURFACE but NOTHING archives it: regenerating it "
                f"replaces the only copy. Copy it into "
                f"docs/probes/versions/<its stamped version>/ before the next run."
            )
            continue
        if r.engine_version != "UNKNOWN" and r.engine_version not in mine:
            problems.append(
                f"{r.artifact} is stamped {r.engine_version} but the only archived "
                f"copies are {mine}: regenerating it would destroy the version it "
                f"currently holds"
            )
    return problems


def classify_without_running(row: Row) -> Result:
    """The verdict for an artifact nobody regenerated.

    Every branch here is UNCHECKED with a distinct reason. None of them is
    CLEAN: not having looked is not agreement.
    """
    path = REPO / row.artifact
    if not path.exists():
        return Result(row, ERROR, "missing", f"{row.artifact} does not exist")
    if row.generator == "NONE":
        return Result(
            row, UNCHECKED, "no-generator",
            "engine gold with no script in this repo; regenerating it means a human "
            "rebuilding the probe from the preamble by hand",
        )
    if row.generator == "MANUAL":
        return Result(
            row, UNCHECKED, "manual-re-verify",
            "hand-transcribed engine ground truth in a source comment; there is no "
            "output file to overwrite, so it can only be re-verified by a human",
        )
    return Result(row, UNCHECKED, "not-run", "audit only; pass --regen to measure")


def check_row_count(row: Row) -> Result | None:
    """Staleness check that needs no game: does `rows` match the file?"""
    if row.rows == "-":
        return None
    path = REPO / row.artifact
    if not path.exists():
        return Result(row, ERROR, "missing", f"{row.artifact} does not exist")
    actual = data_rows(path)
    try:
        claimed = int(row.rows)
    except ValueError:
        return Result(row, ERROR, "bad-row-count", f"{row.rows!r} is not a number")
    if claimed != actual:
        return Result(
            row, DRIFT, "row-count",
            f"manifest says {claimed} rows, file has {actual}",
        )
    return None


def regenerate(row: Row, live: str | None) -> Result:
    """Rerun one generator and diff the result against what is committed."""
    if row.generator in ("NONE", "MANUAL"):
        return classify_without_running(row)

    if row.engine_version == "UNKNOWN":
        return Result(
            row, UNCHECKED, "version-unknown",
            "the artifact records no engine version, so a diff against the live game "
            "cannot distinguish drift from a version change",
        )
    if live is None:
        return Result(
            row, UNCHECKED, "no-live-version",
            "could not read the live Openplanet version; refusing to diff against an "
            "assumed one",
        )
    if live != row.engine_version:
        return Result(
            row, UNCHECKED, "version-moved",
            f"artifact was produced against {row.engine_version}, live game is {live}; "
            f"rerun and RESTAMP rather than reading the difference as drift",
        )

    path = REPO / row.artifact
    before = path.read_text(encoding="utf8") if path.exists() else None
    proc = subprocess.run(
        row.generator, shell=True, cwd=REPO,
        capture_output=True, text=True,
    )
    if proc.returncode != 0:
        tail = (proc.stderr or proc.stdout or "").strip().splitlines()
        return Result(
            row, ERROR, "generator-failed",
            f"exit {proc.returncode}: {tail[-1] if tail else 'no output'}",
        )
    if not path.exists():
        return Result(row, ERROR, "no-output", "generator succeeded but wrote no file")
    after = path.read_text(encoding="utf8")
    if before is None:
        return Result(row, DRIFT, "new", "artifact did not exist before this run")
    if after == before:
        return Result(row, CLEAN, "identical", f"{data_rows(path)} rows")
    return Result(
        row, DRIFT, "content",
        f"{data_rows(path)} rows now, {len(before.splitlines())} lines before",
        dependents_unchecked=[d for d in row.dependents.split(";") if d != "-"],
    )


def run(regen: bool, only: str | None, engine_version: str | None = None) -> int:
    try:
        rows = parse_manifest()
    except ManifestError as e:
        print(f"MANIFEST ERROR: {e}", file=sys.stderr)
        return 2

    stray = unregistered_corpora(rows)
    selected = [r for r in rows if not only or only in r.artifact]

    # B139: assert PRESENCE before reporting a verdict. A selector that matches
    # nothing, or a manifest that parsed to a handful of rows, would otherwise
    # produce a clean report with no findings in it.
    if not selected:
        print(
            f"NOTHING SELECTED: --only {only!r} matched none of {len(rows)} manifest rows",
            file=sys.stderr,
        )
        return 2

    live = engine_version or live_engine_version()

    results: list[Result] = []
    for row in selected:
        stale = check_row_count(row)
        if stale is not None:
            results.append(stale)
            continue
        results.append(regenerate(row, live) if regen else classify_without_running(row))

    print(f"=== B129 GOLD REGEN REPORT ({'regen' if regen else 'audit'}) ===")
    print(f"manifest: {MANIFEST.relative_to(REPO)}  rows: {len(rows)}  selected: {len(selected)}")
    build = live_engine_build() if engine_version is None else None
    print(f"live engine: {live or 'UNREADABLE'}" + (f"  ({build})" if build else ""))
    print()
    width = max(len(r.row.artifact) for r in results)
    for res in results:
        print(f"  {res.status:<9} {res.row.artifact:<{width}}  {res.reason}")
        if res.detail:
            print(f"  {'':<9} {'':<{width}}  {res.detail}")
        for dep in res.dependents_unchecked:
            print(f"  UNCHECKED {dep:<{width}}  restates an artifact that just changed")

    # VERSION CROSS-CHECK. Useful without a regen: it says which artifacts a
    # rerun would have to RESTAMP rather than merely rewrite. Three engine
    # generations are cited concurrently in this repo, so "the file differs"
    # and "the game moved" are different findings and are reported separately.
    if live:
        moved = [r for r in results
                 if r.row.engine_version not in ("UNKNOWN", live) and r.row.kind != "PRIMARY"]
        unstamped = [r for r in results if r.row.engine_version == "UNKNOWN"]
        if moved or unstamped:
            print()
            print(f"VERSION CROSS-CHECK against the live engine ({live}):")
            for r in moved:
                print(f"  STAMPED-OLDER  {r.row.artifact}  recorded {r.row.engine_version}")
            for r in unstamped:
                print(f"  UNSTAMPED      {r.row.artifact}  no version recorded")
            print("  A rerun RESTAMPS these. Their content differing from the live game is")
            print("  a version change, not drift, and the runner will not call it drift.")

    # SURFACE RETENTION (B151). Declared in the manifest, checked here.
    held = retained_versions()
    surface = [r for r in results if r.row.volatility == SURFACE]
    if surface:
        print()
        print(f"SURFACE RETENTION — stores hold {held or 'NOTHING'}:")
        for r in surface:
            v = r.row.engine_version
            if r.row.kind == "VERSIONS":
                note = "the store itself"
            else:
                mine = archived_versions_of(r.row)
                if not mine:
                    note = f"stamped {v}; NOT ARCHIVED — a regen destroys this copy"
                elif v == "UNKNOWN":
                    note = f"UNSTAMPED; archived copies exist for {mine}"
                elif v in mine:
                    note = (f"at {v}, archived; a regen to {live or 'a new version'} "
                            f"keeps these answers")
                else:
                    note = f"stamped {v} but only {mine} archived"
            print(f"  {r.row.artifact:<46} {note}")
    retention = surface_retention_problems(rows, live)
    if retention:
        print()
        print("SURFACE RETENTION PROBLEMS:")
        for pr in retention:
            print(f"  {pr}")

    hybrids = [r.row.artifact for r in results if r.row.volatility == HYBRID]
    if hybrids:
        print()
        print("HYBRID artifacts — a language rule measured over a game-surface")
        print("enumeration. ROW-ADDED/ROW-REMOVED is EXPECTED on a game update and is")
        print("not a language finding; BEHAVIOUR-CHANGED still is:")
        for h in hybrids:
            print(f"  {h}")

    counts = {s: sum(1 for r in results if r.status == s) for s in (CLEAN, DRIFT, UNCHECKED, ERROR)}
    print()
    print("  " + "  ".join(f"{k}={v}" for k, v in counts.items()))

    if stray:
        print()
        print("UNREGISTERED CORPORA (present on disk, absent from the manifest):")
        for s in stray:
            print(f"  {s}")

    orphans = [r.row.artifact for r in results if r.reason == "no-generator"]
    if orphans:
        print()
        print("NO GENERATOR EXISTS for these artifacts. Reported every run, because an")
        print("artifact that cannot be regenerated is a finding, not an omission:")
        for o in orphans:
            print(f"  {o}")

    print()
    if stray:
        print("FAILED: a corpus on disk is not registered in the manifest.")
        return 1
    if counts[ERROR]:
        print(f"FAILED: {counts[ERROR]} artifact(s) errored.")
        return 1
    if regen and counts[CLEAN] + counts[DRIFT] == 0:
        # The over-condemn guard, pointed at this runner itself: a --regen run
        # in which every artifact came back UNCHECKED measured nothing, and a
        # report with no failures in it would otherwise read as success.
        print("FAILED: NOTHING-MEASURED — a --regen run verified no artifact at all.")
        return 1
    if counts[DRIFT]:
        print(f"DRIFT: {counts[DRIFT]} artifact(s) differ. Nothing was committed; review and commit deliberately.")
        return 1
    if regen:
        print(f"CLEAN: {counts[CLEAN]} artifact(s) regenerated identical.")
    else:
        print("AUDIT ONLY: no artifact was regenerated, so none is reported CLEAN.")
    return 0


# ---------------------------------------------------------------- selftest


def _selftest() -> int:
    """Offline checks of the runner's own logic. No game needed."""
    import tempfile

    failures: list[str] = []

    def check(name: str, cond: bool, why: str = "") -> None:
        if not cond:
            failures.append(f"{name}: {why}")

    rows = parse_manifest()
    check("manifest parses", len(rows) > 0, "no rows")
    check("manifest has corpora", any(r.kind == "CORPUS" for r in rows), "no CORPUS row")
    check("manifest has primary", any(r.kind == "PRIMARY" for r in rows), "no PRIMARY row")

    # Every row count in the manifest matches the file on disk.
    for r in rows:
        stale = check_row_count(r)
        check(f"row count {r.artifact}", stale is None, stale.detail if stale else "")

    # POSITIVE CONTROL for check_row_count. Without this, a mutation making it
    # return None unconditionally passes every "row count" check above, since
    # those assert `stale is None`. A guard that cannot fail is not a guard.
    probe_ctl = REPO / "docs/probes/zz-selftest-rowcount.tsv"
    probe_ctl.write_text("# selftest control\ncol\na\nb\nc\n", encoding="utf8")
    try:
        ctl_row = Row(
            artifact=probe_ctl.relative_to(REPO).as_posix(), kind="CORPUS",
            question="does check_row_count detect a mismatch?", generator="NONE",
            engine_version="1.29.5", probe_date="2026-09-01", rows="99",
            diff="STRING", dependents="-", volatility="SEMANTICS",
        )
        got = check_row_count(ctl_row)
        check("row-count mismatch is detected", got is not None and got.status == DRIFT,
              f"a corpus with 3 rows claiming 99 was not reported: {got}")
        ctl_row.rows = "3"
        check("row-count match is not a false DRIFT", check_row_count(ctl_row) is None,
              "a correct row count was reported as drift")
    finally:
        probe_ctl.unlink()

    # OVER-CONDEMN CONTROL. A classifier that returns ERROR for everything
    # satisfies every "not CLEAN" check above, so those alone cannot tell a
    # working runner from one that condemns the whole repo. A healthy,
    # present artifact must come back UNCHECKED with a benign reason.
    healthy = next((r for r in rows if r.kind == "CORPUS" and r.generator not in ("NONE", "MANUAL")), None)
    check("a healthy corpus exists to test", healthy is not None, "none found")
    if healthy is not None:
        res = classify_without_running(healthy)
        check("healthy corpus is not condemned", res.status == UNCHECKED,
              f"a present, generator-backed corpus was reported {res.status}")
        check("healthy corpus reason is not-run", res.reason == "not-run",
              f"got reason {res.reason!r}")

    # Directory scan finds every committed corpus and flags none as stray.
    check("no stray corpora", unregistered_corpora(rows) == [], str(unregistered_corpora(rows)))

    # A corpus that is NOT in the manifest must be reported.
    with tempfile.TemporaryDirectory():
        fake = REPO / "docs/probes/zz-selftest-stray.tsv"
        fake.write_text("# selftest\ncol\nrow\n", encoding="utf8")
        try:
            stray = unregistered_corpora(rows)
            check("stray corpus is detected", fake.relative_to(REPO).as_posix() in stray,
                  "an unregistered corpus was not reported")
        finally:
            fake.unlink()
    check("stray detection is reversible", unregistered_corpora(rows) == [], "fixture leaked")

    # NOT-RUN is never CLEAN.
    for r in rows:
        res = classify_without_running(r)
        check(f"audit never CLEAN {r.artifact}", res.status != CLEAN,
              "an unrun artifact was reported CLEAN")
        check(f"audit reason present {r.artifact}", bool(res.reason), "UNCHECKED with no reason")

    # An UNKNOWN version refuses to diff even with a live game present.
    #
    # SYNTHETIC, for the reason B148 hit and B151 hit again: a test that reaches
    # into real repo state to find its subject has a SHELF LIFE, and it expires
    # ON SUCCESS. This check originally selected a real UNKNOWN-stamped row;
    # B151 restamped b117 and b119, and every restamp brings the day the
    # subject runs out and the check goes red like a regression. The guard is
    # about the CODE PATH, so it supplies its own subject.
    unk = Row(
        artifact="docs/probes/b117-member-visibility.tsv", kind="CORPUS",
        question="does an unstamped artifact refuse to diff against a live game?",
        generator="python3 scripts/probe_member_visibility.py", engine_version="UNKNOWN",
        probe_date="2026-09-01", rows="-", diff="STRING", dependents="-",
        volatility="SEMANTICS",
    )
    res = regenerate(unk, live="1.29.14")
    check("UNKNOWN version refuses", res.status == UNCHECKED and res.reason == "version-unknown",
          f"got {res.status}/{res.reason}")
    # The count of real UNKNOWNs is REPORTED instead, so progress is visible
    # without a test depending on the repo staying broken.
    real_unknown = [r.artifact for r in rows if r.engine_version == "UNKNOWN"]
    print(f"  manifest rows still engine_version=UNKNOWN: {len(real_unknown)} {real_unknown}")

    # A version MISMATCH refuses to diff, and says restamp rather than drift.
    known = next((r for r in rows if r.engine_version == "1.29.5" and r.generator not in ("NONE", "MANUAL")), None)
    check("a stamped corpus exists to test", known is not None, "none found")
    if known is not None:
        res = regenerate(known, live="1.29.99")
        check("version mismatch refuses", res.status == UNCHECKED and res.reason == "version-moved",
              f"got {res.status}/{res.reason}")
        res2 = regenerate(known, live=None)
        check("absent live version refuses", res2.status == UNCHECKED and res2.reason == "no-live-version",
              f"got {res2.status}/{res2.reason}")

    # Orphans and PRIMARY rows are UNCHECKED with their own distinct reasons,
    # never folded into not-run.
    # The no-generator branch is exercised with a SYNTHETIC row rather than by
    # requiring the manifest to contain a real orphan. B148 gave b067 and b071
    # generators, which emptied the orphan set and made the original form of
    # this check fail for a good reason — the guard was fine, its SUBJECT had
    # gone. A guard whose test depends on the repo currently being broken
    # stops being testable exactly when the repo gets fixed.
    synthetic_orphan = Row(
        artifact="docs/probes/b052-scope-provenance.tsv", kind="CORPUS",
        question="does the no-generator branch report its own reason?",
        generator="NONE", engine_version="1.29.14", probe_date="2026-09-01",
        rows="-", diff="STRING", dependents="-", volatility="SURFACE",
    )
    check("orphan reason", classify_without_running(synthetic_orphan).reason == "no-generator",
          f"got {classify_without_running(synthetic_orphan).reason!r}")
    check("orphan is UNCHECKED, not CLEAN",
          classify_without_running(synthetic_orphan).status == UNCHECKED, "")
    # And the manifest's real orphan count is REPORTED, so emptying it is a
    # visible event rather than a silent one.
    real_orphans = [r.artifact for r in rows if r.generator == "NONE"]
    print(f"  manifest orphans (generator=NONE): {len(real_orphans)} {real_orphans}")
    primary = next((r for r in rows if r.kind == "PRIMARY"), None)
    if primary is not None:
        check("primary reason", classify_without_running(primary).reason == "manual-re-verify", "")

    # THE VERSION READER. The whole version-refusal mechanism rests on this
    # returning the RIGHT version or None, so it gets its own controls rather
    # than being trusted because the refusals above pass.
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        multi = tdp / "multi.log"
        multi.write_text(
            "[Platform] [LOG] [1]  Openplanet: 1.29.5 (next, Public, aaaaaaaa)\n"
            "...\n"
            "[Platform] [LOG] [2]  Openplanet: 1.29.14 (next, Public, 6fd4200f)\n",
            encoding="utf8",
        )
        # The log accumulates across sessions; only the LAST line describes the
        # engine a probe would hit now. Reading the first would silently
        # report a stale generation as live, which is worse than reading none.
        check("last version line wins", live_engine_version(multi) == "1.29.14",
              f"got {live_engine_version(multi)!r}")
        check("build descriptor is read", live_engine_build(multi) == "next, Public, 6fd4200f",
              f"got {live_engine_build(multi)!r}")

        bare = tdp / "bare.log"
        bare.write_text("[Platform] [LOG] nothing useful here\n", encoding="utf8")
        check("a log with no version line returns None", live_engine_version(bare) is None,
              "a version was invented from a log that has none")

        check("a missing log returns None", live_engine_version(tdp / "nope.log") is None,
              "a version was invented from a log that does not exist")

    # THE SUBPROCESS PATH ITSELF. Every check above exercises regenerate()'s
    # REFUSAL branches with an injected version; none of them ever ran a
    # generator. That left the actual --regen path — spawn, compare, classify
    # — with no coverage at all, which is the half that does the work. These
    # use a harmless generator against a scratch artifact, so they need no
    # game and rewrite nothing committed.
    scratch = REPO / "docs/probes/zz-selftest-regen.tsv"

    def scratch_row(gen: str) -> Row:
        return Row(
            artifact=scratch.relative_to(REPO).as_posix(), kind="CORPUS",
            question="does the subprocess path classify correctly?", generator=gen,
            engine_version="1.29.14", probe_date="2026-09-01", rows="-",
            diff="STRING", dependents="-", volatility="SEMANTICS",
        )

    try:
        # Identical output -> CLEAN.
        scratch.write_text("col\na\n", encoding="utf8")
        res = regenerate(scratch_row(f"printf 'col\\na\\n' > {scratch}"), live="1.29.14")
        check("a generator reproducing the file is CLEAN", res.status == CLEAN,
              f"got {res.status}/{res.reason}")

        # Different output -> DRIFT, and the dependents ride along.
        row = scratch_row(f"printf 'col\\nb\\n' > {scratch}")
        row.dependents = "src/config.rs"
        res = regenerate(row, live="1.29.14")
        check("a generator changing the file is DRIFT", res.status == DRIFT,
              f"got {res.status}/{res.reason}")
        check("DRIFT carries its dependents", res.dependents_unchecked == ["src/config.rs"],
              f"got {res.dependents_unchecked}")

        # A failing generator is ERROR, never CLEAN and never DRIFT.
        scratch.write_text("col\na\n", encoding="utf8")
        res = regenerate(scratch_row("exit 3"), live="1.29.14")
        check("a failing generator is ERROR", res.status == ERROR and res.reason == "generator-failed",
              f"got {res.status}/{res.reason}")

        # A generator that succeeds but writes nothing is ERROR, not CLEAN.
        scratch.unlink()
        res = regenerate(scratch_row("true"), live="1.29.14")
        check("a generator writing no file is ERROR", res.status == ERROR and res.reason == "no-output",
              f"got {res.status}/{res.reason}")
    finally:
        scratch.unlink(missing_ok=True)
    check("scratch artifact removed", not scratch.exists(), "a selftest fixture was stranded (B077)")

    # SURFACE RETENTION (B151). The policy is only worth the check behind it.
    check("a version store exists", bool(retained_versions()),
          "no store holds any version; SURFACE retention would be declared and "
          "implemented by nothing")
    # Both stores must be read, and the version SET cannot show that: they both
    # hold 1.29.5 and 1.29.14, so dropping one left the check green and the
    # mutation harness called the guard vacuous. An artifact archived ONLY in
    # the corpus store does distinguish them.
    b052 = next((r for r in rows
                 if r.artifact == "docs/probes/b052-qualified-enum-spellings.tsv"), None)
    check("b052 row exists", b052 is not None, "")
    if b052 is not None:
        check("both stores are read", archived_versions_of(b052) != [],
              "b052 is archived only in docs/probes/versions, so an empty result "
              "means that store is not being read at all")

    # An artifact the stores DO archive, and one they do not. Both directions,
    # because a retention check that only ever says "fine" is not a check.
    typedb_row = next((r for r in rows if r.artifact == "tests/fixtures/typedb"), None)
    check("typedb row exists", typedb_row is not None, "")
    if typedb_row is not None:
        check("an archived artifact is detected", "1.29.5" in archived_versions_of(typedb_row),
              f"got {archived_versions_of(typedb_row)}")

    absent = Row(
        artifact="docs/probes/b119-const-receiver.tsv", kind="CORPUS",
        question="is an unarchived SURFACE artifact reported?", generator="x",
        engine_version="1.29.14", probe_date="2026-09-01", rows="-",
        diff="STRING", dependents="-", volatility=SURFACE,
    )
    check("an unarchived artifact is detected", archived_versions_of(absent) == [],
          f"a corpus no store holds reported archives {archived_versions_of(absent)}")
    check("an unarchived SURFACE artifact is a problem",
          any("NOTHING archives it" in p for p in surface_retention_problems([absent], "1.29.14")),
          "an unarchived SURFACE artifact produced no retention problem")

    # A SEMANTICS artifact is deliberately NOT retained, and must not be
    # reported as a problem — otherwise the label would be decoration and every
    # semantics corpus would demand an archive nobody wants.
    sem = Row(**{**absent.__dict__, "volatility": SEMANTICS})
    check("a SEMANTICS artifact needs no archive",
          surface_retention_problems([sem], "1.29.14") == [],
          "a SEMANTICS artifact was told to keep per-version history")

    # Volatility is validated, not merely stored.
    check("volatility vocabulary is closed", KNOWN_VOLATILITY == {SURFACE, SEMANTICS, HYBRID})
    # An INVALID volatility must be refused at parse time. Reading the value off
    # an already-valid manifest cannot test the validator: disabling it left
    # every check above green, and the mutation harness reported the guard
    # vacuous.
    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as fh:
        fh.write("\t".join(COLUMNS) + "\n")
        fh.write("\t".join(["docs/probes/x.tsv", "CORPUS", "does it?", "NONE",
                            "1.29.5", "2026-09-01", "-", "STRING", "-",
                            "WHENEVER"]) + "\n")
        badvol = Path(fh.name)
    try:
        why = None
        try:
            parse_manifest(badvol)
        except ManifestError as e:
            why = str(e)
        check("bad volatility raises", why is not None and "unknown volatility" in why,
              f"an unrecognised volatility was accepted: {why}")
    finally:
        badvol.unlink()
    for r in rows:
        check(f"volatility {r.artifact}", r.volatility in KNOWN_VOLATILITY, r.volatility)
    check("the manifest uses every volatility",
          {r.volatility for r in rows} == KNOWN_VOLATILITY,
          f"only {sorted({r.volatility for r in rows})} present; a vocabulary with an "
          f"unused value is untested")

    # A malformed manifest is an ERROR, not an empty clean run.
    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as fh:
        fh.write("# only a comment\n")
        bad = Path(fh.name)
    try:
        kind = None
        try:
            parse_manifest(bad)
        except ManifestError:
            kind = "ManifestError"
        except Exception as e:  # noqa: BLE001 - the point is to name what escaped
            kind = type(e).__name__
        check("empty manifest raises", kind == "ManifestError",
              f"an empty manifest did not raise a clean ManifestError (got {kind})")
    finally:
        bad.unlink()

    # A manifest that is VALID in every respect except its column NAMES. It
    # has 9 columns, a known kind and a known diff mode, so it parses cleanly
    # once the header check is removed — which is what makes it a precise test
    # OF THAT CHECK rather than of the guards downstream of it.
    renamed = "\t".join(["artifact", "kind", "QUESTION_RENAMED", "generator",
                         "engine_version", "probe_date", "rows", "diff", "dependents"])
    body = "\t".join(["docs/probes/x.tsv", "CORPUS", "does it?", "NONE",
                      "1.29.5", "2026-09-01", "-", "STRING", "-"])
    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as fh:
        fh.write(renamed + "\n" + body + "\n")
        bad2 = Path(fh.name)
    try:
        why = None
        try:
            parse_manifest(bad2)
        except ManifestError as e:
            why = str(e)
        check("wrong columns raise", why is not None and "columns are" in why,
              f"a wrong-column manifest was accepted, or failed for another reason: {why}")
    finally:
        bad2.unlink()

    total = 0
    for _ in rows:
        total += 1
    check("checked every row", total == len(rows), "")

    print(f"regen-golds selftest: {len(rows)} manifest rows, {len(failures)} failure(s)")
    for f in failures:
        print(f"  FAIL {f}")
    if not rows:
        print("FAILED: selftest ran against zero rows and would pass vacuously")
        return 1
    return 1 if failures else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--regen", action="store_true", help="rerun generators against the live game")
    ap.add_argument("--only", help="restrict to artifacts whose path contains this substring")
    ap.add_argument("--selftest", action="store_true", help="offline checks of this runner")
    ap.add_argument("--json", action="store_true", help="machine-readable manifest dump")
    ap.add_argument(
        "--engine-version",
        help="state the live Openplanet version explicitly instead of reading it from "
        "the game log. For a probing setup whose log this script cannot find — never "
        "to silence a version-moved refusal you have not actually resolved.",
    )
    args = ap.parse_args(argv)

    if args.selftest:
        return _selftest()
    if args.json:
        print(json.dumps([r.__dict__ for r in parse_manifest()], indent=2))
        return 0
    return run(regen=args.regen, only=args.only, engine_version=args.engine_version)


if __name__ == "__main__":
    sys.exit(main())
