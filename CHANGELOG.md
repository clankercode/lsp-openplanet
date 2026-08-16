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

## [0.4.1] - 2026-08-16

### Fixed
- **#34 follow-up (review "Arm B"):** the inherited-overload augmentation in
  0.4.0 fixed a false positive but introduced a false negative — once the
  overload set held 2+ entries (own + inherited), a call whose arg count was
  outside **every** overload's range went silent (the pre-0.4.0 single-overload
  path had caught it). Now the multi-overload no-match arm diagnoses when the
  count exceeds/undershoots every overload's arity, while type-driven no-match
  and genuinely-ambiguous calls stay silent (the conservative anti-FP policy).

## [0.4.0] - 2026-08-16

A large compiler-parity and dependency-resolution batch driven by a full
plugin-fleet dogfood sweep (129 real plugins compared LSP-vs-game). Most fixes
kill false positives the LSP reported on code the game compiles clean.

### Added
- **`.op` archive dependency exports (#20):** load dependency export scripts
  directly from installed `.op` (ZIP) archives in memory, and walk
  `export_dependencies` transitively. Cross-file shared-function imports from
  `.op` deps now resolve (kills the `undefined identifier 'tabs'` class of FP).
- **`game_target`-derived preprocessor defines:** `default_defines()` no longer
  defines every ManiaPlanet platform at once. Defines now come from the game
  target (default `TMNEXT`), overridable via config file / init options /
  repo-local `.openplanet-lsp.toml`, so the LSP only compiles the `#if` branches
  the real game would (#36).
- **`source_paths` / `ignore_paths` config:** new `.openplanet-lsp.toml` keys to
  restrict which `.as` files are checked when a repo keeps non-compiled scripts
  (asset packs, fixtures, experiments) alongside the real sources. `source_paths`
  is an allowlist (wins if set), `ignore_paths` a blocklist; with neither,
  everything under the plugin root is checked (unchanged default).
- **Parser diagnostics:** error on `shared` applied to non-class items, and on
  unary `!` applied to a non-bool operand.

### Fixed
- **Dependency id resolution (#33):** resolve a dependency whose id is the
  provider's normalized display name (e.g. `BetterRoomManager` ← "Better Room
  Manager") when dir-name / `.op` / module-name all miss. Fixes `dependency not
  found` + cascading unknown-type FPs (tm-bosslike, tm-simple-room-admin).
- **Inherited method overloads (#34):** a `Ns::Class::Method(...)` call now
  counts overloads inherited from the workspace class's parent chain, so a call
  matching a parent-declared overload is no longer flagged for arity
  (tm-mlfeed-race-data).
- **Dead `#if` platform branches (#36):** TM2020 treats `#if MP4` / `#elif TURBO`
  as dead; the LSP no longer type-checks them (tm-skids-magician).
- **Typecheck FP silencing:** accept subclass args where a base param is expected
  (#22); silence `ArgTypeMismatch` on `auto`/workspace-local args (#23); silence
  spurious `MwAddRef`/`MwRelease` on `CMwNod`-derived types (#21); sibling class
  fields no longer silence undefined identifiers (#30); strip the `const`
  wrapper before the unary `!` bool check; allow `shared` on functions while
  keeping the var-decl rejection.
- **OP 1.29.5 parity (#29):** better-totd / dashboard typecheck FPs and
  catch/string handling.
- **Dependency-resolution visibility (#20 review):** surface silent
  dependency-resolution failures instead of swallowing them.

### Tooling
- **Plugin-fleet dogfood driver** (`scripts/fleet_dogfood.py`) + curated ledger
  (`docs/plugin-fleet-ledger.md`) for systematic LSP-vs-game parity sweeps.

## [0.3.4] - 2026-08-12

### Added
- **Watch TUI polish (product-ready):** pretty caret detail pane, compact/relaxed
  density (`c`), right-aligned `› fragment ‹` on relaxed rows, content-sized detail box.
- **Showcase screenshots:** real `tests/fixtures/showcase-diags` frames; README hero
  `docs/images/watch-demo.png` (relaxed + MakeTint detail).
- **Async watch checks:** background worker so `checking…` paints and the UI stays
  navigable during analysis; dirty edits coalesce to one follow-up run.
- **Stable selection** across refresh (path/range/message identity).
- **Watch health / stale:** last-good list labeled stale while checking or after
  failure; `watch off · r to refresh` when notify fails.
- **justfile** for local build/install/test/check/watch/TUI export recipes.
- README **Watch TUI** keys, watched files (`*.as`/`*.inc`/`info.toml`), exit codes.

### Changed
- Status wording: `checked in N ms` (was `last: Nms`).
- Brighter error/warning palette for dark terminals; `▸` selection marker.
- Message ellipsis; location column width capped to ~40% of list width.
- Page size follows viewport × density; Ctrl-C quits; plain keys ignore Ctrl/Alt.
- Info/Hint counts appear in the header when non-zero.
- TUI export tests are `#[ignore]` — regenerate with `just tui-frames` /
  `just tui-showcase-shots` (normal `cargo test` no longer rewrites docs).

### Fixed
- Duplicate startup check (`first_poll` + `dirty`) — exactly one initial run.
- Detail pane blank line restored between path header and source gutter.
- Fragments right-align and only truncate when they would collide with the LHS.

## [0.3.3] - 2026-08-12

### Fixed
- **crates.io**: fold the watch TUI into the main `openplanet-lsp` package
  (`src/tui/`) so Trusted Publishing does not need a separate first-time crate
  create. `cargo publish` dry-run clean.

### Changed
- Removed workspace crate `openplanet-lsp-tui` (code lives under `src/tui/`).


## [0.3.2] - 2026-08-12

### Fixed
- Attempted crates.io packaging for the watch TUI as a path dependency (incomplete —
  Trusted Publishing cannot create a new crate name). Superseded by **0.3.3**.

### Notes
- GitHub Release + npm for **0.3.2** match 0.3.1 feature set; crates.io needs **0.3.3**.


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
