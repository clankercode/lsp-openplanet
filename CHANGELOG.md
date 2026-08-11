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

- Multi-platform distribution: GitHub Release binaries and npm packages
  (`openplanet-lsp` + platform optional dependencies) for Linux, macOS, and
  Windows (x64 and arm64). See `RELEASE.md`.

### Fixed

- Compiler-parity diagnostics batch (B001–B007): named argument binding,
  empty catch bodies, external method arity, bare `string` param warnings,
  Nadeo undefined members when member lists are trusted, distinct enum args
  at external calls, and external named-arg binding follow-up.
