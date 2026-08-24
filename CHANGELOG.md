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

### Added
- **`errors` config + `--errors last|first|off`** — human `check` output
  (plain and pretty) groups diagnostics so a flood of warnings does not bury
  errors. Default `last`: warnings/info/hints first, then errors. `first`
  puts errors first; `off` keeps analysis order. JSON is never regrouped.
  Workspace `.openplanet-lsp.toml` wins over the user config; the flag wins
  over both.
- **`unused` config + `--unused` / `--no-unused`** — unused functions, vars,
  parameters, and class members can be `ignore`, `warn` (default), or `error`.
  `unused = "ignore"` (or `false`) in `.openplanet-lsp.toml` / the user config
  turns the noisy pass off; `--unused error` makes them fail `check` without
  `--warnings-as-errors`. Workspace wins over user; the flag wins over both.
- **`openplanet-lsp config` TUI** — edit `~/.config/openplanet-lsp/config.toml`
  interactively. Every persisted setting is listed; changing a value writes
  the file immediately (comments and unknown keys are kept). `--path FILE`
  selects a different file (tests / alt locations).
- **Config files reload live** — editing `$HOME/.config/openplanet-lsp/config.toml`
  or a workspace `.openplanet-lsp.toml` retriggers `check --watch` and makes a
  running language server re-load the config ladder, rebuild snapshots, and
  republish diagnostics. Invalid TOML is ignored (last-good settings stay,
  with a warning) instead of crashing the session.
- **Type-DB freshness is a visible, managed pipeline** (GH #59) — every
  `check` run now states which Openplanet type database it judged the code
  against (`type db: op=1.29.5 (next, Public, 1234ad9e)  game op=…  mp=…`),
  the JSON report carries the same stamps as an additive `typedb` object,
  and the language server logs the line at init. New
  `openplanet-lsp typedb status|diff|refresh` inspects the resolved DBs,
  reports symbol-level drift between two DB directories, and harvests both
  files out of a local install — dry-run by default, `--yes` to write,
  `--into` always explicit, and never over the network. `just typedb-status`
  / `typedb-diff` / `typedb-refresh` / `typedb-refresh-apply` /
  `typedb-guard` wrap the maintainer flow, and a named CI step guards the
  shipped fixture DBs against a corrupt or truncated harvest.

### Changed
- **`.as` discovery honors `.gitignore` (GH #60)** — files excluded by
  `.gitignore` rules at or below the plugin root (plus a root-local
  `.git/info/exclude`) are no longer parsed or diagnosed. This matches
  rust-analyzer / gopls / tsc: a file you believe is excluded should not
  produce squiggles, and in real trees it drops second copies of a plugin's
  own sources (`.worktrees/`, scratch probe dirs) that collided with the
  real ones. Rules above the plugin root are never read, and a global
  gitignore / `.ignore` file is not consulted, so nothing invisible to the
  repo can shrink the checked set. `.git` directory contents are never
  discovered on either path. Opt out with `honor_gitignore = false` in
  `.openplanet-lsp.toml` (workspace wins over the user config) or
  `check --no-gitignore` (wins over both) when gitignored files are still
  compile-critical. `RUST_LOG=debug` logs how many files were excluded.

### Fixed
- **"Never used" no longer fires when the only caller is in a file we
  skipped** (GH #61) — a declaration whose callers all live in own-module
  files left out of the checked set (excluded by `.gitignore`, or filtered
  out by `source_paths` / `ignore_paths`) was reported "never used". The
  game compiles those files, so the warning was wrong. They are now read
  for that one purpose: the unused pass records what they reference. Their
  declarations stay invisible, they still produce no diagnostics of their
  own, and the namespace rule above stays disarmed for them. Cost is
  proportional to how much was omitted and nothing at all when nothing was
  (`check` on a 262-file plugin: unchanged at ~0.5s, ~0.74s with a
  gitignored second copy of the whole tree beside it).
- **Unknown members of your own namespaces are now flagged** (GH #58) —
  `TournamentStation::SendEvent(...)` where nothing named `SendEvent`
  exists under that namespace is a game compile error
  (`No matching symbol 'TournamentStation::SendEvent'`) that used to pass
  silently. Fires for any namespace declared in the plugin's own module or
  in a resolved dependency's export surface, with a near-name hint when
  one is close. Stays silent whenever the symbol set is not authoritative:
  a dependency that did not resolve (optional ones included), a
  `source_paths` / `ignore_paths` filter, a `.gitignore` that actually
  excluded a source file, an engine namespace, a nested path (`A::B::Fn`),
  or a name a sibling module contributes.
- **`check` no longer joins the unused report once per file** — the
  per-file diagnostics path asked for the whole workspace's findings and
  filtered, an O(files²) join. It now asks per file: `check` on the
  tm-editor-plus-plus fixture (≈300 files) drops from ~5.0s to ~0.6s.
- **Namespace-shortcut completions no longer shuffle between runs** — the
  `UI::`/`Math::` shortcut items all share one `sortText`, so the order the
  server builds them in is what the editor displays, and it was inheriting
  `HashMap` iteration order: the same file could offer the same completions
  in a different order on each request. Function and enum shortcuts are now
  emitted in sorted qualified-name order.
- **Loading the same type DB twice yields the same index** — 66 inline
  member-enum type names collide within a Nadeo module (15 distinct
  anonymous enums are all `Control::UnnamedEnum`) and were resolved by
  `HashMap` iteration order, so two loads of identical bytes could produce
  different enum registrations. Registration now walks modules and types in
  sorted name order.
- **Workspace function return types are real types, not `<error>`** —
  free-function symbol extraction left `return_type` empty, so hovering
  a call to a workspace function (e.g. `ts_log(...)` declared in another
  file) rendered `(local) <error> name`. Extraction now records the
  declared return type and `GlobalScope::lookup_function_return` parses
  it, so hover shows `(local) void ts_log`.
- **Saving a file refreshes sibling diagnostics** — the server now
  handles `textDocument/didSave` (advertised in capabilities): the
  owning shard rebuilds and every open document in it republishes
  diagnostics, so declaring a symbol in one file clears "undefined
  identifier" squiggles in the files that use it without needing an
  edit there.
- **Plugin-root discovery follows symlinks** — opening a folder whose
  plugin dirs are symlinks (e.g. `dips++-all`, where every entry links to
  a plugin living elsewhere) discovered zero roots and degraded to a
  single manifest-less workspace: externals from the typedb still worked
  but no local types, globals, or functions resolved. Discovery now
  classifies symlinked entries via `stat` (not `lstat`) and guards
  symlink cycles with a canonical-path seen set.
- **Symlink-alias document URIs resolve to snapshot files** — a document
  opened through a symlinked path now matches the snapshot entry loaded
  via the canonical plugin root (exact URI match first, canonical-path
  fallback), so type-recorded hover, unused-declaration warnings, and
  cross-file diagnostics work for symlinked workspaces too.
- **Manifest diagnostics publish to the open document's URI** — when
  `info.toml` is open through a symlink-alias path, manifest squiggles
  now land on that URI (using the live buffer content) instead of the
  canonical alias the client may never have opened.
- **Engine callback list completed** — `OnDisabled`, `OnEnabled`,
  `OnError`, `OnMouseButton`, `OnMouseMove`, `OnKeyRelease`,
  `OnSettingsLoad`, `OnSettingsSave`, `RenderEarly`, `RenderMenuMain`,
  and `Update` are now recognized as Openplanet engine entry points and
  no longer flagged as unused declarations.

### Added
- **`unused_callbacks` config key** (GH #56) — names the
  unused-declaration pass should treat as runtime-invoked callbacks, for
  frameworks that dispatch by name reflection (MLHook's
  `OnCounterUpdate`), which a fixed list cannot cover. Entries are
  literal names or a single `*` wildcard (`On*`, `*Update`, `On*Update`,
  bare `*`) — not a glob, not a regex. Additive to the built-in
  Openplanet callback list, never a replacement; matches unqualified
  names of free functions at file top level and exempts their parameters
  too. Workspace `.openplanet-lsp.toml` wins over the user config; lists
  do not merge across levels.
- **Unused locals and parameters** (GH #55) — the unused-declaration pass
  now covers function-body bindings — "a local variable `x` is never
  used", "a parameter `p` is never used" — with the same suppressions as
  before (a leading `_`, or a trailing `// lsp: ignore unused`). Usage is
  syntactic — any later mention in the enclosing function body counts, so
  a write with no read is *used* — and the scan reads the unmasked source
  so a mention inside an inactive `#if` branch still counts. Parameters
  stay silent where the signature is imposed rather than chosen:
  Openplanet callbacks, operator overloads, both halves of a virtual pair
  (overrides and the base they override), interface implementations
  (including inherited and partially-qualified contracts), mixin members,
  lambdas, functions handed to a funcdef (handler tables, `startnew`
  coroutines), no-op stubs whose body has zero statements, public-API
  export files, and any declaration the pass already reports as unused.
  Dogfooded over the `tm-*` fixtures across three rounds — 63 sites
  classified by hand plus a mechanical screen over every emitted warning
  — with no residual false positives, so the pass is on by default.
- **Define matrix for `check`** — `define_matrix` in
  `.openplanet-lsp.toml` lists preprocessor define axes that `check`
  cross-multiplies on top of the base define set, running every combo
  through the full analysis pipeline. Diagnostics are labeled with the
  combo they fired in (`[DEV]`, `[seen in: {} | DEV]` in plain/pretty
  output; a `"combo"` array in the new `--format json` report), and
  duplicates across combos are merged into one entry. Supporting keys:
  `define_groups` (mutually-exclusive defines — combos defining two
  members of one group are skipped; the platform selectors
  `{TMNEXT, MP4, TURBO}` form an implicit group whenever ≥2 are axes),
  `define_combinations` (explicit pin list that replaces the
  cross-product entirely), and `define_matrix_max_combinations`
  (cap, default 16 — exceeding it is a hard error pointing at
  `define_combinations`). Any `--define` CLI flag collapses the matrix
  back to a single-combo run. LSP mode is untouched (GH #57).
- **Unused-declaration warnings** — never-used declarations are flagged
  with `warning` severity across the whole workspace: file-level decls
  (functions, classes, interfaces, enums and their values, funcdefs,
  globals, `import ... from` aliases) and class members (fields, methods,
  properties). Usage is name-resolved, not text-matched, so sibling-file
  references count; usage in dependency exports counts too. Exemptions:
  Openplanet engine callbacks (`Main`, `Render*`, `OnKeyPress`, ...),
  constructors/destructors, operator overloads (`op*`), `override`
  methods (any ancestor declares the member), mixin classes and their
  members, and declarations in the plugin's own `info.toml`
  `exports`/`shared_exports` files (public API). Suppress per declaration
  with a leading `_` in the name or a trailing
  `// lsp: ignore unused` comment. This initial declaration-level slice
  excluded locals and parameters; the GH #55 addition above now covers
  function-body bindings too.
- **`warnings_as_errors` config option** — persist the
  `check --warnings-as-errors` / `-Werror` behaviour in
  `.openplanet-lsp.toml` (workspace) or
  `~/.config/openplanet-lsp/config.toml` (user). Precedence: CLI flag
  (including explicit `=false`) > workspace > user > default `false`.
  Together with the new unused warnings this pins code standards per
  project (GH #48 follow-up).

- **Namespace-shortcut completions** — at identifier positions, functions
  from a configurable set of namespaces are offered fully qualified:
  typing `Beg` suggests `UI::Begin` (old vscode-openplanet-angelscript
  QoL parity). Items are labelled `NS::Member`, filter on the bare member
  name, and sort after in-scope completions. Defaults: `UI` and `Math`
  built in, plus `UX` and `MathX` as custom defaults. Config (dotfile or
  init options): `shortcut_namespaces` (replaces the UX/MathX default),
  `shortcut_disable_default_namespaces` (drop UI/Math entirely),
  `shortcut_match_enums` (also suggest enums and enum values from those
  namespaces; off by default). Namespace variables are not matchable —
  the typedb has no global-variable data source. Shortcut items carry
  `(` as a commit character, label details (params marker + return
  type), markdown documentation with all overload signatures, and a
  dedicated `zz_shortcut_` sort prefix so they always rank below
  in-scope completions.

### Fixed
- **Single-arg constructor conversions in call arguments (GH #54)** — a
  value type's one-argument constructor is an implicit conversion in
  AngelScript, so `mat4::Inverse(someMat3)` compiles through
  `mat4(const mat3&in)`. `check` no longer reports
  `argument 1 of 'Inverse': expected 'mat4', got 'mat3'` for it. The
  accepted pairs are read from the typedb's own `asBEHAVE_CONSTRUCT`
  behaviors (copy constructors excluded), so every registered conversion
  is covered — `mat3`/`mat4`/`iso3`/`iso4`/`quat`, `string`/`wstring`,
  `RGBAColor` from `vec3`/`vec4`, `IO::File` from `string`, and the rest.
  Argument types with no registered constructor still fail
  (`mat4::Inverse(someVec3)`), game-verified by RemoteBuild probe.
- **Ternary branch unification (GH #52 repro 2)** — a ternary whose
  branches have no unifiable common type now exits 1 with the game's
  exact error `Can't find unambiguous implicit conversion to make both
  expressions have the same type`. Game-verified via RemoteBuild probes
  (2026-08-18) for: `string` vs `wstring` (`c ? "" : map.MapInfo.FileName`),
  non-numeric primitive pairs (`c ? 1 : true`, `c ? "" : 5`), distinct
  math value types (`c ? nat3v : int3v`), `string` vs math value types
  (`c ? "" : vec3(1,2,3)`), and numeric vs `null` (`c ? 1 : null`).
  Ternaries now also carry their unified result type, so numeric mixes
  keep feeding downstream checks (float-unified ternaries warn on `int`
  init like the game; mixed int×uint ternaries promote to `uint` and warn
  in relational mixes like the game), while the `Signed/Unsigned
  mismatch` `+` flavor stays silent through ternary operands — both
  game-verified.
- **False-negative batch (GH #47, #49, #50, #51, #52 repro 1)** — four
  error classes the game's AngelScript compiler rejects but `check`
  silently accepted (exit 0, zero diagnostics). All game-verified via
  RemoteBuild probes; every one now exits 1 in non-watch mode:
  - Member access on engine container properties (`root.Childs[i].Visible`
    on `MwFastArray<CControlBase@>` and friends) — indexing engine array
    generics now yields the element type, so own + base-chain member
    lookups run and `'X' is not a member of 'Y'` fires (GH #50, #52).
  - Member access on `cast<T>(x)` results — the cast now types as `T@`
    instead of unknown (GH #51).
  - `int3 v = nat3expr;` and `int3(nat3expr)` — distinct math value types
    (nat2/3, int2/3, vec2/3/4, iso3/4, quat) never convert implicitly
    (GH #49).
  - Overloaded engine calls where no overload matches — e.g.
    `UI::InputText(label, str, false)` (a `bool` value binds neither the
    `int flags` nor the `bool&out changed` parameter) → `No matching
    signatures to 'UI::InputText(const string, string, const bool)'`
    (GH #47). The same rule covers unique overloads: a **bool** literal
    into an `&out` slot (`UI::Begin("w", true)`) is rejected with the
    game's qualified-name wording, while numeric literals bind `&out` via
    temporaries (`Text::TryParseFloat("1.5", 0.0)` stays legal) — both
    game-verified via RemoteBuild probes.
- **Parser parity:** `class @Name` declarations (as shipped in PluginManager)
  parse as plain `Name` — the game accepts them; we previously emitted a
  parse-error cascade (19→1 pre-existing errors on that plugin).
- **Warning parity:** `Signed/Unsigned mismatch` no longer fires when a
  relational operand contains an explicit conversion (`u < int(E::LAST)` is
  silent in-game; only variable-vs-variable mixes warn).

## [0.5.0] - 2026-08-17

Architecture batch from the `improve-codebase-architecture` review (GH #39–#43).
No new diagnostics by design — this release is about the LSP being fast and
consistent — but request latency and memory behavior improve, and hover /
completion / signature help now answer from recorded checker state instead of
re-derived parses.

### Added
- **Queryable checker interface (GH #42):** `Checker` now records
  span→type results during its normal expression walk
  (`with_type_recording` / `type_at_span` / `recorded_expr_types`), giving
  server features a single authoritative type source. Hover, completion, and
  signature help consume the recorded types (stage 2) instead of re-parsing
  and re-inferring per request.

### Changed
- **One workspace view, not N re-derivations (GH #39):** new
  `AnalysisSnapshot` module — the backend caches a single snapshot per
  document change instead of re-loading and re-parsing the whole plugin per
  keystroke and per request; the CLI `check` uses the same snapshot. Cuts
  redundant work across all 13 request handlers.
- **Every feature through the `DocumentAnalysis` seam (GH #40):** hover,
  completion, signature, inlay hints, folding, highlights, symbols,
  navigation, call hierarchy and semantic tokens all consume the same
  `&DocumentAnalysis` view (same preprocessing + AST), ending the
  "one query, three answers" divergence. (Formatter intentionally stays on
  the raw AST so `#if 0`-disabled code is never deleted on format.)
- **Deeper `GlobalScope`, closed pub-field seam (GH #41):** the
  `workspace`/`external` fields are now private; the name-resolution ladder
  and class-member/inheritance walks that server features and the checker
  previously duplicated now live behind the `GlobalScope` interface.
  `SymbolTable` lookups are indexed instead of linear scans — faster on
  large multi-file plugins.
- **Tests cross the production seam (GH #43):** test-only `Option` fallbacks
  in `WorkspaceFiles` are gone; tests exercise the same
  `TestWorkspace` → `DocumentAnalysis` → checker path production uses.

### Breaking (library API)
- `GlobalScope::workspace` / `GlobalScope::external` are private; use the
  interface methods instead.
- Feature entry points now take `&DocumentAnalysis` (and the backend cache is
  `Arc<AnalysisSnapshot>`), so downstream code calling the old signatures
  must adapt. CLI behavior is unchanged.

### Distribution
- GitHub Release binaries, npm (meta + 6 platform packages), crates.io —
  all at 0.5.0, published by `release.yml` from tag `v0.5.0`.

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
