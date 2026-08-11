# RELEASE.md — openplanet-lsp release procedure

**Agents and humans: keep this file accurate.**
If you change `.github/workflows/release.yml`, npm package layout, version
bump scripts, supported targets, secrets, or any step below, update this
document in the **same PR/commit**. Stale release docs are a bug.

Canonical location: repository root `RELEASE.md`
(Also linked from `docs/RELEASE.md` and `README.md`.)

---

## What a release produces

One git tag (`vX.Y.Z`) drives both channels via
[`.github/workflows/release.yml`](.github/workflows/release.yml):

1. **GitHub Release** — multi-platform binary archives attached to the tag
2. **npm** — meta package `openplanet-lsp` + optional platform packages with
   the native binary

| Runner           | Rust target                 | npm package                 | Archive   |
|------------------|-----------------------------|-----------------------------|-----------|
| ubuntu-22.04     | x86_64-unknown-linux-gnu    | openplanet-lsp-linux-x64    | `.tar.gz` |
| ubuntu-24.04-arm | aarch64-unknown-linux-gnu   | openplanet-lsp-linux-arm64  | `.tar.gz` |
| macos-13         | x86_64-apple-darwin         | openplanet-lsp-darwin-x64   | `.tar.gz` |
| macos-14         | aarch64-apple-darwin        | openplanet-lsp-darwin-arm64 | `.tar.gz` |
| windows-latest   | x86_64-pc-windows-msvc      | openplanet-lsp-win32-x64    | `.zip`    |
| windows-11-arm   | aarch64-pc-windows-msvc     | openplanet-lsp-win32-arm64  | `.zip`    |

CI creates the GitHub Release with **auto-generated** notes and uploads
artifacts. **After CI goes green, an agent (or human) must write a proper
changelog and update the release body** — see [Post-CI](#5-post-ci--changelog--update-github-release).

---

## One-time setup

### GitHub

- Push access to `clankercode/lsp-openplanet`
- Actions allowed to create releases (`contents: write` on the workflow is enough for `GITHUB_TOKEN`)
- Workflow must grant `id-token: write` for npm OIDC (already set in `release.yml`)
- `gh` CLI authenticated (`gh auth status`)

### npm — GitHub OIDC trusted publishing (preferred; no long-lived token)

Releases publish with **OpenID Connect** from GitHub Actions. There is **no
`NPM_TOKEN` secret**. Docs: https://docs.npmjs.com/trusted-publishers

#### Packages (must already exist on the registry)

| Package |
|---------|
| `openplanet-lsp` |
| `openplanet-lsp-linux-x64` |
| `openplanet-lsp-linux-arm64` |
| `openplanet-lsp-darwin-x64` |
| `openplanet-lsp-darwin-arm64` |
| `openplanet-lsp-win32-x64` |
| `openplanet-lsp-win32-arm64` |

First-time claim: publish each package once from a maintainer machine (we
already did this for `0.2.0`). After that, CI owns subsequent versions via OIDC.

#### Configure trusted publisher (CLI)

Requires **npm ≥ 11.15**, account **2FA**, and write access on each package.

```bash
# One package:
npm trust github openplanet-lsp \
  --file release.yml \
  --repo clankercode/lsp-openplanet \
  --allow-publish -y

# All packages (2s pause avoids rate limits; first call may prompt 2FA —
# enable “skip 2FA for 5 minutes” on the npm site if bulk-configuring):
for p in openplanet-lsp \
  openplanet-lsp-linux-x64 openplanet-lsp-linux-arm64 \
  openplanet-lsp-darwin-x64 openplanet-lsp-darwin-arm64 \
  openplanet-lsp-win32-x64 openplanet-lsp-win32-arm64
do
  npm trust github "$p" \
    --file release.yml \
    --repo clankercode/lsp-openplanet \
    --allow-publish -y
  sleep 2
done

# Verify:
npm trust list openplanet-lsp
```

Expected trust config per package:

| Field | Value |
|-------|--------|
| type | `github` |
| repository | `clankercode/lsp-openplanet` |
| file | `release.yml` (filename only) |
| permissions | `publish` |

UI equivalent: each package → **Settings → Trusted Publisher → GitHub Actions**.

#### Optional hardening

After OIDC works end-to-end:

- Package **Settings → Publishing access** → require 2FA and **disallow tokens**
- Do **not** add an `NPM_TOKEN` Actions secret (the workflow refuses token auth)

#### CI requirements (already in `release.yml`)

- `permissions.id-token: write` (and `contents: write` for GH Release assets)
- Publish job: Node **24**, `registry-url: https://registry.npmjs.org`
- `npm publish … --access public --provenance` with **no** `NODE_AUTH_TOKEN`
- Workflow file name on disk must stay **`release.yml`** (trusted publisher match)

## Version lockstep (required)

These **must** match before tagging:

| Source                         | Example   |
|--------------------------------|-----------|
| `Cargo.toml` `version`         | `0.3.0`   |
| every `npm/*/package.json`     | `0.3.0`   |
| git tag                        | `v0.3.0`  |

The release workflow **fails** unless `Cargo.toml`, every npm package version,
every meta-package optional-dependency pin, and the tag version all match.

Use the helper:

```bash
./scripts/release/bump-version.sh 0.3.0
```

---

## Full release procedure

### 0. Preconditions

- [ ] On a clean branch that will become the release tip (usually `master`)
- [ ] `cargo test` green (or CI green on the commit you will tag)
- [ ] Optional local smoke: `./scripts/release/smoke-local.sh`
- [ ] npm trusted publishers configured for all 7 packages (`npm trust list …`)
- [ ] You know the previous tag for the changelog range:
  ```bash
  git describe --tags --abbrev=0
  # or: gh release list --limit 5
  ```

### 1. Bump version

```bash
./scripts/release/bump-version.sh 0.3.0
cargo build   # refresh Cargo.lock if needed
```

### 2. Draft changelog (repo file)

Maintain a running changelog at **`CHANGELOG.md`** (create on first release if
missing). Add a new section **at the top** under the heading for this version
**before or immediately after** the version bump commit.

Format (Keep a Changelog style):

```markdown
# Changelog

## [0.3.0] - YYYY-MM-DD

### Added
- …

### Fixed
- …

### Changed
- …

### Distribution
- npm / GitHub Release notes for this version
```

How to gather notes (agents should do this, not invent):

```bash
PREV=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
# If PREV is empty, this is the first tagged release — summarize major features.
git log --oneline ${PREV:+${PREV}..}HEAD
git log --format='%h %s' ${PREV:+${PREV}..}HEAD
# Prefer user-facing themes over raw commit spam. Group by Added/Fixed/Changed.
# Include notable compiler-parity / LSP behavior changes.
```

Also prepare a **GitHub release body** (same content is fine) that will be
applied **after** CI finishes — see step 5. You may keep a draft in
`/tmp/release-notes-v0.3.0.md` during the release.

### 3. Commit release metadata

```bash
git add Cargo.toml Cargo.lock npm CHANGELOG.md RELEASE.md
git commit -m "chore: release v0.3.0"
```

Include `CHANGELOG.md` in this commit whenever it changed.

### 4. Tag and push (starts CI)

```bash
git tag -a v0.3.0 -m "v0.3.0"
git push origin HEAD
git push origin v0.3.0
```

Pushing the tag starts `release.yml`.

Watch until **success**:

```bash
gh run list --workflow=release.yml --limit 5
gh run watch
# or:
gh run list --workflow=release.yml --branch v0.3.0 --limit 1
```

On success, CI has typically:

- Built all matrix targets
- Created/updated GitHub Release `v0.3.0` with binary assets
- Published npm platform packages, then the meta package

**Do not stop here.** Auto-generated release notes are not the final changelog.

### 5. Post-CI — changelog + update GitHub Release

This step is **mandatory** for every release once `release.yml` has finished
successfully.

#### 5a. Confirm artifacts

```bash
TAG=v0.3.0
VERSION=0.3.0

# Release exists and has assets
gh release view "$TAG"
gh release view "$TAG" --json assets --jq '.assets[].name'

# npm published
npm view openplanet-lsp version
npm view openplanet-lsp@$VERSION version
npm view openplanet-lsp-linux-x64@$VERSION version
```

If CI failed: fix forward, retag only if necessary (prefer a new patch version
over moving tags). Do **not** write fake success notes.

#### 5b. Finalize changelog text

Ensure `CHANGELOG.md` section for `$VERSION` is complete and accurate against:

- `git log` since previous tag
- PR titles merged in the range (optional: `gh pr list --search "merged:>=…"`)
- Behavior users care about (diagnostics, install paths, breaking changes)

If you only drafted notes earlier, commit any final `CHANGELOG.md` edits on
master (no need to retag for doc-only fixes unless you want the tag commit to
include them — doc-only follow-up commits are fine).

#### 5c. Update the GitHub Release body

Replace the CI auto-notes with the real changelog (keep asset files untouched):

```bash
TAG=v0.3.0
VERSION=0.3.0

# Extract this version's section from CHANGELOG.md into a temp body,
# or write notes explicitly:
cat > /tmp/gh-release-${TAG}.md <<EOF
## openplanet-lsp ${VERSION}

$(awk "/^## \\[${VERSION}\\]/{flag=1; next} /^## \\[/{flag=0} flag" CHANGELOG.md)

### Install

\`\`\`bash
npm install -g openplanet-lsp@${VERSION}
openplanet-lsp --version
\`\`\`

Binaries are attached below (Linux / macOS / Windows, x64 and arm64).

### npm packages

- \`openplanet-lsp@${VERSION}\` (launcher)
- \`openplanet-lsp-linux-x64\`, \`linux-arm64\`, \`darwin-x64\`, \`darwin-arm64\`, \`win32-x64\`, \`win32-arm64\`
EOF

# Update title + body; do not delete assets
gh release edit "$TAG" \
  --title "openplanet-lsp ${VERSION}" \
  --notes-file "/tmp/gh-release-${TAG}.md"

# Verify
gh release view "$TAG"
```

If `CHANGELOG.md` is missing a section, write the notes file from the git log
summary, then **backfill `CHANGELOG.md`** and commit it.

#### 5d. Smoke-check the published bits

```bash
# npm (optional, needs network)
npm exec --yes openplanet-lsp@${VERSION} -- --version

# Or install a GH asset and run --version
```

### 6. Done checklist

- [ ] Tag `vX.Y.Z` pushed
- [ ] `release.yml` green
- [ ] GitHub Release has **all** platform archives
- [ ] GitHub Release **body** is human changelog (not only auto notes)
- [ ] `CHANGELOG.md` has matching section (committed)
- [ ] `npm view openplanet-lsp version` == `X.Y.Z`
- [ ] Platform package(s) visible on npm for this version
- [ ] This `RELEASE.md` still matches reality (update if workflow/layout changed)

---

## Dry run (no publish)

Build/pack only; skips GitHub upload and `npm publish`:

```bash
gh workflow run release.yml -f dry_run=true
gh run watch
```

Use this to validate the matrix before a real tag when the workflow changed.

---

## Local smoke (current host only)

```bash
./scripts/release/smoke-local.sh
```

Builds `--release`, verifies source manifest lockstep, stages and packs the host
platform + meta npm tarballs in a temporary directory, then runs
`openplanet-lsp --version` through the Node launcher. It does not rewrite or
stage files in the source npm package directories.

---

## Install (end users)

```bash
npm install -g openplanet-lsp
openplanet-lsp --version
openplanet-lsp check /path/to/plugin
```

Or download a GitHub Release archive and put the binary on `PATH`.

---

## Layout (keep in sync with the tree)

```
RELEASE.md                        # this procedure (update when release process changes)
CHANGELOG.md                      # user-facing history (update every release)
npm/
  openplanet-lsp/                 # meta package (bin launcher)
  openplanet-lsp-linux-x64/       # native binary packages (binary filled in CI)
  openplanet-lsp-linux-arm64/
  openplanet-lsp-darwin-x64/
  openplanet-lsp-darwin-arm64/
  openplanet-lsp-win32-x64/
  openplanet-lsp-win32-arm64/
scripts/release/
  bump-version.sh                 # Cargo.toml + all npm versions
  smoke-local.sh                  # host-only pack + run
.github/workflows/
  ci.yml                          # PR/main tests + npm manifest smoke
  release.yml                     # tag → binaries + GH release + npm publish
```

---

## Agent responsibilities (summary)

When asked to cut a release, agents must:

1. Bump versions with `scripts/release/bump-version.sh`
2. **Write/update `CHANGELOG.md`** from real git history (not empty stubs)
3. Commit, tag `vX.Y.Z`, push tag
4. **Wait for `release.yml` success**
5. **Update the GitHub Release** with `gh release edit` so notes match the changelog
6. Verify assets + npm publish
7. **Edit this `RELEASE.md`** in the same change set whenever the procedure or tooling changes

Do **not** treat CI’s auto-generated release notes as the finished product.

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Release job: Cargo version ≠ tag | `bump-version.sh`, retag / new patch |
| `npm publish` ENEEDAUTH / 401 | Trusted publisher missing/wrong: `npm trust list <pkg>`; workflow file must be `release.yml`; repo `clankercode/lsp-openplanet`; job needs `id-token: write` + Node 24 |
| Optional platform package missing after install | Install `openplanet-lsp-<platform>@x.y.z` or use GH binary |
| `windows-11-arm` runner unavailable | Adjust matrix in `release.yml` **and this doc** |
| Release body still auto-generated only | You skipped step 5 — run `gh release edit` |
| Changelog empty / “various fixes” only | Expand from `git log` since previous tag; be specific |

---

## First publish checklist

- [ ] Packages claimed on npm (one-time maintainer publish)
- [ ] Trusted publishers set (`npm trust github … --file release.yml --repo clankercode/lsp-openplanet --allow-publish`)
- [ ] `CHANGELOG.md` created with first version section
- [ ] `Cargo.toml` version matches intended tag
- [ ] `./scripts/release/smoke-local.sh` passes
- [ ] Tag pushed; `release.yml` green
- [ ] GitHub Release notes updated via `gh release edit`
- [ ] `npm view openplanet-lsp version` correct
- [ ] No `NPM_TOKEN` secret required (OIDC only)
