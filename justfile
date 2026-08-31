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
# instead of quietly shrinking the total.
#
# Extra args are forwarded: `just test -- --test-threads=2`.
test *ARGS:
    python3 scripts/test-suite.py {{ARGS}}

# Proves the gate above can actually fail: runs the #[ignore]d canary in
# tests/zz_crash_canary.rs, which aborts, and checks the gate rejects the run
# because no summary was produced — not merely because cargo exited non-zero.
test-selftest:
    python3 scripts/test-suite.py --selftest

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

# B083: did MY change add a clippy finding? A multiset difference of
# (file, lint, message) fingerprints against docs/clippy-baseline.json, with
# line numbers normalised out and NEVER `-D warnings`. Answers the question a
# lane actually has, on a tree that is red for everyone.
#
# The clippy gate that works on a red tree: did I add a lint?
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
