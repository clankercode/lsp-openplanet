# openplanet-lsp-tui

Watch-mode diagnostics TUI used by [`openplanet-lsp`](https://crates.io/crates/openplanet-lsp)
(`check --watch`). Host adapters implement `TuiDataSource`; this crate owns the
ratatui UI and event loop.

Normally you depend on `openplanet-lsp` rather than this crate directly.
