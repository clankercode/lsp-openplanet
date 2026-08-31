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
# every later one vanish from the count without a single failed test. Extra args
# are forwarded: `just test -- --test-threads=2`.
test *ARGS:
    python3 scripts/test-suite.py {{ARGS}}

# Proves the gate above can actually fail: runs the #[ignore]d canary in
# tests/zz_crash_canary.rs, which aborts, and checks the gate rejects the run
# because no summary was produced — not merely because cargo exited non-zero.
test-selftest:
    python3 scripts/test-suite.py --selftest

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

# Clippy (all targets)
clippy:
    cargo clippy --all-targets -- -D warnings

# Format check
fmt:
    cargo fmt --all -- --check

# Format fix
fmt-fix:
    cargo fmt --all

# Clean build artifacts
clean:
    cargo clean
