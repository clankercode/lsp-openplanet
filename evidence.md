# Config reload on disk change — evidence

## What changed

Live `check --watch` and a running LSP reload when either config file
changes, without a restart.

- `src/config_watch.rs` (new): classifies workspace
  `.openplanet-lsp.toml` vs `$HOME/.config/openplanet-lsp/config.toml`;
  picks the nearest existing user-config parent to watch; invalid TOML
  → keep last-good; shared 250ms notify helper.
- `src/cli/watch.rs`: those paths are interesting; the watcher also
  watches the user-config parent so create/replace is visible. Invalid
  TOML does not set dirty.
- `src/server/mod.rs`: `.openplanet-lsp.toml` and the user-global URI
  in `did_change_watched_files` re-run `LspConfig::load` /
  `load_with_parent` (force rebuild; `configs_equivalent` now includes
  `honor_gitignore` / shortcuts / unused callbacks). After
  `initialized`, a notify watch covers the user file (clients do not)
  plus plugin roots so workspace config is seen even without a client
  watcher. Open-document diagnostics are republished.
- `src/lib.rs`: `pub mod config_watch`.
- `tests/config_reload_tests.rs`: create / modify / delete, invalid
  TOML, shortcuts, user-URI routing, notify-watch wiring.
- `CHANGELOG.md`: user-visible note.

## Tests run

- Red first: `interesting_paths` failed until `.openplanet-lsp.toml`
  and the user-global suffix were classified. `config_reload_tests`
  failed until `did_change_watched_files` reloaded the ladder.
- Green: `cargo test --locked -- --test-threads=2` — all suites passed
  (lib 937, plus integration including `config_reload_tests`,
  `multi_root_*`, `gitignore_*`, `cli_*`, TUI snapshots).

## How to verify

1. `openplanet-lsp check --watch` in a plugin. Edit
   `.openplanet-lsp.toml` (`honor_gitignore`, `source_paths`,
   `game_target`, `defines`) — a re-check should run with the new
   settings. Same for `$HOME/.config/openplanet-lsp/config.toml`.
2. Break the TOML (`defines = [`) — session stays up; last-good
   settings remain.
3. In a running LSP, change either file — diagnostics / completion
   shortcuts / discovery should update without restarting.

## Known remaining risks

- `game_target` reload updates defines/snapshots but does not reload
  the type-DB index (pre-existing: reshard never reloaded it).
- Notify watches on plugin roots are armed at `initialized`. A folder
  added later still reloads workspace config if the client sends
  `didChangeWatchedFiles`; otherwise wait for the next initialized
  session.
- A mid-write partial file can look like invalid TOML and skip one
  debounce window (250ms). A later event recovers.
- User-global *apply* is not asserted in-process (mutating `$HOME`
  races other tests). Wiring is tested via notify + a workspace file
  already on disk; loaders already cover reading the user path.
