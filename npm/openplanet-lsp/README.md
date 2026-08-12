# openplanet-lsp

Language Server Protocol implementation for **OpenPlanet AngelScript**
(Trackmania / OpenPlanet plugins).

This npm package installs a small Node launcher plus an optional
platform-specific native binary (`openplanet-lsp-*`).

![openplanet-lsp check — pretty diagnostics](https://raw.githubusercontent.com/clankercode/lsp-openplanet/master/docs/images/check-demo.png)

![openplanet-lsp watch TUI — relaxed density](https://raw.githubusercontent.com/clankercode/lsp-openplanet/master/docs/images/watch-demo.png)


## Install

```bash
npm install -g openplanet-lsp
# or project-local:
npm install --save-dev openplanet-lsp
```

```bash
openplanet-lsp --version
openplanet-lsp --help
```

## Use as an LSP

Runs as a **stdio** language server (no args):

```bash
openplanet-lsp
```

Point your editor’s LSP client at the `openplanet-lsp` binary. Workspace root
should be the plugin directory (the folder with `info.toml`).

**Neovim** (sketch):

```lua
vim.lsp.start({
  name = "openplanet-lsp",
  cmd = { "openplanet-lsp" },
  root_dir = vim.fs.root(0, { "info.toml", ".git" }),
})
```

**Helix** (`languages.toml` sketch):

```toml
[language-server.openplanet-lsp]
command = "openplanet-lsp"
```

See the [full README on GitHub](https://github.com/clankercode/lsp-openplanet#use-as-an-lsp-editor)
for VS Code–style settings and more.

## CLI

```bash
# Diagnostics for a plugin tree (offline)
openplanet-lsp check .
openplanet-lsp check ./MyPlugin

# Self-update
openplanet-lsp update --check
openplanet-lsp update --check --source github   # or: crate | npm
openplanet-lsp update --status
openplanet-lsp update
```

Status looks like:

```text
current:  0.3.0 (install type: npm-global)
latest:   0.3.0 (source checked: npm)
status:   up to date
```

## Supported platforms

| Platform package             | OS      | Arch  |
|-----------------------------|---------|-------|
| `openplanet-lsp-linux-x64`  | Linux   | x64   |
| `openplanet-lsp-linux-arm64`| Linux   | arm64 |
| `openplanet-lsp-darwin-x64` | macOS   | x64   |
| `openplanet-lsp-darwin-arm64` | macOS | arm64 |
| `openplanet-lsp-win32-x64`  | Windows | x64   |
| `openplanet-lsp-win32-arm64`| Windows | arm64 |

Also: [GitHub Releases](https://github.com/clankercode/lsp-openplanet/releases) ·
[crates.io](https://crates.io/crates/openplanet-lsp)

## Programmatic

```js
const { resolveBinaryPath } = require("openplanet-lsp");
console.log(resolveBinaryPath());
```

## License

Unlicense OR CC0-1.0 (public domain dedication).
