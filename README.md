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

## Release / distribution

See **[RELEASE.md](RELEASE.md)** for the full procedure (keep it up to date when
release tooling changes):

- multi-platform GitHub Release binaries (Linux / macOS / Windows, x64 + arm64)
- npm publish of `openplanet-lsp` + platform packages
- version bump + tag procedure
- **changelog writing + post-CI `gh release edit`**
- local smoke test (`scripts/release/smoke-local.sh`)
