# openplanet-lsp

Language Server Protocol implementation for OpenPlanet AngelScript.

This npm package installs a small Node launcher plus an optional
platform-specific native binary (`openplanet-lsp-*`).

## Install

```bash
npm install -g openplanet-lsp
# or project-local:
npm install --save-dev openplanet-lsp
```

## Usage

```bash
openplanet-lsp              # stdio LSP server
openplanet-lsp --version
openplanet-lsp check <path>
openplanet-lsp update --check
openplanet-lsp update
```

`update` reads the latest version from the npm registry, writes status to
`~/.config/openplanet-lsp/update-status.json`, and (without `--check`) runs the
install-method-specific upgrade command when one is known (npm / pnpm / yarn /
bun global or local, or cargo).

## Supported platforms

| Platform package                 | OS      | Arch  |
|----------------------------------|---------|-------|
| `openplanet-lsp-linux-x64`       | Linux   | x64   |
| `openplanet-lsp-linux-arm64`     | Linux   | arm64 |
| `openplanet-lsp-darwin-x64`      | macOS   | x64   |
| `openplanet-lsp-darwin-arm64`    | macOS   | arm64 |
| `openplanet-lsp-win32-x64`       | Windows | x64   |
| `openplanet-lsp-win32-arm64`     | Windows | arm64 |

GitHub release archives (same binaries) are also published at:
https://github.com/clankercode/lsp-openplanet/releases

## Programmatic

```js
const { resolveBinaryPath } = require("openplanet-lsp");
console.log(resolveBinaryPath());
```
