# Changelog

All notable user-facing changes to openplanet-lsp are documented here.

Format inspired by [Keep a Changelog](https://keepachangelog.com/).
Versions follow [SemVer](https://semver.org/).

<!--
Release agents: add a new ## [X.Y.Z] - YYYY-MM-DD section at the top when
cutting a release. See RELEASE.md. After release.yml succeeds, update the
GitHub Release body to match this section (gh release edit).
-->

## [Unreleased]

## [0.3.2] - 2026-08-12

### Fixed
- **crates.io publish**: path-only `openplanet-lsp-tui` blocked `cargo publish`.
  The TUI crate is now a publishable package; CI publishes **tui then main**.
  `openplanet-lsp` depends on `openplanet-lsp-tui` with an explicit version.

### Notes
- GitHub Release + npm already shipped **0.3.1** (pretty check, watch TUI,
  bare-TTY entrypoints, showcase fixture, README hero). 0.3.2 is the crates.io
  packaging fix for those same features.


## [0.3.1] - 2026-08-12

### Added
- **Pretty `check` output** (`--format plain|pretty|auto`): source excerpts with
  gutter line numbers and caret spans under the diagnostic range. Auto uses
  pretty when stdout is color-capable; plain stays gcc-style for pipes/CI.
  No outer box frame in the CLI (chrome lives in the watch TUI).
- **`check --watch`**: live diagnostics TUI (ratatui). Re-checks the plugin on
  `*.as` / `info.toml` changes (notify + debounce). Keys: `q` quit, `j`/`k`
  scroll, `PgUp`/`PgDn`, `g`/`G` top/end, `r` refresh. Detail pane for the
  selected diagnostic.
- **Bare TTY entrypoints**: with no subcommand, a TTY inside a plugin root
  (`info.toml`) starts the watch TUI; non-TTY (editors) still starts the LSP.
  Force the server with `openplanet-lsp --lsp` or `openplanet-lsp lsp`. No
  plugin nearby → short help (exit 2). Optional path for `check` defaults to `.`.
- **Config `default_mode`**: `tui` (default) or `lsp` in
  `~/.config/openplanet-lsp/config.toml` or workspace `.openplanet-lsp.toml`.
- **Showcase fixture** `tests/fixtures/showcase-diags/`: curated multi-file
  typecheck diagnostics for screenshots and CI (~11 diags).
- **Workspace crate** `crates/openplanet-lsp-tui`: mock `TuiDataSource`,
  TestBackend + insta snapshots.
- **README hero** `docs/images/check-demo.png` (even padding via
  `scripts/pad_screenshot.py`).
- **crates.io CI**: Trusted Publishing via `rust-lang/crates-io-auth-action@v1`
  in a dedicated `publish-crates` job (no bare OIDC).

### Changed
- Interactive CLI color via `src/term.rs` (`FORCE_COLOR` / `NO_COLOR` /
  TTY). Color decisions are not OnceLock-sticky across tests.
- CI-stable optional-dependency define test (missing Editor on runners).

### Notes
- First release that exercises crates.io Trusted Publishing end-to-end when
  the publisher is configured for `release.yml`.
- Full multi-pane IDE TUI remains out of scope (see GH #9).


## [0.3.0] - 2026-08-12

### Added
- **Standalone self-update** (shipped): non-npm/cargo installs download the matching GitHub Release archive and atomically replace the running binary.
- **`update --source npm|crate|github`**: choose which channel to query for the latest version (default: npm).
- Status UX: `current: X.Y.Z (install type: standalone)` and `latest: X.Y.Z (source checked: npm)`.
- **crates.io** packaging metadata, dual Unlicense OR CC0-1.0, and CI publish via Trusted Publishing (OIDC) on release tags.

### Changed
- Cargo self-update install no longer passes `--locked`.


## [0.2.9] - 2026-08-12

### Added
- **Standalone self-update**: installs outside npm/cargo (e.g. `~/.local/bin`) download the matching GitHub Release archive, extract the binary, and atomically replace the running path. Override archive with `OPENPLANET_LSP_RELEASE_ARCHIVE` for tests.
- **crates.io**: package metadata + dual license (Unlicense OR CC0-1.0); optional CI publish via `CARGO_REGISTRY_TOKEN`.

### Changed
- Cargo self-update install no longer passes `--locked` (uses latest compatible deps from the git install path).


## [0.2.8] - 2026-08-12

### Architecture (I7 → I1 → I2+I3)

- **Shared plugin workspace load** (`workspace::load`): CLI `check` and LSP Backend share one path for plugin sources + dependency exports; required/`export_dependencies` load export symbols; optional deps when present; missing required deps reported even without configured plugin dirs.
- **DocumentAnalysis** seam: single preprocess→lex→parse module consumed by diagnostics and key handlers.
- **CallSite + callables**: `typecheck::call_site` owns arg bind and unique-arity pick; `GlobalScope::callables_free` / `callables_method` shared by checker and signature help.
- Review hardening: consistent unresolved-dependency reporting; shared workspace symbol pipeline.

### Notes

- Builds on 0.2.7 self-update work; no release-pipeline change required.


## [0.2.7] - 2026-08-12

### Added
- Self-update CLI hardening and multi package-manager support:
  `openplanet-lsp update` / `update --check` / `update --status`
- Install-method detection for **npm / pnpm / yarn / bun** (global + local),
  plus cargo / development / standalone
- Status file: `~/.config/openplanet-lsp/update-status.json`
  (`XDG_CONFIG_HOME` / `%APPDATA%` / `OPENPLANET_LSP_CONFIG_DIR`)
- Dev/CI overrides: `OPENPLANET_LSP_VERSION`, `OPENPLANET_LSP_LATEST_VERSION`,
  `OPENPLANET_LSP_UPDATE_PACKAGE`, `OPENPLANET_LSP_PACKAGE_MANAGER`,
  `OPENPLANET_LSP_EXE`
- Smoke scripts: `smoke-self-update.sh` (local tarballs) and
  `smoke-self-update-registry.sh` (registry multi-PM loop)
- CI workflow **`self-update-matrix`** (manual `workflow_dispatch` + post-tag
  release); not run on every PR

### Fixed
- Post-apply status truthfulness (`pending_restart` / `installed_version`)
- LSP auto-check refreshes on running-binary version skew; notify only on
  fresh network checks
- Windows path classification (Program Files nodejs, `\` separators)
- Atomic status write; exit code `3` when update cannot be auto-applied
- Release workflow only publishes on real non-deleted tags

### Distribution
- Multi-platform GitHub Release + npm OIDC publish

## [0.2.6] - 2026-08-12
