#!/usr/bin/env python3
"""Prove `issue_repro_game.py --selftest` is non-vacuous (B077).

A test suite that still passes once the protection is removed tests nothing.
Each mutation below deletes or neuters exactly ONE guard in
`scripts/issue_repro_game.py`, in a throwaway copy of `scripts/`; the selftest
must go red for every one of them. `just probe-cleanup-mutants`.

Mutations are applied to a COPY, never to the checkout, so an interrupted run
cannot leave a neutered guard behind -- the same property the script under test
is about.

An anchor that no longer matches, or that matches more than once, is reported
as a MISS rather than skipped: a mutation that silently failed to apply would
otherwise be scored as a pass and this harness would itself become the vacuous
thing it exists to detect.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

# (label, old, new)
MUTATIONS = [
    ("__enter__ cleanup on partial stage",
     "            if self.lease is not None and remove_tree(self.dest, lease=self.lease):\n"
     "                _STAGED.pop(self.dest, None)\n            raise",
     "            raise"),
    ("signal handlers",
     "    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):",
     "    for sig in ():"),
    ("atexit hook",
     "    atexit.register(_cleanup_staged)",
     "    pass  # atexit removed"),
    ("second probe dir prefix",
     'PROBE_DIR_PREFIXES = (PROBE_PREFIX, "lsp-enum-corpus-probe")',
     'PROBE_DIR_PREFIXES = (PROBE_PREFIX,)'),
    ("early registration in __enter__",
     "            _STAGED[self.dest] = self.lease",
     "            pass  # registration removed"),
    ("contamination guard in probe()",
     "    strays = probe_dirs_present(root, allow={fid})",
     "    strays = []"),
    ("empty-only sweeping (any dir looks empty)",
     "        return d.is_dir() and not d.is_symlink() and not any(d.iterdir())",
     "        return True"),
    ("live-pid protection",
     "        if pid is not None and pid != os.getpid() and pid_alive(pid):",
     "        if False:"),
    ("removal verification in remove_tree",
     "    return not _path_present(d)",
     "    return True"),
    ("non-empty-dir protection in sweep",
     "        if not is_empty_dir(d):",
     "        if False:"),
    # NOTE: this mutation makes the guard PERMIT rather than deleting it.
    # Deleting the call would let the un-injected probe reach the REAL game
    # during mutation testing -- the exact defect under test, committed by the
    # harness that checks for it. `fleet.rb_alive` stays replaced; it just
    # stops refusing. Same red, zero traffic.
    ("selftest game-access revocation (guard permits instead of refusing)",
     "    def refuse(*_a, **_k):\n        raise AssertionError(",
     "    def refuse(*_a, **_k):\n        return False\n        raise AssertionError("),
    ("emergency-teardown failure report",
     '    if stuck:\n        print(',
     '    if False:\n        print('),

    # --- B118: the three false-green channels ---------------------------
    # Each of these RESTORES one half of the original defect. The selftest
    # must go red for every one, or the pair-cases added for B118 are
    # decoration. They are listed as 4-tuples because two of them live in
    # fleet_dogfood.py.
    ("B118 ch1: probe defaults back to the TRAC-filtering window",
     "issue_repro_game.py",
     "    read_log_window = read_log_window_fn or fleet.read_log_window_raw",
     "    read_log_window = read_log_window_fn or fleet.read_log_window"),
    ("B118 ch1: build_ran no longer gates the verdict",
     "fleet_dogfood.py",
     '    if not build_ran:\n        status = "NOT-RUN"',
     '    if False:\n        status = "NOT-RUN"'),
    ("B118 ch1: build_slice returns lines even when the build never started",
     "fleet_dogfood.py",
     "    if start is None:\n        return []",
     "    if start is None:\n        start = 0"),
    ("B118 ch2: raw_build_errors stops excluding LOCATED diagnostics "
     "(collapses the two error kinds)",
     "fleet_dogfood.py",
     "        and not _LOCATED_DIAG.search(l)",
     "        and True"),
    ("B118 ch2: location-less errors stop reaching a bucket at all",
     "fleet_dogfood.py",
     "    raw_errs = raw_build_errors(window, fid)",
     "    raw_errs = []"),
    ("B118 ch2: BUILD-ERR collapsed into REJECTED",
     "fleet_dogfood.py",
     '        status = "BUILD-ERR"\n    else:\n        status = "CLEAN"',
     '        status = "REJECTED"\n    else:\n        status = "CLEAN"'),
    ("B118 ch3: `Script compilation failed!` no longer counted, so a failed "
     "compile that still logs `Loaded plugin` reads as CLEAN",
     "fleet_dogfood.py",
     "    build_failed = any(BUILD_FAILED in l for l in slice_)",
     "    build_failed = False"),
    ("B118 ch3: `Loaded plugin` treated as SUFFICIENT for a clean build",
     "fleet_dogfood.py",
     "    elif raw_errs or load_error or not build_loaded:",
     "    elif raw_errs or load_error:"),
    # The CONTROL that proves the harness is wired at all -- this attribution
    # predates B118 and already worked; if breaking it does NOT go red, the
    # three cases above are measuring nothing either.
    # --- B127: a success reply is not a report on the build --------------
    ("B127: a clean transport reply certifies the build (the original defect: "
     "`{'error':'', 'data':'Plugin loaded'}` comes back for a build whose "
     "dependency could not be resolved)",
     "fleet_dogfood.py",
     "    elif raw_errs or load_error or not build_loaded:",
     "    elif load_error:"),
    ("B127 over-condemn: every build reported BUILD-ERR (a guard that fires on "
     "everything passes a suite that only checks things get caught)",
     "fleet_dogfood.py",
     '    elif raw_errs or load_error or not build_loaded:\n',
     '    elif True:\n'),
    ("B118 control: own/foreign error attribution stops discriminating",
     "fleet_dogfood.py",
     "    game_errors = [d for d in errs if d[\"file\"].startswith(mine)]",
     "    game_errors = [d for d in errs]"),

    # --- B145: the engine's verdict must be RECORDED, and recorded honestly --
    # The ticket's own two mutations, plus the shapes that make a recorder look
    # right while saying nothing: 64 fixtures got here because a generator
    # printed and nobody stored, so a recorder that WRITES but writes the same
    # thing for every fixture would have been just as invisible.
    ("B145: the recorder writes an EMPTY record for every fixture",
     '        "engine_message": message,',
     '        "engine_message": "",'),
    ("B145: every fixture is recorded as CLEAN, the ticket's own mutation",
     '    verdict = {"CLEAN": "ACCEPT", "REJECTED": "REJECT"}.get(status, "UNKNOWN")',
     '    verdict = "ACCEPT"'),
    ("B145: a probe that NEVER RAN is recorded as an accepted build — silence "
     "read as an answer, which is the shape this artifact exists to stop",
     '    status = rec.get("build_status") or "NOT-RUN"',
     '    status = rec.get("build_status") or "CLEAN"'),
    ("B145: an UNMEASURED LSP is recorded as clean",
     '    lsp_says = "UNMEASURED" if lsp_count is None else (',
     '    lsp_says = "CLEAN" if lsp_count is None else ('),
    ("B145: run_lsp_on_fixture reports a clean LSP when the CLI could not be "
     "run at all",
     '        return {"count": None, "error": f"no LSP binary at {binary}"}',
     '        return {"count": 0, "error": ""}'),
    ("B145: run_lsp_on_fixture reports clean when the count cannot be parsed",
     '        return {"count": None, "error": "no diagnostic count in LSP output"}',
     '        return {"count": 0, "error": ""}'),
    ("B145: the engine message keeps the staging folder id, so the record "
     "churns on a probe-id change and is not about the fixture",
     '    leaf = str(d.get("file", "")).split("/")[-1] or "?"',
     '    leaf = str(d.get("file", "")) or "?"'),
    # --- B145: the engine's WARNINGS are ground truth too ------------------
    # The first run of this recorder dropped them: 37d-variable-shadow came
    # back CLEAN with the boilerplate, while the engine had said `Variable 'x'
    # hides another variable`. That both lost the message text and
    # manufactured a disagreement with an LSP that was right.
    ("B145: engine warnings are dropped, so a warned fixture records the "
     "clean boilerplate and loses the engine's own text",
     "        message = \" | \".join(_engine_text(d) for d in warns) or CLEAN_REASON",
     "        message = CLEAN_REASON"),
    ("B145: a warned fixture is indistinguishable from a silent one, which "
     "turns an LSP that agrees with the engine into a reported gap",
     '    if verdict == "ACCEPT" and warns:\n        verdict = "ACCEPT-WITH-WARNINGS"',
     "    pass"),
    ("B145 OVER-CONDEMN: every accepted fixture is recorded as warned",
     '    if verdict == "ACCEPT" and warns:',
     '    if verdict == "ACCEPT":'),
    ("B145: another session's warning in the shared log is attributed to this "
     "fixture — Openplanet.log is shared, so this is a false verdict at random",
     '            and str(d.get("file", "")).split("/")[0] == fid]',
     "            ]"),
    ("B145: folder_warnings picks up ERRORS too, double-counting them",
     '            if d.get("sev") == "WARN"',
     '            if d.get("sev") in ("WARN", "ERR")'),
    # --- B145 follow-up: a prose reason must EXPIRE when its subject moves --
    # B157/B168's shape, reproduced live in this artifact within one
    # regeneration: b102's note survived a polarity flip and still read as a
    # reason. Prose cannot be verified; it CAN be made to expire.
    ("B145: the stamp is never checked, so a note written against a different "
     "measurement still settles the row — the b102 shape exactly",
     "            if stamp != now:",
     "            if False:"),
    ("B145 OVER-CONDEMN: every note is treated as expired, so no gap can ever "
     "be explained and the artifact cannot be committed at all",
     "            if stamp != now:",
     "            if True:"),
    ("B145: divergence_state collapses the engine verdict, so a note survives "
     "the engine changing its mind",
     '    return f"{verdict}|{lsp_today}"',
     '    return f"{lsp_today}"'),
    ("B145: divergence_state collapses the LSP answer, so a note survives the "
     "checker changing its mind",
     '    return f"{verdict}|{lsp_today}"',
     '    return f"{verdict}"'),
    ("B145: divergence_state is constant, so no note ever expires",
     '    return f"{verdict}|{lsp_today}"',
     '    return "always"'),
    ("B145: an expired note is carried across the regeneration anyway",
     "    if stamp != now:\n        return None\n    return note, now",
     "    return note, now"),
    ("B145 OVER-CONDEMN: carry_forward drops every note, so a regeneration "
     "silently converts every explained gap into an unexplained one",
     "    return note, now",
     "    return None"),
    ("B145: a placeholder is carried as if it were a reason",
     '    if not note or note == "-":\n        return None',
     "    if False:\n        return None"),
    ("B145: a freshly written row is stamped with a constant, so the stamp "
     "stops describing the measurement it was written against",
     '        "divergence_for": (divergence_state(verdict, lsp_says)\n'
     '                           if divergences.get(slug) else "-"),',
     '        "divergence_for": "-",'),

    # --- B145 follow-up: polarity agreement is not the same question -------
    ("B145: the declaration-abort rule is removed, so an engine that never "
     "reached a call site reads as agreeing with an LSP about call sites",
     '            and str(row.get("engine_compiled", "")) == "0"',
     "            and False"),
    ("B145 OVER-CONDEMN: every REJECTED row demands a declaration note, "
     "including the ones where the engine compiled the whole unit",
     '            and str(row.get("engine_compiled", "")) == "0"',
     "            and True"),
    ("B145: a declaration-time abort still counts bodies as compiled, so the "
     "rule can never fire",
     '        if d.get("sev") == "INFO"\n        and str(d.get("msg", "")).startswith("Compiling ")',
     '        if True\n        or str(d.get("msg", "")).startswith("Compiling ")'),
    ("B145: another session's `Compiling` lines are counted as ours, which "
     "inflates the count and hides an abort",
     '        and str(d.get("file", "")).split("/")[0] == fid\n    )',
     "        and True\n    )"),
    ("B145: the compiled-body count is dropped from the record entirely",
     '        "engine_compiled": folder_compiled(rec),',
     '        "engine_compiled": 99,'),
    ("B145: a REQUIRED declaration note is then condemned as a stale one, "
     "which makes the row impossible to satisfy in either direction",
     "        elif agrees and not needs_note and declared != \"-\":",
     "        elif agrees and declared != \"-\":"),
    ("B145: engine/LSP disagreement is never reported",
     "        agrees = engine_silent == (lsp == \"CLEAN\")",
     "        agrees = True"),
    ("B145 OVER-CONDEMN: every row is reported as disagreeing, which would "
     "make the artifact impossible to commit at all",
     "        agrees = engine_silent == (lsp == \"CLEAN\")",
     "        agrees = False"),
    ("B145: a NOT-RUN engine verdict is accepted as evidence",
     '        if status in ("NOT-RUN", "BUILD-ERR"):',
     "        if False:"),
    ("B145: a STALE divergence note on a row that now agrees is not reported, "
     "so a fixed gap keeps being described as an open one",
     '        elif agrees and not needs_note and declared != "-":',
     "        elif False:"),
    ("B145: hand-written divergence notes are dropped on regen, turning every "
     "explained gap into an unexplained one",
     "    if not dest.is_file():\n        return {}",
     "    if True:\n        return {}"),
    ("B145: a tab inside an engine message is written raw, shifting every "
     "later column of that row",
     '        lines.append("\\t".join(str(row[c]).replace("\\t", " ") for c in TSV_COLUMNS))',
     '        lines.append("\\t".join(str(row[c]) for c in TSV_COLUMNS))'),
    ("B145: the corpus header stops naming its columns",
     '    lines = preamble + ["\\t".join(TSV_COLUMNS)]',
     '    lines = preamble + ["fixture"]'),
]

def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/issue_repro_game.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=300,
    ).returncode

def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b077-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        targets = {
            "issue_repro_game.py": work / "scripts" / "issue_repro_game.py",
            "fleet_dogfood.py": work / "scripts" / "fleet_dogfood.py",
        }
        pristines = {k: v.read_text() for k, v in targets.items()}

        if run(work) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for mut in MUTATIONS:
            # B118 mutations live in fleet_dogfood.py, so each entry names its
            # file. A 3-tuple keeps meaning "issue_repro_game.py".
            label, old, new = mut[0], mut[-2], mut[-1]
            fname = mut[1] if len(mut) == 4 else "issue_repro_game.py"
            target, pristine = targets[fname], pristines[fname]
            if old not in pristine:
                print(f"  ANCHOR MISSING: {label} [{fname}]", file=sys.stderr)
                missed += 1
                continue
            # Anchor must be unique, or the mutation is not the one described.
            if pristine.count(old) != 1:
                print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}", file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            rc = run(work)
            if rc != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}  <-- selftest is vacuous here", file=sys.stderr)
                missed += 1
            target.write_text(pristine)

        print(f"\n{caught}/{len(MUTATIONS)} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1

if __name__ == "__main__":
    sys.exit(main())
