#!/usr/bin/env python3
"""Prove `tests/b145_issue_repro_verdicts.rs` can actually fail (B145).

The corpus it guards is engine ground truth that nothing else in the repo can
re-derive: the game is not available in CI, so if these assertions are vacuous
the artifact silently becomes decoration -- which is precisely the state B145
was raised to end. A gate over an artifact nobody can regenerate on demand has
to be shown to bite.

Each mutation edits ONE cell of the committed corpus and requires the Rust test
to go red. Half are OVER-CONDEMN shapes in spirit: a check that refused every
declared divergence, or that demanded one everywhere, would make the artifact
impossible to commit at all -- so the baseline pass is itself load-bearing and
is asserted first.

The corpus is restored after every mutation, and a failure to restore is a hard
error: leaving a mutated engine artifact on disk would be worse than the
vacuity this is looking for.
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
CORPUS = SRC / "docs/probes/b145-issue-repro-verdicts.tsv"
TEST = "b145_issue_repro_verdicts"


def cells(line: str) -> list[str]:
    return line.split("\t")


def load() -> tuple[list[str], list[str], list[list[str]]]:
    """(preamble lines, header cells, data rows)."""
    pre, header, rows = [], None, []
    for line in CORPUS.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            pre.append(line)
            continue
        if header is None:
            header = cells(line)
            continue
        rows.append(cells(line))
    assert header is not None, "corpus has no header row"
    return pre, header, rows


def render(pre: list[str], header: list[str], rows: list[list[str]]) -> str:
    return "\n".join(pre + ["\t".join(header)] + ["\t".join(r) for r in rows]) + "\n"


def col(header: list[str], name: str) -> int:
    return header.index(name)


def find(rows: list[list[str]], header: list[str], **want) -> int:
    """Index of the first row matching every field, or -1."""
    for i, row in enumerate(rows):
        if all(row[col(header, k)] == v for k, v in want.items()):
            return i
    return -1


def mutations(header: list[str], rows: list[list[str]]):
    """(label, mutate) pairs. `mutate` edits a COPY of rows in place."""
    div = col(header, "divergence")
    msg = col(header, "engine_message")
    lsp = col(header, "lsp_today")
    ver = col(header, "verdict")
    evs = col(header, "engine_version")

    rs_all = rows
    gap = next(i for i, r in enumerate(rows)
               if r[div].strip() != "-" and r[lsp] not in ("UNMEASURED",))
    agreeing = next(i for i, r in enumerate(rows)
                    if r[div].strip() == "-" and r[lsp] != "UNMEASURED")
    notrun = find(rows, header, verdict="UNKNOWN")
    assert gap >= 0 and notrun >= 0, "the corpus no longer has the rows this harness anchors on"

    def drop_divergence(rs):
        rs[gap][div] = "-"

    def invent_divergence(rs):
        rs[agreeing][div] = "the LSP has a gap here"

    def blank_message(rs):
        rs[agreeing][msg] = ""

    def move_lsp_answer(rs):
        cur = rs[agreeing][lsp]
        rs[agreeing][lsp] = "DIAGNOSES(9999)" if cur == "CLEAN" else "CLEAN"

    def dress_notrun_as_answer(rs):
        rs[notrun][ver] = "ACCEPT"

    def drop_a_fixture(rs):
        rs.pop(agreeing)

    def unstamp_version(rs):
        rs[agreeing][evs] = "whenever"

    def leak_staging_id(rs):
        rs[agreeing][msg] = "lsp-issue-repro-fx/Main.as(1,1): something"

    dfor = col(header, "divergence_for")
    comp = col(header, "engine_compiled")
    noted = next(i for i, r in enumerate(rs_all) if r[div].strip() != "-")
    aborted = next(i for i, r in enumerate(rs_all)
                   if r[comp].strip() == "0"
                   and r[col(header, "build_status")] == "REJECTED"
                   and r[lsp] not in ("CLEAN", "UNMEASURED"))

    def unstamp_note(rs):
        rs[noted][dfor] = "-"

    def stamp_for_another_state(rs):
        rs[noted][dfor] = "ACCEPT|DIAGNOSES(9999)"

    def stamp_with_no_note(rs):
        rs[agreeing][dfor] = "REJECT|CLEAN"

    def drop_declaration_note(rs):
        rs[aborted][div] = "-"
        rs[aborted][dfor] = "-"

    def hide_the_abort(rs):
        rs[aborted][comp] = "7"

    return [
        ("a divergence note loses its stamp, so a reason typed straight into "
         "the file can never expire — the B157/B168 shape",
         unstamp_note),
        ("a divergence note is stamped for a DIFFERENT measurement, which is "
         "how b102's note survived a full polarity flip and still read as a "
         "reason",
         stamp_for_another_state),
        ("a stamp appears on a row with no note behind it",
         stamp_with_no_note),
        ("the reason is deleted from a row where the engine never reached a "
         "call site, so a declaration-time rejection passes as agreement",
         drop_declaration_note),
        ("a declaration-time abort is recorded as having compiled bodies, so "
         "the rule can never fire on it",
         hide_the_abort),
        ("a declared divergence is DELETED from a row where the engine and "
         "this LSP genuinely disagree — the gap becomes invisible",
         drop_divergence),
        ("a divergence is declared on a row where the two AGREE — a stale note "
         "is how a fixed gap keeps being described as an open one",
         invent_divergence),
        ("a row's engine message is blanked, leaving a polarity and no "
         "evidence — the message text is the whole point of the artifact",
         blank_message),
        ("a row's recorded LSP answer is changed, as a checker change would "
         "change it — the engine evidence beside it was measured against the "
         "old answer",
         move_lsp_answer),
        ("a verdict the probe could NOT take is dressed as an accepted build",
         dress_notrun_as_answer),
        ("a fixture's row is deleted, so it silently carries no engine "
         "evidence again",
         drop_a_fixture),
        ("a row loses its engine version stamp, so it cannot be diffed across "
         "generations",
         unstamp_version),
        ("an engine message keeps the probe's staging folder id, which makes "
         "the record churn for reasons that are not about the engine",
         leak_staging_id),
    ]


def run_test() -> int:
    return subprocess.run(
        ["cargo", "test", "--test", TEST],
        cwd=SRC, capture_output=True, text=True,
    ).returncode


def main() -> int:
    if not CORPUS.is_file():
        print(f"no corpus at {CORPUS}", file=sys.stderr)
        return 2
    pristine = CORPUS.read_text()
    backup = Path(tempfile.mkdtemp(prefix="b145-mut-")) / "corpus.tsv"
    backup.write_text(pristine)

    pre, header, rows = load()
    try:
        if run_test() != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for label, mutate in mutations(header, rows):
            copy = [list(r) for r in rows]
            mutate(copy)
            CORPUS.write_text(render(pre, header, copy))
            if run_test() != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}  <-- the gate is vacuous here",
                      file=sys.stderr)
                missed += 1
            CORPUS.write_text(pristine)
        total = caught + missed
        print(f"\n{caught}/{total} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1
    finally:
        # Restoring is not optional: a mutated engine artifact left on disk is
        # worse than the vacuity this harness looks for, because the next
        # reader has no way to tell it was ever edited.
        #
        # This RAISES rather than returning. A `return` inside `finally`
        # discards any exception in flight -- so a crash mid-run would exit 0
        # through here, and the one path that must never report success
        # quietly is the one that failed to put the artifact back.
        CORPUS.write_text(backup.read_text())
        if CORPUS.read_text() != pristine:
            raise SystemExit(
                "FATAL: the corpus was NOT restored; recover it with "
                "`git checkout -- docs/probes/b145-issue-repro-verdicts.tsv`")
        shutil.rmtree(backup.parent, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
