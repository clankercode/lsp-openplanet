# Releasing openplanet-lsp

This project ships two distribution channels from the same build matrix:

1. **GitHub Releases** — platform archives (`.tar.gz` / `.zip`) attached to a tag.
2. **npm** — a meta package `openplanet-lsp` plus optional platform packages
   (`openplanet-lsp-linux-x64`, `…-darwin-arm64`, `…-win32-x64`, etc.) that
   contain the native binary.

## One-time setup

### GitHub

- Actions must be allowed to create releases (default `GITHUB_TOKEN` is enough
  with `contents: write` on the workflow).
- Push access to `clankercode/lsp-openplanet`.

### npm

1. Create an [npm access token](https://www.npmjs.com/settings/~/tokens) with
   **Automation** permission (or granular publish on the package names below).
2. Add it as a repository secret:
   ```bash
   gh secret set NPM_TOKEN
   ```
3. Reserve package names on first publish (the release workflow does this):
   - `openplanet-lsp`
   - `openplanet-lsp-linux-x64`
   - `openplanet-lsp-linux-arm64`
   - `openplanet-lsp-darwin-x64`
   - `openplanet-lsp-darwin-arm64`
   - `openplanet-lsp-win32-x64`
   - `openplanet-lsp-win32-arm64`

No npm org is required — packages are unscoped.

## Supported targets

| Runner            | Rust target                    | npm package                    | Archive   |
|-------------------|--------------------------------|--------------------------------|-----------|
| ubuntu-22.04      | x86_64-unknown-linux-gnu       | openplanet-lsp-linux-x64       | `.tar.gz` |
| ubuntu-24.04-arm  | aarch64-unknown-linux-gnu      | openplanet-lsp-linux-arm64     | `.tar.gz` |
| macos-13          | x86_64-apple-darwin            | openplanet-lsp-darwin-x64      | `.tar.gz` |
| macos-14          | aarch64-apple-darwin           | openplanet-lsp-darwin-arm64    | `.tar.gz` |
| windows-latest    | x86_64-pc-windows-msvc         | openplanet-lsp-win32-x64       | `.zip`    |
| windows-11-arm    | aarch64-pc-windows-msvc        | openplanet-lsp-win32-arm64     | `.zip`    |

## Release procedure

Versions must match across:

- `Cargo.toml` `version`
- every `npm/*/package.json` `version`
- git tag `vX.Y.Z`

### 1. Bump version

```bash
./scripts/release/bump-version.sh 0.3.0
cargo build   # refresh Cargo.lock if the version string is embedded anywhere
```

### 2. Commit

```bash
git add Cargo.toml Cargo.lock npm
git commit -m "chore: release v0.3.0"
```

### 3. Tag and push

```bash
git tag -a v0.3.0 -m "v0.3.0"
git push origin HEAD
git push origin v0.3.0
```

Pushing the tag starts [`.github/workflows/release.yml`](../.github/workflows/release.yml).

### 4. Watch CI

```bash
gh run list --workflow=release.yml --limit 5
gh run watch
```

On success:

- GitHub Release `v0.3.0` has one archive per target
- npm shows `openplanet-lsp@0.3.0` and matching platform packages

### Dry run (no publish)

```bash
gh workflow run release.yml -f dry_run=true
```

Builds and packs only; skips release upload and `npm publish`.

## Local smoke test (current host only)

Builds a release binary, packs the matching platform + meta npm packages into a
temp prefix, and runs `--version` through the Node launcher:

```bash
./scripts/release/smoke-local.sh
```

## Install for end users

```bash
npm install -g openplanet-lsp
openplanet-lsp --version
openplanet-lsp check /path/to/plugin
```

Or grab a GitHub Release archive and put `openplanet-lsp` on `PATH`.

## Layout

```
npm/
  openplanet-lsp/                 # meta package (bin launcher)
  openplanet-lsp-linux-x64/       # native binary packages (filled in CI)
  openplanet-lsp-linux-arm64/
  openplanet-lsp-darwin-x64/
  openplanet-lsp-darwin-arm64/
  openplanet-lsp-win32-x64/
  openplanet-lsp-win32-arm64/
scripts/release/
  bump-version.sh
  smoke-local.sh
.github/workflows/
  ci.yml                          # PR/main tests + npm manifest smoke
  release.yml                     # tag → binaries + GH release + npm publish
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Release job fails “Cargo.toml version != tag” | Run `bump-version.sh` and retag |
| `npm publish` 401/403 | Check `NPM_TOKEN` secret; ensure packages are not taken by another user |
| Optional platform package missing after install | npm skipped optionalDep (offline/mirror). `npm i openplanet-lsp-<platform>@x.y.z` or use GH release binary |
| `windows-11-arm` runner unavailable | Remove that matrix row or mark the job non-blocking; x64 Windows still ships |

## First publish checklist

- [ ] `NPM_TOKEN` secret set on the repo
- [ ] Package names free on registry.npmjs.org
- [ ] `Cargo.toml` version matches intended tag
- [ ] `./scripts/release/smoke-local.sh` passes on a dev machine
- [ ] Tag `vX.Y.Z` pushed
- [ ] Release workflow green
- [ ] `npm view openplanet-lsp version` shows the new version
