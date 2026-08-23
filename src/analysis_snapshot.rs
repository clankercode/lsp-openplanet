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

use std::collections::{HashMap, HashSet};
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

/// Address a snapshot file by path or by URI — input to
/// [`AnalysisSnapshot::table_for_own_file`].
pub enum FileRef<'a> {
    Path(&'a std::path::Path),
    Uri(&'a Url),
}

/// The workspace-level analysis view (see module docs).
pub struct AnalysisSnapshot {
    files: Vec<SnapshotFile>,
    symbols: SymbolTable,
    missing_required_dependencies: Vec<String>,
    /// Canonicalized paths of this plugin's own info.toml `[script]`
    /// exports + shared_exports files. Declarations inside them are
    /// public API — exempt from unused-declaration warnings. Empty for
    /// in-memory snapshots (no manifest to read).
    export_paths: std::collections::HashSet<PathBuf>,
    /// Strict `[script]` EXPORTS only (no shared_exports). Files here
    /// compile into CONSUMERS only — their declarations do not resolve
    /// in the defining plugin's own module (game parity 2026-08-23).
    strict_export_paths: std::collections::HashSet<PathBuf>,
    /// Cached own-module view (see `own_module_symbols`). Built eagerly
    /// in `from_load` when strict exports exist.
    own_module_symbols: Option<SymbolTable>,
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
        // GH #58: the pool is the plugin's complete module closure only
        // when the loader walked a real root and every declared dependency
        // resolved (optional and transitive export_dependencies included).
        // Diagnostics that read the absence of a name as a game-compiler
        // rejection gate on this — an in-memory pool, or the
        // open-documents-only fallback the server builds when the disk load
        // failed, never claims it.
        if load.dependency_closure_complete {
            table.mark_dependency_closure_complete();
        }
        // Own-module view for diagnostics: identical pool except strict
        // export files hidden from name resolution (game parity
        // 2026-08-23 — export decls are consumer-only).
        let strict_ids: HashSet<usize> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                let canonical = f.path.canonicalize().unwrap_or_else(|_| f.path.clone());
                load.strict_export_paths.contains(&canonical)
            })
            .map(|(i, _)| i)
            .collect();
        let own_module_symbols = if strict_ids.is_empty() {
            None
        } else {
            let mut own = table.clone();
            for fid in strict_ids {
                own.hide_file(fid);
            }
            Some(own)
        };
        Self {
            files,
            symbols: table,
            missing_required_dependencies: load.missing_required_dependencies.clone(),
            export_paths: load.export_paths.clone(),
            strict_export_paths: load.strict_export_paths.clone(),
            own_module_symbols,
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
            dependency_closure_complete: false,
            export_paths: HashSet::new(),
            strict_export_paths: HashSet::new(),
        };
        Self::from_load(&load, config)
    }

    /// Like [`Self::from_files`], but with an explicit `report_diagnostics`
    /// flag per file (test surface for dependency-export behaviour).
    pub fn from_files_with_flags(files: &[(PathBuf, String, bool)], config: &LspConfig) -> Self {
        let load = PluginWorkspaceLoad {
            root: PathBuf::new(),
            files: files
                .iter()
                .map(|(p, s, report)| WorkspaceSourceFile {
                    path: p.clone(),
                    source: s.clone(),
                    report_diagnostics: *report,
                })
                .collect(),
            missing_required_dependencies: Vec::new(),
            dependency_closure_complete: false,
            export_paths: Default::default(),
            strict_export_paths: Default::default(),
        };
        Self::from_load(&load, config)
    }

    /// Pooled workspace symbols (file_id == index into `files()`).
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    /// The defining plugin's OWN-module symbol view: pooled symbols with
    /// strict `[script]` exports files hidden. Game model: a strict
    /// export file is compiled into consumers only, so its declarations
    /// do not resolve in the defining plugin's own module (bodies there
    /// = game "No matching symbol"). Shared exports stay visible (the
    /// game compiles them into the defining plugin too). `None` when
    /// nothing is strict-exported (identical view — callers fall back
    /// to `symbols()`). Built eagerly in `from_load`.
    pub fn own_module_symbols(&self) -> Option<&SymbolTable> {
        self.own_module_symbols.as_ref()
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
        self.file_entry_of(uri).map(|f| &f.analysis)
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

    /// Canonicalized own-manifest export file paths (public API — see
    /// field docs). Populated by `from_load` from the workspace loader.
    pub fn export_paths(&self) -> &std::collections::HashSet<PathBuf> {
        &self.export_paths
    }

    /// Strict own-manifest EXPORTS paths (no shared_exports) — decls in
    /// these files are consumer-only and hidden from the defining
    /// plugin's own module view (game parity 2026-08-23).
    pub fn strict_export_paths(&self) -> &std::collections::HashSet<PathBuf> {
        &self.strict_export_paths
    }

    /// The symbol table to use when CHECKING one of this plugin's own
    /// files: the own-module view (strict export decls hidden) unless
    /// `path` is itself a strict export file — a file always sees its
    /// own declarations, so an un-hidden copy is built in that case.
    /// Falls back to the full pooled table when nothing is
    /// strict-exported. Shared by the CLI check loop and the server's
    /// diagnostics publish path; owns the un-hidden copy when one is
    /// needed so callers don't duplicate the dance.
    pub fn table_for_own_file(
        &self,
        path_or_uri: FileRef<'_>,
    ) -> std::borrow::Cow<'_, SymbolTable> {
        let Some(own) = self.own_module_symbols.as_ref() else {
            return std::borrow::Cow::Borrowed(&self.symbols);
        };
        let fid = match path_or_uri {
            FileRef::Path(p) => self.file_id_of_path(p),
            FileRef::Uri(u) => self.file_id_of(u),
        };
        match fid {
            Some(fid) if own.is_file_hidden(fid) => {
                let mut unhidden = own.clone();
                unhidden.unhide_file(fid);
                std::borrow::Cow::Owned(unhidden)
            }
            _ => std::borrow::Cow::Borrowed(own),
        }
    }

    /// file_id (index into `files()`) of the file with this URI, when
    /// present in the snapshot.
    pub fn file_id_of(&self, uri: &Url) -> Option<usize> {
        self.files
            .iter()
            .position(|f| f.uri.as_ref() == Some(uri))
            .or_else(|| {
                let canon = canonical_file_path(uri)?;
                self.files
                    .iter()
                    .position(|f| f.path.canonicalize().map(|p| p == canon).unwrap_or(false))
            })
    }

    /// file_id of the file with this path, when present.
    pub fn file_id_of_path(&self, path: &std::path::Path) -> Option<usize> {
        self.files.iter().position(|f| f.path == path)
    }

    /// Snapshot file entry for `uri`: exact URI match first, then a
    /// canonical-path fallback so documents opened through a symlinked
    /// path (e.g. dips++-all) resolve to the same file the disk load
    /// found via the canonical plugin root.
    fn file_entry_of(&self, uri: &Url) -> Option<&SnapshotFile> {
        self.files
            .iter()
            .find(|f| f.uri.as_ref() == Some(uri))
            .or_else(|| {
                let canon = canonical_file_path(uri)?;
                self.files
                    .iter()
                    .find(|f| f.path.canonicalize().map(|p| p == canon).unwrap_or(false))
            })
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
        let analysis = &self.file_entry_of(uri)?.analysis;
        let mut checker = Checker::new(analysis.masked_source(), scope).with_type_recording();
        checker.check_file(&analysis.file);
        Some(CheckedFile {
            analysis,
            checker,
            diagnostics: Vec::new(),
        })
    }
}

/// Canonicalized file path for a `file://` URI (None for non-file URIs or
/// unresolvable paths).
fn canonical_file_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()?.canonicalize().ok()
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

    #[cfg(unix)]
    #[test]
    fn symlink_alias_uri_resolves_to_same_file() {
        // dips++-all layout: snapshot paths come from the canonical
        // plugin root, but the client opens documents through a symlink.
        // analysis_of/file_id_of must fall back to canonical-path match.
        let dir = std::env::temp_dir().join(format!("ols-snap-symlink-{}", std::process::id()));
        let real_dir = dir.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_path = real_dir.join("c.as");
        std::fs::write(&real_path, "int x;").unwrap();
        let link_dir = dir.join("linked");
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

        let snap = AnalysisSnapshot::from_files(
            &[(real_path.canonicalize().unwrap(), "int x;".to_string())],
            &LspConfig::default(),
        );
        let alias_uri = Url::from_file_path(link_dir.join("c.as")).unwrap();
        assert!(snap.analysis_of(&alias_uri).is_some());
        assert_eq!(snap.file_id_of(&alias_uri), Some(0));
        let symbols = snap.symbols();
        let scope = crate::typecheck::global_scope::GlobalScope::new(symbols, None);
        assert!(snap.checked_file(&alias_uri, &scope).is_some());
        let _ = std::fs::remove_dir_all(&dir);
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
