#!/usr/bin/env python3
"""Prove `fleet_dogfood.py --selftest` is non-vacuous (B118 / B123 / B127).

`build_verdict` decides whether a build succeeded and `verdict` decides whether
a plugin shows PARITY. Both are guards against a MISSING signal, which red/green
cannot validate on its own — before the fix the silence they now catch was the
silence they reported as success.

Each mutation restores one half of a real defect. Two of them are OVER-CONDEMN
mutations, which matter as much as the rest: a guard that reports every build
broken passes a suite that only checks that broken builds get caught, and it
would make the dogfood tool useless while looking strictly safer.
"""
import shutil, subprocess, sys, tempfile
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent

MUTATIONS = [
    # --- B127: the transport reply is not a report on the build -----------
    ("B127: a clean transport reply certifies the build again",
     "    elif raw_errs or load_error or not build_loaded:",
     "    elif load_error:"),
    ("B127: verdict() counts diagnostics before checking the build happened, "
     "so an empty list reads as PARITY-CLEAN",
     '    if status in ("NOT-RUN", "BUILD-ERR"):\n'
     '        return f"lsp={lspn} game=NO-BUILD({status})"',
     '    if False:\n'
     '        return f"lsp={lspn} game=NO-BUILD({status})"'),
    ("B127 over-condemn: every build reported BUILD-ERR",
     "    elif raw_errs or load_error or not build_loaded:\n",
     "    elif True:\n"),
    ("B127 over-condemn: verdict() calls every build a non-build, which would "
     "silence the dogfood tool entirely while looking safe",
     '    if status in ("NOT-RUN", "BUILD-ERR"):',
     '    if status != "ZZZ":'),
    ("B127: a pre-B127 record passes as verified",
     '    unverified = "" if status else " build=?"',
     '    unverified = ""'),

    # --- B118: the three false-green channels, now in build_verdict --------
    ("B118 ch1: build_ran no longer gates the verdict",
     '    if not build_ran:\n        status = "NOT-RUN"',
     '    if False:\n        status = "NOT-RUN"'),
    ("B118 ch2: location-less errors stop reaching a bucket",
     "    raw_errs = raw_build_errors(window, fid)",
     "    raw_errs = []"),
    ("B118 ch2: BUILD-ERR collapsed into REJECTED",
     '        status = "BUILD-ERR"\n    else:\n        status = "CLEAN"',
     '        status = "REJECTED"\n    else:\n        status = "CLEAN"'),
    ("B118 ch3: `Script compilation failed!` no longer counted",
     "    build_failed = any(BUILD_FAILED in l for l in slice_)",
     "    build_failed = False"),
    ("B118 ch3: build_slice stops distinguishing 'never started'",
     "    if start is None:\n        return []",
     "    if start is None:\n        start = 0"),
    # --- B139: the run-level gate ------------------------------------------
    ("B139: the no-controls case stops being refused, so a --only run that "
     "filtered out every control exits 0 having verified no channel",
     "    if not controls:",
     "    if False:"),
    ("B139: an all-ambiguous run stops being refused, so a transport that "
     "died midway exports a table of AMBIGUOUS as the measured answer",
     "    if measured and all(r[\"verdict\"] == ambiguous for r in measured):",
     "    if False:"),
    ("B139: control mismatches stop being reported",
     '        if r["verdict"] != r["expect"]:',
     "        if False:"),
    ("B139: an empty run is no longer refused",
     '        return ["the run produced no rows at all, so nothing was measured"]',
     "        return []"),
    ("B139 OVER-CONDEMN: every run refused, which would fail the honest "
     "partial results this gate exists to preserve",
     "    problems: list[str] = []\n    if not rows:",
     '    problems: list[str] = ["always"]\n    if not rows:'),
    ("B139 OVER-CONDEMN: any ambiguous row at all is fatal, which would have "
     "failed B096 claim 2's two legitimately ambiguous rows",
     "    if measured and all(r[\"verdict\"] == ambiguous for r in measured):",
     "    if measured and any(r[\"verdict\"] == ambiguous for r in measured):"),

    # --- B141: the variant-table checker -----------------------------------
    ("B141: opposite-polarity controls stop being required, so a probe whose "
     "controls cannot disagree passes (B086's original defect)",
     "    if len(controls) < 2:",
     "    if False:"),
    ("B141: duplicate variant ids stop being caught, so one measurement "
     "silently replaces another in the corpus",
     "    if dupes:",
     "    if False:"),
    ("B141: rows no longer have to say what shipped code they stand in for",
     "    if unnamed:",
     "    if False:"),
    ("B141 OVER-CONDEMN: every variant table rejected",
     "    problems: list[str] = []\n    if not variants:",
     '    problems: list[str] = ["always"]\n    if not variants:'),

    # --- B128: the filtered reader must stay test-only --------------------
    # The guard here is a static scan, so the mutation reintroduces the thing
    # it guards against -- a real production caller -- rather than neutering
    # the scan, which would only prove the suite notices its own absence.
    ("B128: a production caller of the severity-filtered reader reappears, "
     "which is how B118's three false-green channels come back",
     "probe_enum_corpus.py",
     "        window = fleet.read_log_window_raw(mark, PROBE_ID)",
     "        window = fleet.read_log_window(mark, PROBE_ID)"),
    ("B128 OVER-CONDEMN: the scan matches everything, so it would flag "
     "docstrings and stubs and become an alarm nobody can satisfy",
     '    needle = "fleet.read_log_window" + "("',
     '    needle = "e"'),

    # The control that proves the harness is wired at all.
    ("control: own/foreign error attribution stops discriminating",
     '    game_errors = [d for d in errs if d["file"].startswith(mine)]',
     "    game_errors = [d for d in errs]"),

    # --- B145: the CLI's second summary shape ------------------------------
    ("B145: B090's `0 errors/warnings` shape stops parsing, so every clean "
     "run with folded infos becomes an unknown the caller must guess about",
     '    if re.search(r"\\b0 errors/warnings\\b", output):\n        # Folded infos are deliberately NOT counted: they are hidden by\n        # default (B090), so counting them would make a clean run look dirty\n        # to every consumer that only sees this number.\n        return 0',
     "    if False:\n        return 0"),
    ("B145 OVER-CONDEMN: unrecognised output parses as ZERO, so a crashed CLI "
     "reads as a clean run",
     "        return 0\n    return None\n\n# ---- staging",
     "        return 0\n    return 0\n\n# ---- staging"),

    # --- B147: a comparison must know its two inputs were different --------
    # The measured incident: a before/after fleet sweep reported IDENTICAL
    # while both halves ran the same stale binary, because the sync between
    # them silently did not take effect and the ledger recorded nothing about
    # which artefact produced it.
    ("B147: the same-binary check is removed, so a comparison whose two "
     "inputs never differed reports IDENTICAL (the recorded incident)",
     '    if a["sha256"] == b["sha256"]:',
     "    if False:"),
    ("B147 OVER-CONDEMN: every comparison is refused, including a genuine "
     "before/after, which makes the idiom unusable",
     '    if a["sha256"] == b["sha256"]:',
     "    if True:"),
    ("B147: an unknown binary identity is accepted, so an old ledger with no "
     "record silently passes as 'they differ'",
     "    if not isinstance(identity, dict):",
     "    if False:"),
    ("B147 OVER-CONDEMN: every ledger is treated as recording no binary",
     "    if not isinstance(identity, dict):",
     "    if True:"),
    ("B147: only the FIRST ledger is checked for a binary identity",
     "    for label, side in ((label_a, a), (label_b, b)):",
     "    for label, side in ((label_a, a),):"),
    ("B147: the same-binary refusal stops naming the shared hash",
     '            f"(sha256={a[\'sha256\']}).\\n"',
     '            f".\\n"'),
    ("B147: lsp_bin_identity fabricates a hash for an absent binary, so 'I "
     "could not look' becomes 'I looked and they matched'",
     "    digest = file_sha256(path)",
     '    digest = file_sha256(path) or "0" * 64'),
    ("B147: file_sha256 returns a constant, so every pair of sweeps looks "
     "like it ran the same binary",
     "    return h.hexdigest()\n\n\ndef fd_sha256",
     '    return "c" * 64\n\n\ndef fd_sha256'),
    ("B147: file_sha256 hashes only a prefix — two builds of one binary "
     "differ late far more often than early",
     '        for chunk in iter(lambda: fh.read(1 << 20), b""):\n            h.update(chunk)',
     "        h.update(fh.read(4))"),
    ("B147: save_state stops stamping the ledger with the binary identity, so "
     "no comparison can ever know what produced it",
     '    st["lsp_bin"] = lsp_bin_identity()',
     "    pass"),
    ("B147: save_state stamps a CONSTANT identity, so every ledger claims the "
     "same binary and every comparison is refused",
     '    st["lsp_bin"] = lsp_bin_identity()',
     '    st["lsp_bin"] = {"path": "x", "sha256": "d" * 64}'),

    # --- B164: different hashes do not prove the baseline commit -----------
    ("B164: malformed nonempty binary hashes are accepted",
     '    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):',
     "    if not digest:"),
    ("B164: a populated provenance_error is ignored",
     '    if "provenance_error" not in identity or identity.get("provenance_error") is not None:',
     "    if False:"),
    ("B164: only the baseline ledger needs trusted provenance",
     "        problems.extend(trusted_binary_problems(side, label))",
     "        if label == label_a:\n            problems.extend(trusted_binary_problems(side, label))"),
    ("B164: the claimed baseline commit check is removed, so a stale lane "
     "baseline with different bytes is accepted",
     "    if expected_baseline_commit is not None:\n"
     "        problems.extend(baseline_commit_problems(\n"
     "            a, expected_baseline_commit, label_a\n"
     "        ))",
     "    if False:\n        problems.extend([])"),
    ("B164 OVER-CONDEMN: every baseline is rejected, including the clean "
     "authoritative commit positive control",
     "    if stamped != expected_commit:",
     "    if True:"),
    ("B164: a missing provenance sidecar is treated as trustworthy",
     '        identity["provenance_error"] = (\n'
     '            f"missing provenance sidecar {provenance_path}"\n'
     '        )\n        return identity',
     '        identity["commit"] = "c" * 40\n        return identity'),
    ("B164: a sidecar for different bytes is accepted",
     "    if stamped_hash != digest:",
     "    if False:"),
    ("B164: schema validation uses isinstance, so JSON true is accepted "
     "as integer schema 1",
     '    schema = provenance.get("schema")\n'
     "    if type(schema) is not int or schema != 1:",
     '    schema = provenance.get("schema")\n'
     "    if not isinstance(schema, int) or schema != 1:"),
    ("B164: source_synced uses truthiness, so integer 1 is accepted as the "
     "exact boolean attestation",
     "    if source_synced is not True:",
     "    if not source_synced:"),
    ("B164: dirty_count loses exact integer validation, so booleans, nulls, "
     "and strings can be stamped into a trusted identity",
     '    dirty = provenance.get("dirty_count")\n'
     "    if type(dirty) is not int or dirty < 0:",
     '    dirty = provenance.get("dirty_count")\n'
     "    if isinstance(dirty, int) and dirty < 0:"),
    ("B164: a dirty baseline is accepted as the exact commit it names",
     "    if type(dirty) is not int or dirty != 0:",
     "    if False:"),
    ("B164: the stale-baseline refusal stops naming the stamped commit",
     '            f"{label} was stamped with producing commit {stamped}, but the "',
     '            f"{label} was stamped with a different producing commit, but the "'),
    ("B164: save_state stops carrying the producing commit from the sidecar",
     '        "commit": commit,\n        "dirty_count": dirty,',
     '        "commit": None,\n        "dirty_count": dirty,'),
    ("B164: LSP invocation reuses its before identity as the after sample",
     '    result["binary_after"] = identity_fn()',
     '    result["binary_after"] = binary_before'),
    ("B164: LSP invocation executes a pathname that can be replaced after pinning",
     '            cmd = [f"/proc/self/fd/{binary_fd}", "check", "--typedb-dir",',
     '            cmd = [str(LSP_BIN), "check", "--typedb-dir",'),
    ("B164: LSP invocation accepts an opened inode outside its before identity",
     '        if executed_sha256 != binary_before.get("sha256"):',
     '        if False:'),
    ("B164: plugin records drop the hash of the inode actually executed",
     '        "executed_sha256": run["executed_sha256"],',
     '        "executed_sha256": None,'),
    ("B164: plugin records replace the before-invocation identity with after",
     '        "binary_before": run["binary_before"],',
     '        "binary_before": run["binary_after"],'),
    ("B164: plugin records replace the after-invocation identity with before",
     '        "binary_after": run["binary_after"],',
     '        "binary_after": run["binary_before"],'),
    ("B164: comparison skips every per-plugin binary identity check",
     '    top = ledger.get("lsp_bin") if isinstance(ledger, dict) else None',
     '    return []\n    top = ledger.get("lsp_bin") if isinstance(ledger, dict) else None'),
    ("B164: a binary change during one plugin invocation is accepted",
     "        if binary_identity_key(before) != binary_identity_key(after):",
     "        if False:"),
    ("B164: an earlier plugin may differ from the ledger final stamp",
     "        if binary_identity_key(before) != top_key:",
     "        if False:"),
    ("B164: comparison ignores a mismatch in the inode actually executed",
     '        if executed != before.get("sha256"):',
     '        if False:'),
]


def run(root: Path) -> int:
    return subprocess.run(
        [sys.executable, "scripts/fleet_dogfood.py", "--selftest"],
        cwd=root, capture_output=True, text=True, timeout=300,
    ).returncode


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="b127-mut-") as tmp:
        work = Path(tmp) / "wt"
        shutil.copytree(SRC / "scripts", work / "scripts")
        targets = {
            "fleet_dogfood.py": work / "scripts" / "fleet_dogfood.py",
            "probe_enum_corpus.py": work / "scripts" / "probe_enum_corpus.py",
        }
        pristines = {k: v.read_text() for k, v in targets.items()}

        if run(work) != 0:
            print("BASELINE FAILED — mutations prove nothing", file=sys.stderr)
            return 2
        print("baseline: PASS")

        caught = missed = 0
        for mut in MUTATIONS:
            # 4-tuples name a file; 3-tuples mean fleet_dogfood.py.
            label, old, new = mut[0], mut[-2], mut[-1]
            fname = mut[1] if len(mut) == 4 else "fleet_dogfood.py"
            target, pristine = targets[fname], pristines[fname]
            if old not in pristine:
                print(f"  ANCHOR MISSING: {label}", file=sys.stderr)
                missed += 1
                continue
            if pristine.count(old) != 1:
                print(f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}",
                      file=sys.stderr)
                missed += 1
                continue
            target.write_text(pristine.replace(old, new))
            rc = run(work)
            if rc != 0:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(f"  MISSED: {label}  <-- selftest is vacuous here",
                      file=sys.stderr)
                missed += 1
            target.write_text(pristine)

        print(f"\n{caught}/{len(MUTATIONS)} mutations caught, {missed} missed")
        return 0 if missed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
