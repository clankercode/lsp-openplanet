//! Shared server-feature test surface (GH #43).
//!
//! The `tui::MockSource` pattern applied to the server: tests build a real
//! in-memory [`AnalysisSnapshot`] — the same interface production
//! (`Backend` handlers and CLI `run_check`) reads — and features are driven
//! through the same entry points (`&GlobalScope`, `WorkspaceFiles`) the
//! handlers construct. No more per-module `ws_from` / `build_workspace`
//! hand-rolls, no test-only `Option` fallbacks.
//!
//! Only compiled under `cfg(test)` (unit tests + integration harnesses that
//! include the crate's test surface).

use std::collections::HashMap;

use tower_lsp::lsp_types::{Position, Url};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::LspConfig;
use crate::typecheck::GlobalScope;

/// One in-memory workspace for a feature test: the snapshot (parse + pooled
/// symbols), plus the derived per-request views handlers build.
pub struct TestWorkspace {
    /// The real snapshot — same interface production reads.
    pub snapshot: AnalysisSnapshot,
}

impl TestWorkspace {
    /// Build a one-file workspace from source text. The file is addressable
    /// as `file:///test/<name>` (`.as` appended when `name` has no
    /// extension).
    pub fn one_file(name: &str, source: &str) -> Self {
        let file_name = if name.ends_with(".as") || name.ends_with(".toml") {
            name.to_string()
        } else {
            format!("{}.as", name)
        };
        let path = std::path::PathBuf::from("/test").join(&file_name);
        let snapshot =
            AnalysisSnapshot::from_files(&[(path, source.to_string())], &LspConfig::default());
        Self { snapshot }
    }

    /// URI of the (single) file — for `analysis_of` / `checked_file`.
    pub fn uri(&self) -> Url {
        self.snapshot.uri_map()[&0].0.clone()
    }

    /// The analysis of the single file.
    pub fn analysis(&self) -> &crate::analysis::DocumentAnalysis {
        &self.snapshot.files()[0].analysis
    }

    /// Build the `GlobalScope` the handlers construct per request.
    pub fn scope(&self) -> GlobalScope<'_> {
        GlobalScope::new(self.snapshot.symbols(), None)
    }

    /// Cross-file view map for navigation / call-hierarchy tests. Callers
    /// hold the map and build `WorkspaceFiles { files: &map }` (the map
    /// borrows the snapshot, so it can't be returned by value behind the
    /// borrow).
    pub fn uri_map(&self) -> HashMap<usize, (Url, &crate::analysis::DocumentAnalysis)> {
        self.snapshot.uri_map()
    }

    /// Convenience: hover-style position on line 0.
    pub fn pos(character: u32) -> Position {
        Position::new(0, character)
    }
}
