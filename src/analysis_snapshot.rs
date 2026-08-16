//! The workspace-level analysis view: one [`DocumentAnalysis`] per file, the
//! pooled [`SymbolTable`], the file_id↔URI map, and the missing-required-
//! dependency report — built once per workspace state, read by every LSP
//! feature and by CLI `check`.
//!
//! Design notes (see `CONTEXT.md` and GH issue #39):
//!
//! * **Parsed, not checked.** Diagnostics are queries *against* a snapshot
//!   (`server::diagnostics::compute_diagnostics_from_analysis`), never
//!   stored in it. The checker stays file-local.
//! * **Absorbs parse+pool, not disk I/O.** `workspace::load` owns the disk
//!   walk + open-document overlay; the snapshot's implementation is one
//!   parse per file + symbol pooling + the file_id map. `file_id == index`
//!   is an invariant owned by construction here, not a comment invariant.
//! * **Rebuilt per document lifecycle event** (`did_open` / `did_change` /
//!   `did_close`). Requests between rebuilds observe one consistent view.
//! * **Two adapters.** The LSP `Backend` and CLI `run_check` build and read
//!   the same interface.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tower_lsp::lsp_types::Url;

use crate::analysis::DocumentAnalysis;
use crate::config::LspConfig;
use crate::symbols::SymbolTable;
use crate::typecheck::checker::Checker;
use crate::typecheck::GlobalScope;
use crate::workspace::load::{PluginWorkspaceLoad, WorkspaceSourceFile};

/// One file's contribution to the snapshot: its path, URI, parsed
/// analysis, and whether diagnostics should be reported for it (false for
/// dependency export files).
pub struct SnapshotFile {
    pub path: PathBuf,
    pub uri: Option<Url>,
    pub analysis: DocumentAnalysis,
    pub report_diagnostics: bool,
}

/// The workspace-level analysis view (see module docs).
pub struct AnalysisSnapshot {
    files: Vec<SnapshotFile>,
    symbols: SymbolTable,
    missing_required_dependencies: Vec<String>,
}

impl AnalysisSnapshot {
    /// Build a snapshot from a merged workspace load (disk files with the
    /// open-document overlay already applied via
    /// [`crate::workspace::load::merge_open_documents`]).
    pub fn from_load(load: &PluginWorkspaceLoad, config: &LspConfig) -> Self {
        let mut files = Vec::with_capacity(load.files.len());
        let mut table = SymbolTable::new();
        for (fid, wf) in load.files.iter().enumerate() {
            let analysis = DocumentAnalysis::analyze(&wf.source, &config.defines);
            let syms = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
            table.set_file_symbols(fid, syms);
            files.push(SnapshotFile {
                path: wf.path.clone(),
                uri: Url::from_file_path(&wf.path).ok(),
                analysis,
                report_diagnostics: wf.report_diagnostics,
            });
        }
        Self {
            files,
            symbols: table,
            missing_required_dependencies: load.missing_required_dependencies.clone(),
        }
    }

    /// Build an in-memory snapshot from plain `(path, source)` pairs —
    /// the test surface, and the fallback when no workspace root is set
    /// (open documents only).
    pub fn from_files(files: &[(PathBuf, String)], config: &LspConfig) -> Self {
        let load = PluginWorkspaceLoad {
            root: PathBuf::new(),
            files: files
                .iter()
                .map(|(p, s)| WorkspaceSourceFile {
                    path: p.clone(),
                    source: s.clone(),
                    report_diagnostics: true,
                })
                .collect(),
            missing_required_dependencies: Vec::new(),
        };
        Self::from_load(&load, config)
    }

    /// Pooled workspace symbols (file_id == index into `files()`).
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    /// Files in the snapshot, in file_id order.
    pub fn files(&self) -> &[SnapshotFile] {
        &self.files
    }

    /// file_id ↔ URI map in the shape navigation/call-hierarchy expect:
    /// `file_id → (uri, analysis)` for files that have a URI.
    pub fn uri_map(&self) -> HashMap<usize, (Url, &DocumentAnalysis)> {
        self.files
            .iter()
            .enumerate()
            .filter_map(|(fid, f)| f.uri.as_ref().map(|uri| (fid, (uri.clone(), &f.analysis))))
            .collect()
    }

    /// Analysis for one file, looked up by URI.
    pub fn analysis_of(&self, uri: &Url) -> Option<&DocumentAnalysis> {
        self.files
            .iter()
            .find(|f| f.uri.as_ref() == Some(uri))
            .map(|f| &f.analysis)
    }

    /// Analysis for one file, looked up by path.
    pub fn analysis_at_path(&self, path: &std::path::Path) -> Option<&DocumentAnalysis> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| &f.analysis)
    }

    /// Required dependencies that no plugins dir resolved.
    pub fn missing_required_dependencies(&self) -> &[String] {
        &self.missing_required_dependencies
    }

    /// The checked view of one file (GH #42): run the checker with type
    /// recording over the snapshot's parse, against the pooled workspace
    /// symbols. Built on demand — diagnostics requests and hover queries
    /// pay for exactly the check pass they need, once per snapshot
    /// generation (the result is not cached; callers holding the snapshot
    /// Arc across a rebuild get a fresh view through the new snapshot).
    ///
    /// `type_index` is the external typedb, when loaded.
    pub fn checked_file<'s>(
        &'s self,
        uri: &Url,
        scope: &'s GlobalScope<'s>,
    ) -> Option<CheckedFile<'s>> {
        let analysis = &self
            .files
            .iter()
            .find(|f| f.uri.as_ref() == Some(uri))?
            .analysis;
        let mut checker = Checker::new(analysis.masked_source(), scope).with_type_recording();
        checker.check_file(&analysis.file);
        Some(CheckedFile {
            analysis,
            checker,
            diagnostics: Vec::new(),
        })
    }
}

/// One file's checked view: the parse it was checked against plus the
/// checker's span→type recording (GH #42). Diagnostics are exposed via
/// [`CheckedFile::take_diagnostics`] so hover-style consumers can ignore
/// them without the checker growing a "no diagnostics" mode.
pub struct CheckedFile<'a> {
    /// The parse the check ran against (masked source + AST).
    pub analysis: &'a DocumentAnalysis,
    checker: Checker<'a>,
    diagnostics: Vec<crate::typecheck::TypeDiagnostic>,
}

impl<'a> CheckedFile<'a> {
    /// Type of the innermost recorded expression containing `offset`
    /// (GH #42). `None` when the offset is not inside any recorded
    /// expression.
    pub fn type_at_offset(&self, offset: u32) -> Option<&crate::typecheck::repr::TypeRepr> {
        self.checker.type_at_offset(offset)
    }

    /// Type recorded for the expression spanning exactly `start..end`.
    pub fn type_at_span_range(
        &self,
        start: u32,
        end: u32,
    ) -> Option<&crate::typecheck::repr::TypeRepr> {
        self.checker.type_at_span_range(start, end)
    }

    /// Drain the checker's diagnostics (GH #39 keeps diagnostics as a
    /// query, not snapshot state — this is the query).
    pub fn take_diagnostics(&mut self) -> Vec<crate::typecheck::TypeDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Borrow the underlying checker (test surface).
    pub fn checker(&self) -> &Checker<'a> {
        &self.checker
    }
}

/// Convenience alias for call-sites that share one snapshot.
pub type SharedSnapshot = Arc<AnalysisSnapshot>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_file_ids_are_indices() {
        let snap = AnalysisSnapshot::from_files(
            &[
                (PathBuf::from("a.as"), "class Foo {}".to_string()),
                (PathBuf::from("b.as"), "void main() { Foo x; }".to_string()),
            ],
            &LspConfig::default(),
        );
        let symbols = snap.symbols();
        // file_id 0 owns Foo; file_id 1 owns main.
        assert!(symbols.lookup("Foo").iter().any(|s| s.file_id == 0));
        assert!(symbols.lookup("main").iter().any(|s| s.file_id == 1));
        assert_eq!(snap.files().len(), 2);
        assert!(snap.missing_required_dependencies().is_empty());
    }

    #[test]
    fn uri_map_covers_files_with_uris() {
        let dir = std::env::temp_dir().join("ols-snap-uri-map-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.as");
        std::fs::write(&path, "int x;").unwrap();
        let snap =
            AnalysisSnapshot::from_files(&[(path, "int x;".to_string())], &LspConfig::default());
        let map = snap.uri_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map[&0].1.masked_source(), "int x;");
        let uri = map[&0].0.clone();
        assert!(snap.analysis_of(&uri).is_some());
    }

    #[test]
    fn checked_file_records_types_for_queries() {
        // GH #42: the checked view exposes span→type queries over the
        // snapshot's parse.
        let dir = std::env::temp_dir().join("ols-snap-checked-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.as");
        let src = "void main() { int x = 1; int y = x; }";
        std::fs::write(&path, src).unwrap();
        let snap = AnalysisSnapshot::from_files(&[(path, src.to_string())], &LspConfig::default());
        let uri = snap.uri_map()[&0].0.clone();
        let symbols = snap.symbols();
        let scope = GlobalScope::new(symbols, None);
        let checked = snap.checked_file(&uri, &scope).expect("checked view");
        // `1` (the initializer of x) sits at offset 22.
        let ty = checked.type_at_offset(22).expect("1 recorded");
        assert_eq!(ty.display(), "int");
        // The use of `x` in `int y = x` sits at offset 33.
        let ty = checked.type_at_offset(33).expect("x recorded");
        assert_eq!(ty.display(), "int");
    }
}
