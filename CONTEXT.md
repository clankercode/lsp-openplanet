# CONTEXT

Domain glossary for lsp-openplanet. Shared language for architecture
reviews, issues, and PRs. Architecture vocabulary (module, interface, seam,
adapter, depth, leverage, locality) is defined separately in the
`codebase-design` skill and used as-is here.

## Analysis

- **DocumentAnalysis** — the per-file view: preprocessor output (masked
  source, defines applied), tokens, AST, parse errors. One file, one
  analysis. The seam every consumer of "this file's AST" crosses; no
  caller lexes/parses on its own.
- **AnalysisSnapshot** — the workspace-level view: the open-document
  overlay merged over the on-disk plugin workspace, one DocumentAnalysis
  per file, the pooled SymbolTable, the file_id↔URI map, and the
  missing-required-dependency report. Parsed, not checked: diagnostics
  are queries *against* a snapshot, never stored in it. Two adapters
  build/read one interface: the LSP `Backend` and CLI `check`.
- **Open-document overlay** — in-editor buffers (LSP `did_open` /
  `did_change`) that take precedence over on-disk file contents when a
  snapshot is built.
