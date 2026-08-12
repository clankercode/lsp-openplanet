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

# Full test suite (lib + integration + tui snaps)
test:
    cargo test --locked

# Fast unit tests only
test-lib:
    cargo test --lib

# TUI crate/module + snapshot tests
test-tui:
    cargo test --lib tui::
    cargo test --test tui_snapshots

# Pretty check on showcase fixture
check-showcase:
    cargo build --release
    FORCE_COLOR=1 ./target/release/{{bin}} check \
      --format pretty \
      --typedb-dir {{typedb}} \
      {{showcase}}

# Watch TUI on showcase fixture
watch-showcase:
    cargo build --release
    ./target/release/{{bin}} check --watch \
      --typedb-dir {{typedb}} \
      {{showcase}}

# Export canned mock TUI frames + PNGs under docs/images/tui-review/
tui-frames:
    cargo test --test tui_export_frames -- --nocapture
    for f in docs/images/tui-review/*.json; do python3 scripts/tui_frame_to_png.py "$f" -o "${f%.json}.png"; done

# Real showcase-diags TUI shots → docs/images/watch-demo.png (relaxed hero)
tui-showcase-shots:
    cargo test --test tui_export_showcase -- --nocapture
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
