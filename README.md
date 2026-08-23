# openplanet-lsp

![openplanet-lsp check — pretty diagnostics](docs/images/check-demo.png)

![openplanet-lsp watch TUI — relaxed density on showcase-diags](docs/images/watch-demo.png)

Language Server Protocol implementation for **Openplanet AngelScript**
(Trackmania / Openplanet plugins).

## Install

```bash
# npm (recommended for most users)
npm install -g openplanet-lsp

# cargo (crates.io)
cargo install openplanet-lsp

# or download a platform archive from
# https://github.com/clankercode/lsp-openplanet/releases
# extract `openplanet-lsp` somewhere on PATH (e.g. ~/.local/bin)
```

```bash
openplanet-lsp --version
openplanet-lsp --help
```

## Use as an LSP (editor)

`openplanet-lsp` speaks **JSON-RPC over stdio**.

| How you launch | What you get |
|----------------|--------------|
| Editor / non-TTY stdio (no args) | Language server |
| TTY, inside a plugin (`info.toml`) | Watch TUI (default) |
| `openplanet-lsp --lsp` or `lsp` | Force language server |
| TTY, no plugin nearby | Short help (exit 2) |

Config (`~/.config/openplanet-lsp/config.toml` or workspace `.openplanet-lsp.toml`):

```toml
# bare TTY default when no subcommand is given
default_mode = "tui"   # or "lsp"

# Game target — picks the preprocessor platform defines (#if TMNEXT / MP4 /
# TURBO / ...). Default "TMNEXT" (TM2020). Override for plugins that only
# support another game.
# game_target = "TMNEXT"

# Which .as files to check, relative to the plugin root. Openplanet compiles a
# specific set; repos often keep non-compiled scripts (asset packs, fixtures,
# experiments) alongside src/. source_paths is an ALLOWLIST (wins if set);
# ignore_paths is a BLOCKLIST (used only when source_paths is absent); with
# neither, every .as under the root is checked (safe default).
# source_paths = ["src"]
# ignore_paths = ["OtherPacks", "vendor"]

# Namespace-shortcut completions (old vscode-openplanet-angelscript QoL):
# typing `Beg` suggests `UI::Begin` etc. at identifier positions. Built-in
# defaults are `UI` + `Math`; `shortcut_namespaces` sets your own custom
# list, replacing the default `UX` + `MathX`. Set
# `shortcut_disable_default_namespaces = true` to drop UI/Math entirely
# (leaving only your custom list, or none); `shortcut_match_enums = true`
# also suggests enums and enum values. Namespace variables are never
# matched — the typedb has no global-variable data source.
# shortcut_namespaces = ["UX", "MathX", "Net"]
# shortcut_disable_default_namespaces = false
# shortcut_match_enums = false

# Define matrix (CLI `check` only; the LSP server keeps one live define
# set). define_matrix lists axes toggled on/off on top of the base define
# set — check runs the cross-product of all combos through the full
# pipeline. Diagnostics are labeled with the combo they fired in
# ([DEV] / [seen in: {} | DEV] in plain/pretty output, a "combo" array
# in --format json), and duplicates across combos are merged into one
# entry. The platform selectors {TMNEXT, MP4, TURBO} form an implicit
# mutually-exclusive group whenever ≥2 of them are axes.
# define_matrix = ["DEV", "DEPENDENCY_MLHOOK"]

# Mutually-exclusive define groups: at most one member is defined per
# combo; violating combos are skipped (never analyzed).
# define_groups = [["FEATURE_A", "FEATURE_B"]]

# Explicit combo pin list: when present (even []), the cross-product is
# skipped entirely and ONLY these combos are checked.
# define_combinations = [[], ["DEV"], ["DEV", "DEPENDENCY_MLHOOK"]]

# Matrix size cap (default 16). A cross-product expanding past the cap is
# a hard error — pin combos with define_combinations or raise the cap.
# define_matrix_max_combinations = 16
#
# Any `--define` CLI flag collapses the matrix back to a single-combo run.
```

Point your editor’s AngelScript / Openplanet language client at the binary
(stdio, **no args** — editors attach pipes, so bare launch stays LSP).

### Generic LSP client settings

| Setting | Value |
|---------|--------|
| Command | `openplanet-lsp` (or full path) |
| Args | _(none)_ |
| Transport | **stdio** |
| File types | typically `.as`, `.op`, Openplanet plugin trees |

Workspace root should be the **plugin directory** (the folder that contains
`info.toml` and your `.as` sources).

### VS Code / Cursor / similar

Use any generic LSP extension (e.g. [vscode-languageclient](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)
wrapper) or an Openplanet-specific extension that shells out to `openplanet-lsp`.
Minimal `settings.json` shape for a generic client:

```json
{
  "myAsLsp.server.path": "openplanet-lsp",
  "myAsLsp.server.args": [],
  "myAsLsp.trace.server": "off"
}
```

Exact keys depend on the extension; the important part is **command =
`openplanet-lsp`, no args, stdio**.

### Neovim (nvim-lspconfig style)

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = { "angelscript", "as" },
  callback = function()
    vim.lsp.start({
      name = "openplanet-lsp",
      cmd = { "openplanet-lsp" },
      root_dir = vim.fs.root(0, { "info.toml", ".git" }),
    })
  end,
})
```

### Helix

In `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "angelscript"
scope = "source.angelscript"
file-types = ["as"]
language-servers = ["openplanet-lsp"]

[language-server.openplanet-lsp]
command = "openplanet-lsp"
```

### What the server provides

Depends on build features, but typically includes:

- diagnostics (parse / typecheck where available)
- hover, go-to-definition, references, document symbols
- completion and signature help
- formatting / folding / semantic tokens (where implemented)

Optional config: place an Openplanet / LSP config near the workspace (see
project docs) or pass environment overrides used by the CLI.

Background update probes run about once per day and can surface an editor
notification when a newer release is available.

## CLI

```text
openplanet-lsp [FLAGS]              # bare: TTY+plugin → watch TUI; else LSP
openplanet-lsp --lsp | lsp          # force language server
openplanet-lsp check [OPTIONS] [PATH]
openplanet-lsp check --watch [PATH] # live diagnostics TUI
openplanet-lsp update [OPTIONS]
```

### `check` — offline plugin diagnostics

Typecheck / lint an Openplanet plugin tree without an editor:

```bash
# one-shot (pretty on TTY; plain when piped / NO_COLOR)
openplanet-lsp check /path/to/plugin
openplanet-lsp check --format plain ./tests/fixtures/showcase-diags
openplanet-lsp check .                  # PATH = plugin root or a .as file
openplanet-lsp check --help

# live watch TUI (re-checks on *.as / *.inc / info.toml changes)
openplanet-lsp check --watch .
# or, from inside a plugin directory on a TTY:
openplanet-lsp
```

#### Watch TUI

Interactive diagnostics browser (requires a real TTY).

| Key | Action |
|-----|--------|
| `j` / `k` or arrows | Move selection |
| `PgUp` / `PgDn` / Space | Page |
| `g` / `G` (Home/End) | Top / end |
| `c` | Toggle **compact** ↔ **relaxed** density |
| `r` | Manual refresh |
| `q` / Esc / Ctrl-C | Quit |

- **Compact:** one row per diagnostic (message ellipsized).
- **Relaxed:** location + message; right-aligned `› fragment ‹` on the location row.
- **Detail:** pretty source excerpt with carets for the selected item.
- Auto-refresh watches `*.as`, `*.inc`, and `info.toml` under the plugin root. If the watcher fails to start, the header shows `watch off · r to refresh`.
- Header status: `checking…` while a run is in flight; `checked in N ms` when ready; last-good list is labeled **stale** during a check or after failure.
- Checks run off the UI thread so navigation stays responsive.

Exit codes for **one-shot** `check`: **0** if no errors (warnings allowed); **1** if diagnostics include errors; **2** on usage / IO failures.

Interactive **`check --watch`**: exits **0** on normal quit; **2** on setup/runtime failure (does not return the last diagnostic status).

Useful options (see `--help` for the full list):

| Flag | Meaning |
|------|---------|
| `--watch` | Live TUI; re-check on file changes |
| `--format plain\|pretty\|auto` | One-shot output style (ignored with `--watch`) |
| `--typedb-dir <DIR>` | Load Openplanet type database from DIR |
| `--no-typedb` | Skip type DB (parse-only / limited checks) |
| `--plugins-dir <DIR>` | Extra Openplanet plugins dir for dependency exports |

### `update` — self-update

```bash
openplanet-lsp update --check              # query latest + write status file
openplanet-lsp update --check --source github
openplanet-lsp update --check --source crate
openplanet-lsp update --status             # print last saved status (offline)
openplanet-lsp update                      # apply via detected install method
openplanet-lsp update --force              # reinstall even if already latest
```

**Version source** (`--source`, default `npm`):

| Value | Channel |
|-------|---------|
| `npm` | registry.npmjs.org (default) |
| `crate` | crates.io (`openplanet-lsp`) |
| `github` | latest GitHub Release tag |

**Install method** is detected from the binary path:

| Method | How update applies |
|--------|--------------------|
| npm / pnpm / yarn / bun (global or local) | package-manager install |
| cargo | `cargo install --git … --force` |
| standalone (`~/.local/bin`, manual extract, …) | download GH Release archive + replace binary |
| development (`target/release/…`) | not auto-updated — rebuild yourself |

Status output looks like:

```text
current:  0.3.0 (install type: standalone)
latest:   0.3.0 (source checked: npm)
status:   up to date
```

Status file: `~/.config/openplanet-lsp/update-status.json`
(override with `OPENPLANET_LSP_CONFIG_DIR`).

### Dev / CI env overrides

| Env | Effect |
|-----|--------|
| `OPENPLANET_LSP_VERSION` | Pretend current version for compare (`--version` stays real) |
| `OPENPLANET_LSP_LATEST_VERSION` | Skip network; treat as latest |
| `OPENPLANET_LSP_UPDATE_PACKAGE` | Install target(s) instead of `@latest` |
| `OPENPLANET_LSP_PACKAGE_MANAGER` | Force `npm` / `pnpm` / `yarn` / `bun` |
| `OPENPLANET_LSP_EXE` | Fake binary path for install-method detection |
| `OPENPLANET_LSP_RELEASE_ARCHIVE` | Local `.tar.gz`/`.zip` for standalone apply tests |

## Build from source

```bash
cargo build --release
./target/release/openplanet-lsp --help
cargo test
```

## Smoke tests

```bash
./scripts/release/smoke-local.sh
./scripts/release/smoke-self-update.sh

FROM_VERSION=latest TARGET_VERSION=latest \
  ./scripts/release/smoke-self-update-registry.sh
# or Actions → "self-update-matrix"
```

## Release / distribution

See **[RELEASE.md](RELEASE.md)** (keep it accurate when tooling changes):

- multi-platform GitHub Release binaries (Linux / macOS / Windows × x64 / arm64)
- npm (`openplanet-lsp` + platform packages) via OIDC trusted publishing
- crates.io via Trusted Publishing (`release.yml` + `id-token: write`)
- version bump, tag, changelog, post-CI `gh release edit`
