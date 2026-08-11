# openplanet-lsp

Language Server Protocol implementation for OpenPlanet AngelScript.

## Build

```bash
cargo build --release
./target/release/openplanet-lsp --help
```

## Test

```bash
cargo test
```

## Install (end users)

Once a release is published:

```bash
npm install -g openplanet-lsp
openplanet-lsp --version
```

Or download platform archives from
[GitHub Releases](https://github.com/clankercode/lsp-openplanet/releases).

## Update

```bash
openplanet-lsp update --check   # query npm + write status file
openplanet-lsp update --status  # print last saved status (offline)
openplanet-lsp update           # apply update via detected install method
```

Latest version is read from the **npm registry** (not the GitHub API). The
binary path is classified as `npm-global`, `npm-local`, `cargo`, `development`,
or `standalone`, and the matching upgrade command is used when possible
(`npm install -g …`, project-local `npm install …`, or
`cargo install --git …`).

Status is written to `~/.config/openplanet-lsp/update-status.json`
(override with `OPENPLANET_LSP_CONFIG_DIR`). The language server also probes
in the background about once per day and shows an editor info message when a
newer release is available.

## Release / distribution

See **[RELEASE.md](RELEASE.md)** for the full procedure (keep it up to date when
release tooling changes):

- multi-platform GitHub Release binaries (Linux / macOS / Windows, x64 + arm64)
- npm publish of `openplanet-lsp` + platform packages
- version bump + tag procedure
- **changelog writing + post-CI `gh release edit`**
- local smoke test (`scripts/release/smoke-local.sh`)
