# openplanet-lsp — common local recipes
# https://github.com/casey/just

set shell := ["bash", "-euo", "pipefail", "-c"]

bin := "openplanet-lsp"
install_dir := env("HOME") / ".local/bin"
typedb := "tests/fixtures/typedb"
showcase := "tests/fixtures/showcase-diags"

# Default: list recipes
default:
    @just --list

# Release build
build:
    cargo build --release

# Debug build
build-dev:
    cargo build

# Build release binary and install to ~/.local/bin
install: build
    mkdir -p "{{install_dir}}"
    install -m 755 "target/release/{{bin}}" "{{install_dir}}/{{bin}}"
    @echo "installed {{install_dir}}/{{bin}}"
    @"{{install_dir}}/{{bin}}" --version

# Full test suite (lib + integration + tui snaps).
#
# B078: wraps `cargo test --locked` and reconciles the result before printing a
# total — cargo's exit code is checked, and every test binary must have reported
# a `test result:` line, against a SEPARATE enumeration. A binary killed
# mid-run emits no summary and cargo then stops, so both the crashed binary and
# every later one vanish from the count without a single failed test.
#
# B085: runs `just test-inventory` first, so a test that vanished inside a
# healthy binary, or a stale binary a test spawns as a child, stops the run
# instead of quietly shrinking the total. And when a binary DOES die, the
# verdict says how — signal or exit status, how many of its tests it managed
# to print, and the machine's load at the time — because "INCOMPLETE RUN"
# alone cannot tell a real crash from a process starved under load.
#
# Throttled to `-j2 -- --test-threads=2` by default: `-j2` caps cargo's build
# jobs but NOT libtest, which otherwise takes one thread per CPU. Pass your own
# `-j` or `--test-threads` to override.
#
# Extra args are forwarded: `just test -- --test-threads=4`.
test *ARGS:
    python3 scripts/test-suite.py {{ARGS}}

# Proves the gate above can actually fail: runs the #[ignore]d canary in
# tests/zz_crash_canary.rs, which aborts, and checks the gate rejects the run
# because no summary was produced — not merely because cargo exited non-zero.
test-selftest:
    python3 scripts/test-suite.py --selftest

# The inventory non-vacuity harness, on the remote. It runs WHOLLY over there
# and is never split across machines: its whole subject is the mtime
# relationship between sources and the artifacts built from them on ONE
# filesystem. Sync a tree and you rewrite that relationship, so a split run
# would measure rsync rather than the tree.
#
# Proves `just test-inventory` can fail (real artifacts, on the remote).
rtest-inventory-mutants:
    python3 scripts/remote-build.py run -- python3 scripts/check_test_inventory_nonvacuous.py

# Prove the B145 issue-repro corpus gate can fail. That corpus is engine ground
# truth nothing in CI can re-derive — the game is not available there — so a
# vacuous gate would quietly turn the artifact back into decoration, which is
# the state B145 was raised to end. Runs on the remote: one sync, N cargo runs.
b145-corpus-mutants:
    python3 scripts/remote-build.py run -- python3 scripts/check_b145_corpus_nonvacuous.py

# Re-measure every issue-repro fixture against the LIVE game and rewrite the
# committed corpus. Needs Trackmania + Openplanet with RemoteBuild; takes ~10
# minutes and stages each fixture in turn. Hand-written `divergence` notes are
# carried across. Leaves a dirty tree on purpose — a human reviews and commits.
b145-regen:
    python3 scripts/issue_repro_game.py --all --tsv docs/probes/b145-issue-repro-verdicts.tsv

# The inventory check alone, on the remote.
rtest-inventory *ARGS:
    python3 scripts/remote-build.py run -- python3 scripts/test_inventory.py {{ARGS}}

# B085: the inventory check on its own, without running the suite. Per test
# binary, `--list` count vs the `#[test]` count in the sources cargo's dep-info
# says it was built from; plus every artifact's mtime against those sources,
# INCLUDING the CARGO_BIN_EXE_* binary tests spawn as a child (not a test
# binary, so `just test`'s binary-level reconciliation cannot see it). Runs as
# the first phase of `just test`; this is for running it alone (`-v` lists
# every binary's counts).
#
# B085: the per-binary content + freshness check `just test` cannot do.
test-inventory *ARGS:
    python3 scripts/test_inventory.py {{ARGS}}

# Proves the check above can fail: injects every defect it claims to catch —
# a vanished test, a stale spawned binary, a dead instrument — including both
# ways it could pass by vacuity.
#
# Proves `just test-inventory` can fail (synthetic, fast).
test-inventory-selftest:
    python3 scripts/test_inventory.py --selftest

# B163: the even swap a suite total cannot see, both ways a test gets
# disabled, and the three population zeros each drive a case here.
#
# Proves the test-SET diff can fail (offline, no cargo).
test-set-diff-selftest:
    python3 scripts/test_set_diff.py --selftest

# Proves the selftest above is non-vacuous by removing each production guard
# in a throwaway copy, including the independent `--list --ignored` path.
test-set-diff-mutants:
    python3 scripts/check_test_set_diff_nonvacuous.py

# The same, end to end on the REAL tree and REAL artifacts: bumps a source
# past the binary built from it, appends a `#[test]` the binary cannot
# contain, and neuters each instrument in a throwaway copy. Restores
# everything, and verifies the restore.
#
# Proves `just test-inventory` can fail (real artifacts, slower).
test-inventory-mutants:
    python3 scripts/check_test_inventory_nonvacuous.py

# B045: same suite, but every test binary runs with HOME/USERPROFILE pointed at
# an empty dir — a test that depends on this machine's Openplanet install or
# user config goes red here instead of on someone else's machine. ADDITIONAL to
# `just test`, deliberately not a replacement: the normal run is faster and is
# what you iterate against. Limits are documented in scripts/test-hermetic.py.
test-hermetic:
    python3 scripts/test-hermetic.py

# Proves the gate above can actually fail: runs the #[ignore]d canaries in
# tests/hermeticity_canary.rs under the same isolated env and checks each lands
# on its documented side (caught_* must FAIL, uncaught_* must PASS).
test-hermetic-selftest:
    python3 scripts/test-hermetic.py --selftest

# Offline checks for the game-probe harness: proves it cannot record ACCEPTED
# on silence it has not earned (B068). Needs no game, unlike the probe itself.
probe-selftest:
    python3 scripts/probe_enum_corpus.py --selftest

# Offline checks that no exit path leaves a probe fixture installed in the
# game's Plugins dir (B077). A stranded fixture is an installed plugin nobody
# knows about, declaring the names the next probe asks about. No game needed.
repro-selftest:
    python3 scripts/issue_repro_game.py --selftest

# B086: does the engine bind a `[script] exports` stub to its body, and by
# what rule? Stages an exporter plus one consumer per (stub, body) shape and
# CALLS the import — an unmatched stub compiles fine and dies at runtime with
# `Unbound function called`. Needs the game; see docs/probes/README.md, which
# records why it is currently blocked.
#
# Game probe: how the engine binds an export stub to its body.
probe-export-binding *ARGS:
    python3 scripts/probe_export_binding.py {{ARGS}}

# Offline half: proves the probe's oracle can tell BOUND from UNBOUND from
# did-not-measure, and that the variant set discriminates. B086 exists because
# a probe ran whose two variants failed identically and measured nothing.
#
# Proves the export-binding probe's oracle can discriminate (no game needed).
probe-export-binding-selftest:
    python3 scripts/probe_export_binding.py --selftest

# Prove that selftest is non-vacuous (B154). It had none until B148's discovery
# check parsed scripts/ instead of trusting a hardcoded list, and the first run
# of this harness found FOUR vacuous guards: two reachable only through inputs
# that were AMBIGUOUS for another reason, and two production paths a selftest
# built entirely on injected windows can never see (B118).
probe-export-binding-mutants:
    python3 scripts/check_export_binding_nonvacuous.py

# B119/B121: does receiver constness travel through a method's return value?
# Needs the game. Offline half: `just probe-const-receiver-selftest`.
probe-const-receiver *ARGS:
    python3 scripts/probe_const_receiver.py {{ARGS}}

# B117: does the engine enforce private/protected member access?
probe-member-visibility *ARGS:
    python3 scripts/probe_member_visibility.py {{ARGS}}

# B130: is an ENGINE method's constness enforced through a const receiver?
probe-engine-const-receiver *ARGS:
    python3 scripts/probe_engine_const_receiver.py {{ARGS}}

# The offline halves of the three probes above: variant-table checks, no game
# needed. These existed only as direct `python3` invocations until B141, and a
# probe a lane cannot find is one whose logic gets hand-rolled a fourth time.
probe-variant-selftests:
    python3 scripts/probe_const_receiver.py --selftest
    python3 scripts/probe_member_visibility.py --selftest
    python3 scripts/probe_engine_const_receiver.py --selftest
    python3 scripts/probe_class_scopes.py --selftest
    python3 scripts/probe_enum_conversion.py --selftest
    python3 scripts/diff_probe_corpus.py --selftest

# B067/B148: is a CLASS name usable as a SCOPE in type position? Regenerates
# docs/probes/b067-class-position-scopes.tsv. Needs the game. The verdict is
# derived by MATCHING ENGINE PROSE, so a reworded message reports UNRECOGNISED
# rather than a flipped verdict — an instrument failure is not a finding.
probe-class-scopes *ARGS:
    python3 scripts/probe_class_scopes.py {{ARGS}}

# B071/B148: enum conversion and enum-member verdicts. Regenerates ONLY the
# engine columns of docs/probes/b071-enum-conversion.tsv — `lsp_today` is this
# repo's own frozen history and row 3 is a retained INVALID measurement, so
# the generator carries both forward and refuses to run without the committed
# file. Needs the game.
probe-enum-conversion *ARGS:
    python3 scripts/probe_enum_conversion.py {{ARGS}}

# Diff two probe corpora and say WHY each row differs: BEHAVIOUR-CHANGED (the
# engine decides differently), WORDING-CHANGED (same verdict, new prose), or
# INSTRUMENT-BROKEN (the probe could not classify what it saw). The last is
# checked FIRST, because an unclassifiable row also has a changed verdict and
# would otherwise be reported as the alarming reading of a cosmetic rewording.
diff-probe-corpus *ARGS:
    python3 scripts/diff_probe_corpus.py {{ARGS}}

# Prove those three selftests are non-vacuous: drop every control row, blank
# what a row stands in for, or restore the duplicated classifier, and each
# probe's selftest must go red. Includes an OVER-CONDEMN mutation.
probe-variant-mutants:
    python3 scripts/check_probe_variants_nonvacuous.py

# Prove remote-build's own guards are non-vacuous: remove the unresolved-merge
# refusal and confirm a conflicted tree reaches the remote, plus five
# OVER-CONDEMN mutations — gating uncommitted work is the normal lane workflow,
# so a guard that refuses a dirty tree, or attributes the main checkout's merge
# to every worktree, would stop all five lanes at once (B132).
remote-build-mutants:
    python3 scripts/check_remote_build_nonvacuous.py

# Offline checks for the two functions that decide whether a build succeeded
# (`build_verdict`) and whether a plugin shows parity (`verdict`). Neither had
# a test of its own, which is how `verdict` kept reporting PARITY-CLEAN for
# builds that never happened (B127). No game needed.
fleet-selftest:
    python3 scripts/fleet_dogfood.py --selftest

# Prove fleet-selftest is non-vacuous: restore each half of B118's, B123's and
# B127's defects, including two OVER-CONDEMN mutations — a guard that calls
# every build broken passes a suite that only checks broken builds get caught.
fleet-verdict-mutants:
    python3 scripts/check_fleet_verdict_nonvacuous.py

# Gate a before/after fleet comparison on the two sweeps having actually run
# DIFFERENT binaries (B147), and on BEFORE having been built from the explicit
# authoritative baseline commit (B164). Resolve BASELINE_COMMIT from the
# baseline checkout/ref, never from whichever lane directory is convenient.
# The measured incidents: a comparison reported
# IDENTICAL while both halves ran the same stale artefact, because the
# sync-back between them silently did not take effect; later, a different but
# stale lane artefact was almost accepted as current master. Exits 3 rather than
# printing a diff when the two ledgers share a binary hash, or when either
# does not record trustworthy provenance — "I could not look" is not "I looked
# and it came from the claimed commit".
#
# Produce the two ledgers with FLEET_STATE_FILE=... on each sweep.
fleet-compare BASELINE_COMMIT BEFORE AFTER:
    python3 scripts/fleet_dogfood.py compare --baseline-commit {{BASELINE_COMMIT}} {{BEFORE}} {{AFTER}}

# Prove probe-selftest is non-vacuous: restore each half of a defect the enum
# probe's guards exist to prevent (B068's sentinel, B097's load check, B123's
# build-health check) and require the selftest to go red for every one. B118
# found 4 of 9 new guards vacuous this way after all their pair-tests passed,
# so a passing pair is not evidence that a guard ran.
enum-probe-mutants:
    python3 scripts/check_enum_probe_nonvacuous.py

# Prove repro-selftest is non-vacuous: neuter each cleanup guard in a
# throwaway copy and require the selftest to go red for every one. A green
# suite with the protection removed would be worse than no suite (B077).
probe-cleanup-mutants:
    python3 scripts/check_probe_cleanup_nonvacuous.py

# Fast unit tests only
test-lib:
    cargo test --lib

# TUI module + watch adapter + snapshot tests
test-tui:
    cargo test --lib tui::
    cargo test --lib cli::watch::
    cargo test --test tui_snapshots

# Pretty check on showcase fixture
check-showcase:
    cargo build --release
    FORCE_COLOR=1 ./target/release/{{bin}} check \
      --format pretty \
      --typedb-dir {{typedb}} \
      {{showcase}}

# GH #18: multi-version check of the version-straddle fixture against the
# recorded snapshots (1.29.5 + 1.29.14) — the Text::Join miss should come
# back labeled `[op 1.29.5]` with an added-in note.
check-versions:
    cargo build
    ./target/debug/{{bin}} check \
      --typedb-dir tests/fixtures/typedb-versions/1.29.5 \
      --typedb-versions-dir tests/fixtures/typedb-versions \
      --openplanet-version 1.29.5,1.29.14 \
      tests/fixtures/issue-repros/gh18-version-straddle || true

# Show resolved type-DB paths + versions (auto-detect, or pass --from DIR)
typedb-status:
    cargo build
    ./target/debug/{{bin}} typedb status

# Parity diff: repo fixture DBs vs the auto-detected local install
typedb-diff:
    cargo build
    ./target/debug/{{bin}} typedb diff {{typedb}}

# Refresh repo fixture DBs from the local install — DRY RUN (shows the diff only)
typedb-refresh:
    cargo build
    ./target/debug/{{bin}} typedb refresh --into {{typedb}}

# Same, but actually write the files (review `just typedb-refresh` first)
typedb-refresh-apply:
    cargo build
    ./target/debug/{{bin}} typedb refresh --into {{typedb}} --yes

# Anti-drift guard for the shipped fixture DBs (also runs inside `just test`)
typedb-guard:
    cargo test --locked --lib typedb_shape_guard

# B134: how much deprecation information is really in Core enum-value doc
# strings? Measured and REJECTED — the prose is attached to a VALUE, and a
# deprecated flag and its replacement are aliases sharing one value, so both
# names carry the identical string and a matcher would warn on the symbol you
# are supposed to adopt. Kept so a future dump can re-run it, not re-derive it.
enum-doc-migrations:
    python3 scripts/measure_enum_doc_migrations.py

# Proves the categoriser can tell correct / wrong-polarity / not-a-migration
# apart, and can say each. Includes the substring trap: `use Borders` contains
# `use Border`, which scored the deprecated member as its own replacement.
enum-doc-migrations-selftest:
    python3 scripts/measure_enum_doc_migrations.py --selftest

# B129: every engine-derived gold artifact in one report. AUDIT ONLY — needs no
# game, changes nothing, and reports each artifact's status against
# docs/probes/MANIFEST.tsv. Nothing here ever auto-commits.
#
# The typedb fixtures are MANIFESTED, not re-scripted: `just typedb-refresh` /
# `typedb-refresh-apply` / `typedb-guard` above already are the pattern this
# ticket asked for, so the manifest records that edge rather than wrapping it
# in a second command that could drift from it.
regen-golds *ARGS:
    python3 scripts/regen_golds.py {{ARGS}}

# The same runner against the LIVE GAME: reruns every generator, rewrites the
# artifacts, prints the drift. Leaves a dirty tree on purpose — a human reviews
# and commits. Needs Trackmania + Openplanet with RemoteBuild on port 30000.
regen-golds-apply:
    python3 scripts/regen_golds.py --regen

# Offline checks of the runner itself: NOT-RUN is never CLEAN, an unstamped or
# version-moved artifact refuses to diff, orphans and PRIMARY rows keep their
# own reasons, and a corpus missing from the manifest is reported. No game.
regen-golds-selftest:
    python3 scripts/regen_golds.py --selftest

# Prove those guards are non-vacuous: neuter each one in a throwaway copy and
# require the selftest to go red FOR THAT GUARD. Includes two OVER-CONDEMN
# mutations — regen-golds is unusually exposed to them, because its normal
# audit output is entirely UNCHECKED and so a report with no failures in it
# looks exactly like a report in which nothing was measured.
regen-golds-mutants:
    python3 scripts/check_regen_golds_nonvacuous.py

# Watch TUI on showcase fixture
watch-showcase:
    cargo build --release
    ./target/release/{{bin}} check --watch \
      --typedb-dir {{typedb}} \
      {{showcase}}

# Export canned mock TUI frames + PNGs under docs/images/tui-review/
tui-frames:
    cargo test --test tui_export_frames -- --ignored --nocapture
    for f in docs/images/tui-review/*.json; do python3 scripts/tui_frame_to_png.py "$f" -o "${f%.json}.png"; done

# Real showcase-diags TUI shots → docs/images/watch-demo.png (relaxed hero)
tui-showcase-shots:
    cargo test --test tui_export_showcase -- --ignored --nocapture
    for f in docs/images/tui-review/showcase-*.json; do python3 scripts/tui_frame_to_png.py "$f" -o "${f%.json}.png"; python3 scripts/pad_screenshot.py "${f%.json}.png" --in-place --pad 16; done
    cp -f docs/images/tui-review/showcase-relaxed-hero.png docs/images/watch-demo.png
    @echo "hero → docs/images/watch-demo.png"

# Clippy (all targets).
#
# RED ON MASTER (B083): 65 errors that belong to nobody, so this cannot tell
# you whether YOUR change is clean. Use `just clippy-delta` for that. Note the
# `-D warnings` here also TRUNCATES the diagnostic set — it stops compilation
# at the first warning, reporting 115 diagnostics where the plain run reports
# 129 — so a green run of this would still be hiding 14.
#
# Clippy, all targets, -D warnings. RED ON MASTER — see clippy-delta.
clippy:
    cargo clippy --all-targets -- -D warnings

# B083 on the remote host — THE DEFAULT PATH.
#
# PINNED TO x-tuf, and for a reason that is not about capacity. The clippy gate
# refuses to compare across toolchains, so the baseline and every run of it must
# share one. x-tuf is rustc 1.97.0, BYTE-IDENTICAL to this desktop's, so a
# baseline recorded there is comparable BOTH remotely and locally. x-alpha is
# 1.97.1 and a baseline from it is comparable to neither — which is exactly the
# refusal a lane hit and correctly did not work around.
#
# The suite gate has no such constraint and defaults to x-alpha (48 cores).
# Override here with LSP_REMOTE_HOST if you re-record somewhere else.
#
# The clippy gate, on the remote build host. Use this one.
rclippy-delta *ARGS:
    LSP_REMOTE_HOST="${LSP_REMOTE_HOST:-x-tuf}" python3 scripts/remote-build.py run -- python3 scripts/clippy_delta.py {{ARGS}}

# Re-record docs/clippy-baseline.json from the remote. Run ON MASTER. Writes
# into the remote CARGO_TARGET_DIR (`@target` is resolved over there, since a
# shell-quoted `$CARGO_TARGET_DIR` would arrive as a literal), fetches it back,
# and only then replaces the committed file — so a failed remote run cannot
# leave a half-written baseline behind.
#
# NOTE the flag order: remote-build.py's `rest` is an argparse.REMAINDER, so an
# option placed AFTER the action word is swallowed and handed to the remote
# shell verbatim (`--sync-back: command not found`). Options go before `run`.
#
# Re-record the clippy baseline from the remote (master only).
rclippy-delta-record:
    LSP_REMOTE_HOST="${LSP_REMOTE_HOST:-x-tuf}" python3 scripts/remote-build.py --sync-back clippy-baseline.json run -- python3 scripts/clippy_delta.py --record --baseline @target --commit "$(git rev-parse HEAD)"
    cp target/clippy-baseline.json docs/clippy-baseline.json
    @echo "docs/clippy-baseline.json updated from x-tuf (rustc 1.97.0, same as local)"

# B163: re-record the committed test baseline from the remote.
#
# Same shape as `rclippy-delta-record` above, and the same flag-order note
# applies: options go BEFORE `run`. The tool writes into the remote
# CARGO_TARGET_DIR, `--sync-back` fetches it, and only then does the committed
# file get replaced — so a failed remote run cannot leave a half-written
# baseline, which for THIS file would mean a short list that reports zero
# removals forever.
#
# Re-recording does not conceal a removal, it RENDERS it: the deleted test
# becomes a deleted line in your diff, next to the change that caused it.
#
# Re-record the test-set baseline from the remote, after an intended change.
rtest-baseline-record:
    python3 scripts/remote-build.py --sync-back test-baseline.txt run -- python3 scripts/test_set_diff.py --record --baseline @target
    cp target/test-baseline.txt docs/test-baseline.txt
    @echo "docs/test-baseline.txt updated — review the diff before committing"

# The non-vacuity harness, on the remote. It mutates the tree, so it runs
# WHOLLY over there against one synced copy — never split across machines.
#
# Proves `just rclippy-delta` can fail (real lints, on the remote).
rclippy-delta-mutants:
    python3 scripts/remote-build.py run -- python3 scripts/check_clippy_delta_nonvacuous.py

# B083: did MY change add a clippy finding? A multiset difference of
# (file, lint, message) fingerprints against docs/clippy-baseline.json, with
# line numbers normalised out and NEVER `-D warnings`. Answers the question a
# lane actually has, on a tree that is red for everyone.
#
# LOCAL run. This now WORKS: the committed baseline is recorded on x-tuf, whose
# rustc/cargo/clippy strings are byte-identical to this desktop's, so the method
# check passes and a real comparison happens. It used to refuse, correctly,
# against an x-alpha baseline at 1.97.1 — fixed by recording on a matching
# toolchain rather than by loosening the check or passing
# --allow-toolchain-drift. Expect a full local clippy build; `just rclippy-delta`
# is cheaper.
#
# The clippy gate, locally — comparable against the x-tuf baseline.
clippy-delta *ARGS:
    python3 scripts/clippy_delta.py {{ARGS}}

# Refresh docs/clippy-baseline.json. Run this ON MASTER: a baseline recorded on
# a lane branch blesses that lane's own lints. Records the method (command,
# toolchain, features, profile, commit) beside the numbers, because numbers
# without their method are not comparable to anything.
#
# Re-record the clippy baseline (master only).
clippy-delta-record:
    python3 scripts/clippy_delta.py --record

# Offline proof the gate can fail: a new lint, one more of an existing lint,
# and a swapped member at an unchanged total must all go red, and a baseline
# it cannot legitimately compare against must be refused.
#
# Proves `just clippy-delta` can fail (synthetic, fast).
clippy-delta-selftest:
    python3 scripts/clippy_delta.py --selftest

# The same, end to end: writes Rust that clippy really lints into the real
# tree, requires the real gate to name it, restores, and verifies the restore.
# Also neuters each guard in a throwaway copy.
#
# Proves `just clippy-delta` can fail (real lints, slower).
clippy-delta-mutants:
    python3 scripts/check_clippy_delta_nonvacuous.py

# Format check
fmt:
    cargo fmt --all -- --check

# Format fix
fmt-fix:
    cargo fmt --all

# Clean build artifacts
clean:
    cargo clean

# ---------------------------------------------------------------------------
# Remote builds (scripts/remote-build.py)
#
# Offload compilation and tests to a build box so this machine stays
# responsive. Defaults to x-alpha (48 cores) — override with $LSP_REMOTE_HOST
# or --host. Works from a lane worktree as well as master; each worktree gets
# its own remote tree and target dir, so lanes never share a build cache.
#
# Run `just rdoctor` first on a new machine.
# ---------------------------------------------------------------------------

# Verify the remote host is usable (ssh, rsync, flock, realpath, cargo,
# python3, just, versions)
rdoctor:
    python3 scripts/remote-build.py doctor

# Proves the remote wrapper's own guards can fail: the $HOME spelling rule that
# rsync and ssh disagree about, the B106 free-space floor, df parsing, flag
# order, and per-worktree slug isolation. Pure — no ssh, no remote, instant.
#
# Proves `remote-build.py`'s pre-flight guards can fail (offline).
remote-selftest:
    python3 scripts/remote-build.py selftest

# Full gated suite on the remote — the same `just test` gate, run over there
rtest *ARGS:
    python3 scripts/remote-build.py test -- {{ARGS}}

# cargo check --all-targets on the remote
rcheck *ARGS:
    python3 scripts/remote-build.py check -- {{ARGS}}

# cargo clippy --all-targets on the remote
rclippy *ARGS:
    python3 scripts/remote-build.py clippy -- {{ARGS}}

# cargo build on the remote; add --sync-back debug/openplanet-lsp to fetch it.
# Every successful fetch writes an adjacent `.provenance.json` sidecar with the
# synced checkout's full commit, branch, dirty count and artefact hash (B164).
# Commit, exact status, and rsync-eligible content are verified unchanged
# across rsync.
# `--no-sync` provenance is deliberately unknown: reused remote state cannot be
# attributed to the current local checkout.
# For a before/after comparison add --sync-back-must-change: it refuses (exit
# 4) when the fetched artefact is byte-identical to the one already there,
# which is the fetch-that-never-happened this batch measured (B147). The
# sha256 of every fetch is printed whether or not the flag is passed.
rbuild *ARGS:
    python3 scripts/remote-build.py build -- {{ARGS}}

# Arbitrary command in the synced remote tree, e.g. `just rrun cargo test --lib`
rrun *ARGS:
    python3 scripts/remote-build.py run -- {{ARGS}}

# Interactive shell in the remote tree, for poking at a failure
rshell:
    python3 scripts/remote-build.py shell

# Remove this worktree's remote tree and build cache under its shared host lock.
rclean:
    python3 scripts/remote-build.py clean

# Map substantial remote trees to git's currently registered local worktrees.
# NO-LOCAL-MATCH is a review queue, not proof of staleness; confirm ownership
# with the coordinator before reclaiming anything (B167).
rinventory:
    python3 scripts/remote-build.py inventory

# Reclaim after a local worktree has already been removed. Both the exact former
# absolute path and its matching remote slug are required. This recipe is a
# dry-run; it prints the resolved remote target and removes nothing.
rreclaim FORMER_ROOT REMOTE_SLUG:
    python3 scripts/remote-build.py --reclaim-root "{{FORMER_ROOT}}" --remote-slug "{{REMOTE_SLUG}}" reclaim

# Execute the same validated reclaim after coordinator confirmation. The tool
# refuses an extant/registered local worktree, an unsafe remote path, and any
# remote process whose cwd/fd/maps/cmdline still points beneath the target, or
# any incomplete process scan. After the first scan, it atomically quarantines
# the target name and rescans before FD-relative deletion. The whole operation
# holds the same remote flock as ordinary builds.
rreclaim-now FORMER_ROOT REMOTE_SLUG:
    python3 scripts/remote-build.py --reclaim-root "{{FORMER_ROOT}}" --remote-slug "{{REMOTE_SLUG}}" --execute-reclaim reclaim
