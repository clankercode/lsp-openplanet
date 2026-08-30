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
    /// GH #61: own-module `.as` files the checked set omitted (gitignored,
    /// or filtered by `source_paths`/`ignore_paths`), as raw
    /// `(path, source)` — the game compiles them, so the
    /// unused-declaration pass records usage from them. Deliberately NOT
    /// parsed here and NOT in `files`: they must contribute no
    /// declarations and no diagnostics, and only the unused pass needs
    /// them, so it pays the parse (`WorkspaceUsage::collect`) rather than
    /// every snapshot rebuild.
    usage_only_sources: Vec<(PathBuf, String)>,
    /// Omitted paths that discovery found but the best-effort read could not
    /// open. They still need usage-only identity so an LSP URI never falls
    /// through to arbitrary-unsaved-buffer diagnostics.
    usage_only_unreadable_paths: Vec<PathBuf>,
    /// True when every omitted source above was readable. This controls
    /// only declaration-level unused warnings; locals/parameters remain
    /// sound because their usage is function-local.
    usage_only_sources_complete: bool,
    /// The `#if` define set these files were parsed with, so the
    /// usage-only sources above can be parsed on demand with the same
    /// masking the checked files got.
    defines: HashSet<String>,
    /// GH #56: `unused_callbacks` from the config ladder — extra names
    /// the unused-declaration pass treats as runtime-invoked callbacks.
    /// Carried here for the same reason as `export_paths`: it is a
    /// config-derived policy input the unused analysis needs, and routing
    /// it through the snapshot keeps every caller's signature unchanged.
    unused_callbacks: Vec<String>,
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
        // GH #58: carry the loader's verdict on whether these sources are
        // the plugin's complete compile surface. The conditions behind it are
        // enumerated on `PluginWorkspaceLoad::dependency_closure_complete` —
        // this is a pass-through, not a second place to decide. An in-memory
        // pool and the server's open-documents-only fallback never claim it.
        // B019: an uninstalled OPTIONAL dependency no longer breaks the
        // closure, because its `#if DEPENDENCY_X` code is masked out for us
        // exactly as it is for the game. That equivalence holds only while
        // nothing force-defines the macro anyway — a `define_matrix` axis or
        // an explicit `defines` entry deliberately can (both are supported:
        // the plugin ships to users who DO have X). When one does, the
        // guarded code is live against a module we do not have, so the
        // pooled table is NOT the complete compile surface and must not
        // claim authority.
        let forced_absent_dependency = load
            .unresolved_optional_dependencies
            .iter()
            .filter_map(|dep| crate::config::dependency_define_name(dep))
            .any(|name| config.defines.contains(&name));
        if load.dependency_closure_complete && !forced_absent_dependency {
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
            usage_only_sources: load.usage_only_sources.clone(),
            usage_only_unreadable_paths: load.usage_only_unreadable_paths.clone(),
            usage_only_sources_complete: load.usage_only_sources_complete,
            defines: config.defines.clone(),
            unused_callbacks: config.unused_callbacks.clone(),
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
            unresolved_optional_dependencies: Vec::new(),
            usage_only_sources: Vec::new(),
            usage_only_sources_complete: true,
            usage_only_unreadable_paths: Vec::new(),
            export_paths: HashSet::new(),
            strict_export_paths: HashSet::new(),
        };
        Self::from_load(&load, config)
    }

    /// Like [`Self::from_files`], but with own-module sources that were
    /// OMITTED from the checked set (GH #61) — gitignored, or filtered out
    /// by `source_paths`/`ignore_paths`. They contribute usage only: no
    /// pooled declarations, no diagnostics, no file_id. The in-memory
    /// surface for the disk behaviour `load_plugin_workspace` builds.
    pub fn from_files_with_usage_only(
        files: &[(PathBuf, String)],
        usage_only: &[(PathBuf, String)],
        config: &LspConfig,
    ) -> Self {
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
            unresolved_optional_dependencies: Vec::new(),
            usage_only_sources: usage_only.to_vec(),
            usage_only_sources_complete: true,
            usage_only_unreadable_paths: Vec::new(),
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
            unresolved_optional_dependencies: Vec::new(),
            usage_only_sources: Vec::new(),
            usage_only_sources_complete: true,
            usage_only_unreadable_paths: Vec::new(),
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

    /// GH #56: extra callback-name patterns from the config ladder.
    pub fn unused_callbacks(&self) -> &[String] {
        &self.unused_callbacks
    }

    /// GH #61: own-module sources omitted from the checked set, for usage
    /// recording only (see the field docs). Not parsed — the caller parses
    /// with [`Self::defines`].
    pub fn usage_only_sources(&self) -> &[(PathBuf, String)] {
        &self.usage_only_sources
    }

    /// Whether `uri` identifies an omitted own-module source. Readable and
    /// unreadable omissions both count: neither may gain diagnostics, a
    /// file ID, or pooled declarations merely because the editor opens it.
    /// Exact paths cover raced/nonexistent files; canonical equality covers
    /// symlink aliases while both paths remain resolvable.
    pub fn is_usage_only_uri(&self, uri: &Url) -> bool {
        let Ok(path) = uri.to_file_path() else {
            return false;
        };
        self.is_usage_only_path(&path)
    }

    /// Path form of [`Self::is_usage_only_uri`], shared by callers that have
    /// already decoded a file URI.
    pub fn is_usage_only_path(&self, path: &std::path::Path) -> bool {
        self.usage_only_sources
            .iter()
            .any(|(omitted, _)| paths_equal(omitted, path))
            || self
                .usage_only_unreadable_paths
                .iter()
                .any(|omitted| paths_equal(omitted, path))
    }

    /// Whether every omitted own-module source was readable when the
    /// workspace load was built.
    pub fn usage_only_sources_complete(&self) -> bool {
        self.usage_only_sources_complete
    }

    /// The `#if` define set this snapshot's files were parsed with.
    pub fn defines(&self) -> &HashSet<String> {
        &self.defines
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

    /// Like [`Self::uri_map`], but only this plugin's OWN files.
    ///
    /// A snapshot also holds the dependency plugins' export files, so they
    /// can resolve — `report_diagnostics` is false for those, which is the
    /// same gate the unused-declaration analysis uses to avoid warning
    /// about someone else's code. `textDocument/rename` needs it because a
    /// `WorkspaceEdit` naming a dependency's file would have the editor
    /// rewrite ANOTHER plugin's source on disk (B049).
    pub fn own_uri_map(&self) -> HashMap<usize, (Url, &DocumentAnalysis)> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.report_diagnostics)
            .filter_map(|(fid, f)| f.uri.as_ref().map(|uri| (fid, (uri.clone(), &f.analysis))))
            .collect()
    }

    /// True when `fid` is one of this plugin's strict `[script] exports`
    /// files — compiled into CONSUMERS only.
    fn is_strict_export_file(&self, fid: usize) -> bool {
        let Some(f) = self.files.get(fid) else {
            return false;
        };
        let canonical = f.path.canonicalize().unwrap_or_else(|_| f.path.clone());
        self.strict_export_paths.contains(&canonical)
    }

    /// The own files a rename / find-references request originating in
    /// `from` may look at — its MODULE, not the whole plugin.
    ///
    /// A plugin with strict `[script] exports` compiles as two separate
    /// units, and the live probe (2026-08-30) pins both halves:
    ///
    /// * The own module cannot see an export file's declarations at all —
    ///   a call to one is `No matching symbol`, an unresolved name or even
    ///   a syntax error inside an export file leaves the plugin compiling
    ///   and running, and the same spelling may be declared on BOTH sides
    ///   with no collision. So the two sides' same-named symbols are
    ///   DIFFERENT symbols and a rename must never cross between them.
    /// * The export files compile together into a consumer, unqualified: a
    ///   consumer plugin called `AFn` and `BFn` from two different export
    ///   files, and `Exports_B.as`'s call to `Exports_A.as`'s `AFn` ran.
    ///   So a rename from inside one export file must reach its siblings.
    ///
    /// With no strict exports there is one module, and this is
    /// [`Self::own_uri_map`].
    pub fn own_uri_map_for_module_of(
        &self,
        from: FileRef<'_>,
    ) -> HashMap<usize, (Url, &DocumentAnalysis)> {
        let map = self.own_uri_map();
        if self.strict_export_paths.is_empty() {
            return map;
        }
        let from_export = match from {
            FileRef::Path(p) => self.file_id_of_path(p),
            FileRef::Uri(u) => self.file_id_of(u),
        }
        .is_some_and(|fid| self.is_strict_export_file(fid));
        map.into_iter()
            .filter(|(fid, _)| self.is_strict_export_file(*fid) == from_export)
            .collect()
    }

    /// The symbol view for the same request: the module `from` belongs to.
    /// Everything outside it is hidden, so a same-named symbol on the
    /// other side of the export boundary cannot be mistaken for this one.
    /// See [`Self::own_uri_map_for_module_of`] for the game evidence.
    pub fn table_for_module_of(&self, from: FileRef<'_>) -> std::borrow::Cow<'_, SymbolTable> {
        if self.strict_export_paths.is_empty() {
            return std::borrow::Cow::Borrowed(&self.symbols);
        }
        let from_export = match from {
            FileRef::Path(p) => self.file_id_of_path(p),
            FileRef::Uri(u) => self.file_id_of(u),
        }
        .is_some_and(|fid| self.is_strict_export_file(fid));
        if !from_export {
            // The own-module view already hides every strict export.
            return match self.own_module_symbols.as_ref() {
                Some(own) => std::borrow::Cow::Borrowed(own),
                None => std::borrow::Cow::Borrowed(&self.symbols),
            };
        }
        // The export module: hide everything that is NOT a strict export.
        let mut export_only = self.symbols.clone();
        for fid in 0..self.files.len() {
            if !self.is_strict_export_file(fid) {
                export_only.hide_file(fid);
            }
        }
        std::borrow::Cow::Owned(export_only)
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

fn paths_equal(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
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
    fn usage_only_uri_includes_unreadable_omitted_paths() {
        let dir = std::env::temp_dir().join(format!(
            "ols-snap-usage-only-unreadable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let unreadable = dir.join("raced-away.as");
        let load = crate::workspace::load::PluginWorkspaceLoad {
            root: dir.clone(),
            files: Vec::new(),
            missing_required_dependencies: Vec::new(),
            dependency_closure_complete: false,
            unresolved_optional_dependencies: Vec::new(),
            usage_only_sources: Vec::new(),
            usage_only_sources_complete: false,
            usage_only_unreadable_paths: vec![unreadable.clone()],
            export_paths: Default::default(),
            strict_export_paths: Default::default(),
        };
        let snapshot = AnalysisSnapshot::from_load(&load, &LspConfig::default());
        let uri = Url::from_file_path(&unreadable).unwrap();
        assert!(snapshot.is_usage_only_uri(&uri));
        assert!(!snapshot.is_usage_only_uri(&Url::from_file_path(dir.join("new.as")).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn usage_only_uri_accepts_a_symlink_alias() {
        let dir =
            std::env::temp_dir().join(format!("ols-snap-usage-only-alias-{}", std::process::id()));
        let real_dir = dir.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_path = real_dir.join("generated.as");
        std::fs::write(&real_path, "void Generated() {}\n").unwrap();
        let alias_dir = dir.join("alias");
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();
        let load = crate::workspace::load::PluginWorkspaceLoad {
            root: real_dir,
            files: Vec::new(),
            missing_required_dependencies: Vec::new(),
            dependency_closure_complete: false,
            unresolved_optional_dependencies: Vec::new(),
            usage_only_sources: vec![(
                real_path.canonicalize().unwrap(),
                "void Generated() {}\n".into(),
            )],
            usage_only_sources_complete: true,
            usage_only_unreadable_paths: Vec::new(),
            export_paths: Default::default(),
            strict_export_paths: Default::default(),
        };
        let snapshot = AnalysisSnapshot::from_load(&load, &LspConfig::default());
        let alias_uri = Url::from_file_path(alias_dir.join("generated.as")).unwrap();
        assert!(snapshot.is_usage_only_uri(&alias_uri));
        let _ = std::fs::remove_dir_all(&dir);
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
    /// Helper: a throwaway plugin root on disk. `script` is appended to
    /// `info.toml` verbatim (empty = no `[script]` section).
    fn temp_plugin(name: &str, script: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ols-snap-closure-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("info.toml"),
            format!("[meta]\nname = \"{name}\"\nversion = \"0.1.0\"\n{script}"),
        )
        .unwrap();
        for (rel, src) in files {
            std::fs::write(root.join(rel), src).unwrap();
        }
        root
    }

    fn load_of(root: &std::path::Path) -> crate::workspace::load::PluginWorkspaceLoad {
        crate::workspace::load::load_plugin_workspace(
            root,
            &crate::workspace::load::DependencySearch::with_defaults(),
        )
        .expect("temp plugin loads")
    }

    /// GH #58: a real disk load with every declared dependency resolved is
    /// the complete module closure, so the pooled table carries the
    /// authority the workspace `No matching symbol` rule gates on.
    #[test]
    fn disk_load_marks_dependency_closure_complete() {
        let root = temp_plugin("Closed", "", &[("src/Main.as", "void Main() {}\n")]);
        let snap = AnalysisSnapshot::from_load(&load_of(&root), &LspConfig::default());
        assert!(
            snap.symbols().dependency_closure_complete(),
            "a dependency-free disk load is the complete closure"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B019 INVERTED this. It used to assert that an unresolved optional
    /// dependency breaks the closure, and it was right to, because
    /// `apply_manifest` defined `DEPENDENCY_<ID>` regardless — so `#if`-
    /// guarded code the game masks out stayed live in our view, referencing a
    /// module we do not have. B019 removed that cause: the macro is no longer
    /// defined, the guarded code is masked out for us too, and what remains
    /// IS the plugin's complete compile surface. The namespace rule (GH #58)
    /// therefore regains authority on plugins whose only gap is an optional
    /// dependency they do not have installed locally.
    #[test]
    fn unresolved_optional_dependency_no_longer_breaks_closure() {
        let root = temp_plugin(
            "OptDep",
            "\n[script]\noptional_dependencies = [\"ZzzNoSuchOptDep99\"]\n",
            &[("src/Main.as", "void Main() {}\n")],
        );
        let load = load_of(&root);
        assert_eq!(
            load.unresolved_optional_dependencies,
            vec!["ZzzNoSuchOptDep99".to_string()],
            "the unresolved optional dep must still be RECORDED"
        );
        let snap = AnalysisSnapshot::from_load(&load, &LspConfig::default());
        assert!(
            snap.symbols().dependency_closure_complete(),
            "an uninstalled optional dependency must NOT cost the rule its authority"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ...but the equivalence that justifies it collapses if something
    /// force-defines the macro anyway. A `define_matrix` axis (or an explicit
    /// `defines` entry) deliberately can — the plugin ships to users who DO
    /// have the dependency. In that combo the guarded code is live against a
    /// module we do not have, so the pooled table must NOT claim authority.
    #[test]
    fn force_defining_an_absent_optional_dependency_still_breaks_closure() {
        let root = temp_plugin(
            "ForcedOptDep",
            "\n[script]\noptional_dependencies = [\"ZzzNoSuchOptDep99\"]\n",
            &[("src/Main.as", "void Main() {}\n")],
        );
        let load = load_of(&root);
        let mut config = LspConfig::default();
        config
            .defines
            .insert("DEPENDENCY_ZZZNOSUCHOPTDEP99".to_string());
        let snap = AnalysisSnapshot::from_load(&load, &config);
        assert!(
            !snap.symbols().dependency_closure_complete(),
            "force-defining an absent dependency must cost the closure claim"
        );
        // Control: without the forced define the same load IS complete.
        let clean = AnalysisSnapshot::from_load(&load, &LspConfig::default());
        assert!(clean.symbols().dependency_closure_complete());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing REQUIRED dependency still breaks the closure: the game would
    /// not load the plugin at all, and the absent module may well declare the
    /// symbol we are about to call unknown. B019 must not relax this.
    #[test]
    fn unresolved_required_dependency_still_breaks_closure() {
        let root = temp_plugin(
            "ReqDep",
            "\n[script]\ndependencies = [\"ZzzNoSuchReqDep99\"]\n",
            &[("src/Main.as", "void Main() {}\n")],
        );
        let load = load_of(&root);
        assert!(
            load.unresolved_optional_dependencies.is_empty(),
            "a required dep must not land in the optional bucket"
        );
        let snap = AnalysisSnapshot::from_load(&load, &LspConfig::default());
        assert!(
            !snap.symbols().dependency_closure_complete(),
            "an unresolved REQUIRED dependency must still break the closure"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The own-module view (and the un-hidden copy built for a strict
    /// export file) must carry the closure flag — otherwise the rule would
    /// silently disarm for exactly the files that need it (GH #58).
    #[test]
    fn own_module_views_keep_the_closure_flag() {
        let root = temp_plugin(
            "Exporter",
            "\n[script]\nmodule = \"T\"\nexports = [\"Exports.as\"]\n",
            &[
                ("src/Exports.as", "import void Send() from \"T\";\n"),
                ("src/Main.as", "void Send() {}\nvoid Main() { Send(); }\n"),
            ],
        );
        let snap = AnalysisSnapshot::from_load(&load_of(&root), &LspConfig::default());
        assert!(
            snap.own_module_symbols().is_some(),
            "a strict export file must produce an own-module view"
        );
        for rel in ["src/Main.as", "src/Exports.as"] {
            let path = root.join(rel);
            let table = snap.table_for_own_file(FileRef::Path(&path));
            assert!(
                table.dependency_closure_complete(),
                "{rel}: own-module view lost the closure flag"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// In-memory snapshots (the test surface, and the server's
    /// open-documents-only fallback) pool a partial view and must never
    /// claim the closure (GH #58).
    #[test]
    fn in_memory_snapshot_is_never_closure_complete() {
        let snap = AnalysisSnapshot::from_files(
            &[(
                PathBuf::from("a.as"),
                "namespace N { void F() {} }".to_string(),
            )],
            &LspConfig::default(),
        );
        assert!(
            !snap.symbols().dependency_closure_complete(),
            "an in-memory pool must not claim the dependency closure"
        );
    }
}
