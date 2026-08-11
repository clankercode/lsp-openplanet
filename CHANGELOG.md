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
- Self-update CI/dev hooks: `OPENPLANET_LSP_VERSION`,
  `OPENPLANET_LSP_LATEST_VERSION`, `OPENPLANET_LSP_UPDATE_PACKAGE`
- `scripts/release/smoke-self-update.sh` + CI/release self-update smoke jobs

### Fixed
- Self-update status truthfulness after apply (`pending_restart` /
  `installed_version`; no longer claims “update available” post-install)
- LSP auto-check refreshes when running binary version skews vs status file
  (or after pending_restart once the new version is running)
- Windows default Node (`Program Files\\nodejs\\…`) classified as npm-global
- Atomic status file write; `%APPDATA%` / `XDG_CONFIG_HOME` config dir order
- Exit code `3` when an update exists but cannot be auto-applied

## [0.2.6] - 2026-08-12

### Fixed
- npm publish tarball paths use `./` so CLI does not treat them as git URLs

### Distribution
- Multi-platform GitHub Release + npm OIDC publish

### Added
- Self-update CLI: `openplanet-lsp update` / `update --check` / `update --status`
  - Latest version from the npm registry (no GitHub API)
  - Detects npm-global, npm-local, cargo, development, and standalone installs
  - Writes `~/.config/openplanet-lsp/update-status.json` (override via
    `OPENPLANET_LSP_CONFIG_DIR`)
  - Language server background check (≈daily) with editor info notification

## [0.2.5] - 2026-08-12

### Fixed
- Clear setup-node injected NODE_AUTH_TOKEN so npm OIDC trusted publish works

### Distribution
- Multi-platform GitHub Release + npm OIDC publish


## [0.2.4] - 2026-08-12

### Fixed
- Cross-compile darwin-x64 on macos-14 (avoid scarce macos-13 runners)

### Distribution
- Multi-platform GitHub Release + npm OIDC publish


## [0.2.3] - 2026-08-11

### Fixed
- bump-version.sh refreshes Cargo.lock so CI `--locked` builds succeed after version bumps

### Distribution
- Multi-platform GitHub Release + npm OIDC publish


## [0.2.2] - 2026-08-11

### Fixed
- Refresh Cargo.lock so release builds succeed with `--locked`

### Distribution
- Multi-platform GitHub Release + npm OIDC publish (retry of 0.2.1 pipeline)


## [0.2.1] - 2026-08-11

### Distribution
- First automated multi-platform release via GitHub Actions + npm OIDC trusted publishing
- Binaries for Linux/macOS/Windows (x64 + arm64) on GitHub Releases
- npm packages: `openplanet-lsp` + platform optional dependencies

### Fixed
- Compiler-parity batch B001–B007 and external named-arg binding (included from master)


### Added

- Multi-platform distribution: GitHub Release binaries and npm packages
  (`openplanet-lsp` + platform optional dependencies) for Linux, macOS, and
  Windows (x64 and arm64). See `RELEASE.md`.

### Fixed

- Compiler-parity diagnostics batch (B001–B007): named argument binding,
  empty catch bodies, external method arity, bare `string` param warnings,
  Nadeo undefined members when member lists are trusted, distinct enum args
  at external calls, and external named-arg binding follow-up.
