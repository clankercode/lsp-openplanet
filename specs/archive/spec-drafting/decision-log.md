# Decision Log

| ID | Decision | Rationale | Source | Impacted Sections | Status |
|----|----------|-----------|--------|-------------------|--------|
| D-001 | Implementation language: Rust | Fast, accurate, low memory/CPU. Principled fresh design. | Confirmed (user) | 2, 9, 11 | active |
| D-002 | Fresh build, not a fork | Existing VS Code LSP is "algorithmically bad". Reference only. | Confirmed (user) | 2, 9 | active |
| D-003 | Generic stdio LSP transport | Editor-agnostic. Works with VS Code, Neovim, Helix, etc. | Confirmed (user) | 2, 9, 10 | active |
| D-004 | Real plugin fixtures; all errors accounted for | User requirement: no silent diagnostic misses | Confirmed (user) | 7, 13 | active |
| D-005 | Primary target TMNEXT; game target swappable | Type JSON files share format across games. Preprocessor #if is the hard part. | Confirmed (user) | 2, 5, 6 | active |
| D-006 | Web Services API out of scope | No native OpenPlanet AngelScript support for it | Confirmed (user) | 2 | active |
| D-007 | info.toml parsed with diagnostic feedback | LSP validates TOML structure, fields, dependencies | Confirmed (user) | 7, 9 | active |
| D-008 | Layered configuration: auto-detect → config file → init params | Maximum flexibility; standard paths for zero-config in common case | Confirmed (user) | 4, 9 | active |
| D-009 | Default defines: all game targets + MANIA64 + WINDOWS + SIG_DEVELOPER | Most permissive set; all code branches visible during development | Confirmed (user) | 4, 6 | active |
