pub mod call_hierarchy;
pub mod code_actions;
pub mod completion;
pub mod diagnostics;
pub mod folding;
pub mod formatter;
pub mod highlights;
pub mod hover;
pub mod inlay_hints;
pub mod navigation;
pub mod scope_query;
pub mod semantic_tokens;
pub mod signature;
pub mod symbols;
#[cfg(test)]
pub mod test_support;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::LspConfig;
use crate::typecheck::GlobalScope;
use crate::typedb::TypeIndex;
use crate::update;

pub struct Backend {
    client: Client,
    config: tokio::sync::RwLock<LspConfig>,
    type_index: tokio::sync::RwLock<Option<Arc<TypeIndex>>>,
    /// Open document contents: URI → source text
    documents: DashMap<Url, String>,
    /// Cached analysis snapshot: rebuilt on every document lifecycle event
    /// (`did_open` / `did_change` / `did_close`). Requests between rebuilds
    /// read the same consistent view (GH #39). Single-workspace view over
    /// the sharded state below (spec: multi-root phase 2).
    snapshot: tokio::sync::RwLock<Arc<AnalysisSnapshot>>,
    workspace_root: tokio::sync::RwLock<Option<PathBuf>>,
    /// Effective plugin roots, set once at `initialize` (t2 hybrid):
    /// `config.plugin_roots` (init-option override) when non-empty, else
    /// `discover_plugin_roots` over the workspace folders (root_uri
    /// fallback), else the opened folder itself.
    plugin_roots: tokio::sync::RwLock<Vec<PathBuf>>,
    /// Sharded state (multi-root spec): one isolated workspace per effective
    /// plugin root — own config, own snapshot, own diagnostics. The legacy
    /// `snapshot` / `workspace_root` / `config` fields stay as the N=1 view;
    /// feature handlers route through `workspace_for` per request URI.
    workspaces: tokio::sync::RwLock<Vec<PluginWorkspace>>,
    /// Workspace folder paths as of the last initialize/folder-change event
    /// (t9): input for re-walking plugin roots on watched-file events.
    folder_paths: tokio::sync::RwLock<Vec<PathBuf>>,
    /// Cached initialization options (t9): re-sharding after watched-file
    /// events must re-apply the same layers (plugin_roots override, parent
    /// config base, extra plugins dirs).
    init_options: tokio::sync::RwLock<Option<serde_json::Value>>,
}

/// One isolated plugin workspace (multi-root spec: "one server, N isolated
/// plugin workspaces"). Own manifest, deps, diagnostics — no cross-plugin
/// resolution (t0); `workspace/symbol` unions across all of them (t8).
#[derive(Clone)]
pub struct PluginWorkspace {
    /// Canonicalized dir containing `info.toml`.
    pub root: PathBuf,
    /// Per-root layered config (t5 ladder applied at load time).
    pub config: LspConfig,
    /// This workspace's analysis snapshot (parse + symbol pool).
    pub snapshot: Arc<AnalysisSnapshot>,
}

impl Backend {
    /// Test/introspection accessor: `(root, snapshot)` per live shard, in
    /// shard order.
    pub async fn workspace_snapshots(&self) -> Vec<(PathBuf, Arc<AnalysisSnapshot>)> {
        self.workspaces
            .read()
            .await
            .iter()
            .map(|w| (w.root.clone(), w.snapshot.clone()))
            .collect()
    }

    /// Test/introspection accessor: `(root, config)` per live shard, in
    /// shard order.
    pub async fn workspace_configs(&self) -> Vec<(PathBuf, crate::config::LspConfig)> {
        self.workspaces
            .read()
            .await
            .iter()
            .map(|w| (w.root.clone(), w.config.clone()))
            .collect()
    }

    /// t9: rebuild the shard set from the current folder list. Reuses
    /// config+snapshot Arcs for roots whose (config-relevant) inputs are
    /// unchanged so unrelated shards keep pointer identity.
    async fn reshard_from_folders(
        &self,
        root_path: Option<&std::path::Path>,
        init_options: Option<&serde_json::Value>,
        publish_diagnostics: bool,
    ) {
        let folder_paths = self.folder_paths.read().await.clone();
        let override_roots = self.init_options.read().await.as_ref().and_then(|o| {
            let roots = LspConfig::load(None, Some(o)).plugin_roots;
            (!roots.is_empty()).then_some(roots)
        });
        let mut effective_roots = if let Some(overrides) = override_roots {
            overrides
        } else {
            let discovered = crate::workspace::roots::discover_plugin_roots(&folder_paths);
            if discovered.is_empty() {
                root_path.map(|p| p.to_path_buf()).into_iter().collect()
            } else {
                discovered
            }
        };
        effective_roots = effective_roots
            .iter()
            .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
            .collect();
        *self.plugin_roots.write().await = effective_roots.clone();

        let n_roots = effective_roots.len();
        let parent_config_base = if n_roots > 1 { root_path } else { None };
        let mut shards: Vec<PluginWorkspace> = Vec::with_capacity(effective_roots.len());
        for root in &effective_roots {
            let mut config =
                LspConfig::load_with_parent(Some(root), parent_config_base, init_options);
            if n_roots > 1 {
                if let Some(parent) = &root_path {
                    let canon = parent
                        .canonicalize()
                        .unwrap_or_else(|_| parent.to_path_buf());
                    if !config.extra_plugins_dirs.contains(&canon) {
                        config.extra_plugins_dirs.push(canon);
                    }
                }
            }
            // Reuse the existing shard when the config-relevant inputs are
            // unchanged (same root + same rendered config) — keeps Arc
            // identity for untouched siblings on re-shard.
            let existing = self
                .workspaces
                .read()
                .await
                .iter()
                .find(|w| w.root == *root)
                .cloned();
            if let Some(prev) = &existing {
                if configs_equivalent(&prev.config, &config) {
                    shards.push(prev.clone());
                    continue;
                }
            }
            let load = crate::workspace::load::DependencySearch::with_defaults()
                .finalize_with_config(&config);
            let open = self.open_documents_under(root);
            let disk = crate::workspace::load::load_plugin_workspace(root, &load);
            let merged = match disk {
                Ok(disk) => crate::workspace::load::merge_open_documents(&disk, &open),
                Err(err) => {
                    tracing::warn!("workspace load failed for {root:?}: {err}");
                    crate::workspace::load::PluginWorkspaceLoad {
                        root: root.clone(),
                        files: open_into_files(open),
                        missing_required_dependencies: Vec::new(),
                        dependency_closure_complete: false,
                        usage_only_sources: Vec::new(),
                        usage_only_sources_complete: true,
                        export_paths: HashSet::new(),
                        strict_export_paths: HashSet::new(),
                    }
                }
            };
            let snapshot = Arc::new(AnalysisSnapshot::from_load(&merged, &config));
            shards.push(PluginWorkspace {
                root: root.clone(),
                config,
                snapshot,
            });
        }
        // The legacy single-workspace view mirrors the first shard (N=1 exact
        // back-compat; N=0 stays empty).
        if let Some(first) = shards.first() {
            *self.snapshot.write().await = first.snapshot.clone();
            *self.config.write().await = first.config.clone();
        }
        *self.workspaces.write().await = shards;

        if publish_diagnostics {
            // Publish manifest diagnostics for every live shard so newly
            // added roots get their info.toml squiggles immediately.
            let live: Vec<(PathBuf, LspConfig, Arc<AnalysisSnapshot>)> = self
                .workspaces
                .read()
                .await
                .iter()
                .map(|w| (w.root.clone(), w.config.clone(), w.snapshot.clone()))
                .collect();
            for (root, config, snapshot) in live {
                let manifest_uri = Url::from_file_path(root.join("info.toml")).ok();
                if let Some(manifest_uri) = manifest_uri {
                    let source = std::fs::read_to_string(root.join("info.toml")).ok();
                    let diagnostics = source
                        .as_deref()
                        .map(|s| {
                            diagnostics::compute_manifest_diagnostics(
                                &manifest_uri,
                                s,
                                &config,
                                snapshot.missing_required_dependencies(),
                            )
                        })
                        .unwrap_or_default();
                    self.client
                        .publish_diagnostics(manifest_uri, diagnostics, None)
                        .await;
                }
            }
        }
    }
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            config: tokio::sync::RwLock::new(LspConfig::load(None, None)),
            type_index: tokio::sync::RwLock::new(None),
            documents: DashMap::new(),
            snapshot: tokio::sync::RwLock::new(Arc::new(AnalysisSnapshot::from_files(
                &[],
                &LspConfig::default(),
            ))),
            workspace_root: tokio::sync::RwLock::new(None),
            plugin_roots: tokio::sync::RwLock::new(Vec::new()),
            workspaces: tokio::sync::RwLock::new(Vec::new()),
            folder_paths: tokio::sync::RwLock::new(Vec::new()),
            init_options: tokio::sync::RwLock::new(None),
        }
    }

    /// Open documents as `(path, source)` pairs for the overlay merge.
    fn open_documents(&self) -> Vec<(PathBuf, String)> {
        self.documents
            .iter()
            .filter_map(|entry| {
                let uri = entry.key().clone();
                let source = entry.value().clone();
                uri.to_file_path().ok().map(|path| (path, source))
            })
            .collect()
    }

    /// Open documents that live under `root` (canonicalized prefix match) —
    /// the symbol-bleed guard: each shard merges only its own documents.
    fn open_documents_under(&self, root: &std::path::Path) -> Vec<(PathBuf, String)> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.open_documents()
            .into_iter()
            .filter(|(path, _)| {
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                canon.starts_with(&root)
            })
            .collect()
    }

    /// Rebuild one shard's snapshot in place and return it.
    async fn rebuild_workspace(&self, root: &std::path::Path) -> Option<Arc<AnalysisSnapshot>> {
        let (config, root) = {
            let ws = self.workspaces.read().await;
            let shard = ws.iter().find(|w| w.root == root)?;
            (shard.config.clone(), shard.root.clone())
        };
        let load =
            crate::workspace::load::DependencySearch::with_defaults().finalize_with_config(&config);
        let open = self.open_documents_under(&root);
        let merged = match crate::workspace::load::load_plugin_workspace(&root, &load) {
            Ok(disk) => crate::workspace::load::merge_open_documents(&disk, &open),
            Err(err) => {
                tracing::warn!("workspace load failed for {root:?}: {err}");
                crate::workspace::load::PluginWorkspaceLoad {
                    root: root.clone(),
                    files: open_into_files(open),
                    missing_required_dependencies: Vec::new(),
                    dependency_closure_complete: false,
                    usage_only_sources: Vec::new(),
                    usage_only_sources_complete: true,
                    export_paths: HashSet::new(),
                    strict_export_paths: HashSet::new(),
                }
            }
        };
        let snapshot = Arc::new(AnalysisSnapshot::from_load(&merged, &config));
        let mut ws = self.workspaces.write().await;
        if let Some(shard) = ws.iter_mut().find(|w| w.root == root) {
            shard.snapshot = snapshot.clone();
        }
        // Keep the legacy N=1 view in sync when this is the primary shard.
        if ws.len() == 1 {
            *self.snapshot.write().await = snapshot.clone();
        }
        Some(snapshot)
    }

    /// Owning workspace for a document URI: canonicalized longest-root-prefix
    /// match. `None` → stray document (per-request fallback parse, t4).
    async fn workspace_root_for(&self, uri: &Url) -> Option<PathBuf> {
        let path = uri.to_file_path().ok()?;
        let path = path.canonicalize().unwrap_or(path);
        let ws = self.workspaces.read().await;
        ws.iter()
            .filter(|w| path.starts_with(&w.root))
            .max_by_key(|w| w.root.as_os_str().len())
            .map(|w| w.root.clone())
    }

    /// Snapshot serving the given URI: the owning shard's snapshot. Strays
    /// (t4) get an EMPTY workspace context — features answer from the
    /// per-request owned parse only. With no shards at all (N=0, pre-init
    /// unit tests), the legacy single-workspace view serves everything.
    async fn snapshot_for(&self, uri: &Url) -> Arc<AnalysisSnapshot> {
        if !self.workspaces.read().await.is_empty() {
            if let Some(root) = self.workspace_root_for(uri).await {
                let ws = self.workspaces.read().await;
                if let Some(shard) = ws.iter().find(|w| w.root == root) {
                    return shard.snapshot.clone();
                }
            }
            return Arc::new(AnalysisSnapshot::from_files(&[], &LspConfig::default()));
        }
        self.snapshot.read().await.clone()
    }

    /// Config serving the given URI: the owning shard's layered config, or
    /// the legacy single-workspace config when no shards exist. Strays (t4)
    /// fall back to the legacy view — matching `snapshot_for`'s behavior of
    /// answering from the per-request context only.
    async fn config_for(&self, uri: &Url) -> LspConfig {
        if !self.workspaces.read().await.is_empty() {
            if let Some(root) = self.workspace_root_for(uri).await {
                let ws = self.workspaces.read().await;
                if let Some(shard) = ws.iter().find(|w| w.root == root) {
                    return shard.config.clone();
                }
            }
        }
        self.config.read().await.clone()
    }

    /// Build a workspace analysis snapshot: open documents overlaid on the
    /// on-disk plugin workspace (plugin sources + dependency exports).
    /// Shared path with CLI `check`; the snapshot owns parse + symbol
    /// pooling (GH #39).
    async fn build_snapshot(&self) -> Arc<AnalysisSnapshot> {
        let config = self.config.read().await;
        let root = self.workspace_root.read().await.clone();
        let open = self.open_documents();

        let load = match root.as_ref() {
            Some(root) => {
                let search = crate::workspace::load::DependencySearch::with_defaults()
                    .finalize_with_config(&config);
                match crate::workspace::load::load_plugin_workspace(root, &search) {
                    Ok(disk) => crate::workspace::load::merge_open_documents(&disk, &open),
                    Err(err) => {
                        tracing::warn!("workspace load failed, falling back to open docs: {err}");
                        crate::workspace::load::PluginWorkspaceLoad {
                            root: root.clone(),
                            files: open_into_files(open),
                            missing_required_dependencies: Vec::new(),
                            dependency_closure_complete: false,
                            usage_only_sources: Vec::new(),
                            usage_only_sources_complete: true,
                            export_paths: HashSet::new(),
                            strict_export_paths: HashSet::new(),
                        }
                    }
                }
            }
            // No workspace root: open documents only.
            None => crate::workspace::load::PluginWorkspaceLoad {
                root: PathBuf::new(),
                files: open_into_files(open),
                missing_required_dependencies: Vec::new(),
                dependency_closure_complete: false,
                usage_only_sources: Vec::new(),
                usage_only_sources_complete: true,
                export_paths: HashSet::new(),
                strict_export_paths: HashSet::new(),
            },
        };

        Arc::new(AnalysisSnapshot::from_load(&load, &config))
    }

    /// Rebuild the cached snapshot and return it.
    async fn refresh_snapshot(&self) -> Arc<AnalysisSnapshot> {
        let snap = self.build_snapshot().await;
        *self.snapshot.write().await = snap.clone();
        snap
    }

    async fn on_change(&self, uri: &Url, text: &str) {
        // Sharded path (multi-root spec): rebuild ONLY the owning workspace.
        if let Some(root) = self.workspace_root_for(uri).await {
            let snapshot = self.rebuild_workspace(&root).await;
            if let Some(snapshot) = snapshot {
                let config = {
                    let ws = self.workspaces.read().await;
                    ws.iter().find(|w| w.root == root).map(|w| w.config.clone())
                };
                if let Some(config) = config {
                    self.publish_sharded_diagnostics(uri, text, &snapshot, &config, &root)
                        .await;
                    return;
                }
            }
        }
        // Stray under no plugin root while shards exist (t4): per-request
        // parse diagnostics only — NEVER the legacy parent-root view (that
        // would merge sibling subtrees, the exact bug sharding fixes).
        if !self.workspaces.read().await.is_empty() {
            self.publish_stray_diagnostics(uri, text).await;
            return;
        }
        self.on_change_legacy(uri, text).await;
    }

    /// Parse-level diagnostics for a stray document (owned one-file symbol
    /// pool; no workspace context — t4).
    async fn publish_stray_diagnostics(&self, uri: &Url, text: &str) {
        let config = self.config.read().await.clone();
        let type_index = self.type_index.read().await;
        let analysis_owned = crate::analysis::DocumentAnalysis::analyze(text, &config.defines);
        let mut symbols = crate::symbols::SymbolTable::new();
        let fid = symbols.allocate_file_id();
        let syms = crate::symbols::SymbolTable::extract_symbols(
            fid,
            analysis_owned.masked_source(),
            &analysis_owned.file,
        );
        symbols.set_file_symbols(fid, syms);
        let diags = diagnostics::compute_diagnostics_from_analysis(
            uri,
            &analysis_owned,
            &config,
            type_index.as_deref(),
            &symbols,
        );
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;
    }

    /// Diagnostics for a document owned by a shard: file diagnostics from the
    /// shard snapshot, manifest diagnostics on the shard's own info.toml.
    async fn publish_sharded_diagnostics(
        &self,
        uri: &Url,
        text: &str,
        snapshot: &Arc<AnalysisSnapshot>,
        config: &LspConfig,
        root: &std::path::Path,
    ) {
        let type_index = self.type_index.read().await;
        // Own-module symbol view: strict export files' decls are
        // consumer-only (game parity 2026-08-23) — hide them from name
        // resolution unless we're diagnosing the export file itself.
        let symbols_for_check =
            snapshot.table_for_own_file(crate::analysis_snapshot::FileRef::Uri(uri));
        let mut diags = match snapshot.analysis_of(uri) {
            Some(analysis) => diagnostics::compute_diagnostics_from_analysis(
                uri,
                analysis,
                config,
                type_index.as_deref(),
                &symbols_for_check,
            ),
            None => {
                let analysis_owned =
                    crate::analysis::DocumentAnalysis::analyze(text, &config.defines);
                let mut symbols = crate::symbols::SymbolTable::new();
                let fid = symbols.allocate_file_id();
                let syms = crate::symbols::SymbolTable::extract_symbols(
                    fid,
                    analysis_owned.masked_source(),
                    &analysis_owned.file,
                );
                symbols.set_file_symbols(fid, syms);
                diagnostics::compute_diagnostics_from_analysis(
                    uri,
                    &analysis_owned,
                    config,
                    type_index.as_deref(),
                    &symbols,
                )
            }
        };
        // Unused-declaration warnings (workspace view: sibling usage
        // suppresses; own-manifest export files are exempt public API).
        if let Some(file_id) = snapshot.file_id_of(uri) {
            let usage =
                crate::analysis::unused::WorkspaceUsage::collect(snapshot, type_index.as_deref());
            diags.extend(diagnostics::unused_declaration_diagnostics(
                text,
                &usage,
                snapshot,
                file_id,
                snapshot.export_paths(),
            ));
        }
        if uri.path().ends_with("info.toml") {
            diags.extend(diagnostics::missing_required_dependency_diagnostics(
                snapshot.missing_required_dependencies(),
            ));
        }
        drop(type_index);
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;
        if uri.path().ends_with("info.toml") {
            return;
        }
        // Non-manifest files also refresh the owning workspace's manifest
        // diagnostics (same shape as the legacy path).
        let manifest_uri = Url::from_file_path(root.join("info.toml")).ok();
        if let Some(manifest_uri) = manifest_uri {
            let manifest_path = root.join("info.toml");
            // The client may have the manifest open through a symlink-alias
            // URI (dips++-all layout) — the canonical `manifest_uri` lookup
            // misses then. Prefer whichever URI the client actually opened
            // so diagnostics land on the visible document (and the open
            // buffer's content is used, not the disk copy).
            let open_manifest_uri = {
                let canon = manifest_path.canonicalize().ok();
                self.documents.iter().find_map(|entry| {
                    let doc_uri = entry.key();
                    if *doc_uri == manifest_uri {
                        return Some(doc_uri.clone());
                    }
                    let doc_path = doc_uri.to_file_path().ok()?.canonicalize().ok()?;
                    (Some(&doc_path) == canon.as_ref()).then(|| doc_uri.clone())
                })
            };
            let publish_uri = open_manifest_uri.unwrap_or_else(|| manifest_uri.clone());
            let manifest_source = self
                .documents
                .get(&publish_uri)
                .map(|document| document.value().clone())
                .or_else(|| std::fs::read_to_string(manifest_path).ok());
            let diagnostics = manifest_source
                .as_deref()
                .map(|source| {
                    diagnostics::compute_manifest_diagnostics(
                        &publish_uri,
                        source,
                        config,
                        snapshot.missing_required_dependencies(),
                    )
                })
                .unwrap_or_else(|| {
                    diagnostics::missing_required_dependency_diagnostics(
                        snapshot.missing_required_dependencies(),
                    )
                });
            self.client
                .publish_diagnostics(publish_uri, diagnostics, None)
                .await;
        }
    }

    /// Legacy single-workspace diagnostics path (pre-sharding behavior; used
    /// for stray/untitled documents and the N=1 back-compat view).
    async fn on_change_legacy(&self, uri: &Url, text: &str) {
        let snapshot = self.refresh_snapshot().await;
        self.publish_legacy_diagnostics(uri, text, &snapshot).await;
    }

    /// Legacy single-workspace diagnostics publish from a prebuilt
    /// snapshot (GH: didSave republishes every open doc — rebuilding the
    /// snapshot per doc would make one Ctrl+S cost M+1 full loads).
    async fn publish_legacy_diagnostics(
        &self,
        uri: &Url,
        text: &str,
        snapshot: &Arc<AnalysisSnapshot>,
    ) {
        let config = self.config.read().await;
        let type_index = self.type_index.read().await;
        let mut diags = match snapshot.analysis_of(uri) {
            // Parsed as part of the snapshot build — reuse the analysis.
            Some(analysis) => diagnostics::compute_diagnostics_from_analysis(
                uri,
                analysis,
                &config,
                type_index.as_deref(),
                snapshot.symbols(),
            ),
            // Not on disk (e.g. untitled documents) — parse on the fly
            // with an owned one-file symbol pool.
            None => {
                let analysis_owned =
                    crate::analysis::DocumentAnalysis::analyze(text, &config.defines);
                let mut symbols = crate::symbols::SymbolTable::new();
                let fid = symbols.allocate_file_id();
                let syms = crate::symbols::SymbolTable::extract_symbols(
                    fid,
                    analysis_owned.masked_source(),
                    &analysis_owned.file,
                );
                symbols.set_file_symbols(fid, syms);
                diagnostics::compute_diagnostics_from_analysis(
                    uri,
                    &analysis_owned,
                    &config,
                    type_index.as_deref(),
                    &symbols,
                )
            }
        };
        // Unused-declaration warnings — workspace view (same as sharded).
        if let Some(file_id) = snapshot.file_id_of(uri) {
            let usage =
                crate::analysis::unused::WorkspaceUsage::collect(&snapshot, type_index.as_deref());
            diags.extend(diagnostics::unused_declaration_diagnostics(
                text,
                &usage,
                &snapshot,
                file_id,
                snapshot.export_paths(),
            ));
        }
        if uri.path().ends_with("info.toml") {
            diags.extend(diagnostics::missing_required_dependency_diagnostics(
                snapshot.missing_required_dependencies(),
            ));
        }

        let manifest_diagnostics = if uri.path().ends_with("info.toml") {
            None
        } else {
            let manifest_uri = self.workspace_root.read().await.as_ref().and_then(|root| {
                let manifest_path = root.join("info.toml");
                Url::from_file_path(&manifest_path)
                    .ok()
                    .map(|manifest_uri| (manifest_uri, manifest_path))
            });
            manifest_uri.map(|(manifest_uri, manifest_path)| {
                let manifest_source = self
                    .documents
                    .get(&manifest_uri)
                    .map(|document| document.value().clone())
                    .or_else(|| std::fs::read_to_string(manifest_path).ok());
                let diagnostics = manifest_source
                    .as_deref()
                    .map(|source| {
                        diagnostics::compute_manifest_diagnostics(
                            &manifest_uri,
                            source,
                            &config,
                            snapshot.missing_required_dependencies(),
                        )
                    })
                    .unwrap_or_else(|| {
                        diagnostics::missing_required_dependency_diagnostics(
                            snapshot.missing_required_dependencies(),
                        )
                    });
                (manifest_uri, diagnostics)
            })
        };
        drop(type_index);
        drop(config);
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;

        if let Some((manifest_uri, manifest_diagnostics)) = manifest_diagnostics {
            self.client
                .publish_diagnostics(manifest_uri, manifest_diagnostics, None)
                .await;
        }
    }
}

/// Convert open documents into workspace source files (all report
/// diagnostics — they are user-editable buffers).
fn open_into_files(
    open: Vec<(PathBuf, String)>,
) -> Vec<crate::workspace::load::WorkspaceSourceFile> {
    open.into_iter()
        .map(
            |(path, source)| crate::workspace::load::WorkspaceSourceFile {
                path,
                source,
                report_diagnostics: true,
            },
        )
        .collect()
}

/// Config fields that participate in shard reuse (t9): everything that
/// changes what gets loaded/parsed. `defines` and the path fields.
fn configs_equivalent(a: &LspConfig, b: &LspConfig) -> bool {
    a.defines == b.defines
        && a.game_target == b.game_target
        && a.plugins_dir == b.plugins_dir
        && a.source_paths == b.source_paths
        && a.ignore_paths == b.ignore_paths
        && a.extra_plugins_dirs == b.extra_plugins_dirs
        && a.openplanet_dir == b.openplanet_dir
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Root intake (t2 hybrid mechanism). The opened folder (root_uri) is
        // the base; workspace folders are preferred as discovery input when
        // the client sent them.
        let root_path = params
            .root_uri
            .as_ref()
            .and_then(|uri| uri.to_file_path().ok());
        let folder_paths: Vec<PathBuf> = params
            .workspace_folders
            .as_ref()
            .map(|folders| {
                folders
                    .iter()
                    .filter_map(|f| f.uri.to_file_path().ok())
                    .collect()
            })
            .filter(|paths: &Vec<PathBuf>| !paths.is_empty())
            .unwrap_or_else(|| root_path.clone().into_iter().collect());

        if let Some(path) = &root_path {
            let config = LspConfig::load(Some(path), params.initialization_options.as_ref());
            *self.config.write().await = config;
        }

        // Effective roots (t2): explicit plugin_roots init-option overrides
        // discovery entirely; otherwise walk the folders. Zero found →
        // single-root fallback on the opened folder (pre-discovery behavior
        // for manifest-less workspaces). Phase 2 will shard the backend per
        // root; phase 1 serves exactly one.
        let override_roots = self.config.read().await.plugin_roots.clone();
        let mut effective_roots = if !override_roots.is_empty() {
            tracing::info!(
                "plugin roots overridden via initializationOptions: {:?}",
                override_roots
            );
            override_roots
        } else {
            let discovered = crate::workspace::roots::discover_plugin_roots(&folder_paths);
            if discovered.is_empty() {
                // Fallback: the opened folder itself.
                root_path.clone().into_iter().collect()
            } else {
                discovered
            }
        };
        // Canonicalize override roots so they compare equal to discovered
        // ones (discovery already canonicalizes).
        effective_roots = effective_roots
            .iter()
            .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
            .collect();

        *self.plugin_roots.write().await = effective_roots.clone();
        *self.folder_paths.write().await = folder_paths;
        *self.init_options.write().await = params.initialization_options.clone();

        match effective_roots.len() {
            0 => {
                // No root_uri and no folders: rootless server (open docs only).
                *self.workspace_root.write().await = None;
            }
            1 => {
                // N=1: the single effective root IS the workspace root. This
                // fixes the parent-dir-treated-as-root bug for single-plugin
                // trees (opening a plugin's parent used to analyze the parent
                // with no manifest).
                *self.workspace_root.write().await = Some(effective_roots[0].clone());
            }
            _ => {
                // N>1: sharded serving (phase 2) — `workspace_root` stays the
                // opened folder for back-compat readers; feature handlers
                // route per request URI through `workspaces`.
                tracing::info!(
                    "multiple plugin roots; serving N isolated workspaces: {:?}",
                    effective_roots
                );
                *self.workspace_root.write().await = root_path.clone();
            }
        }

        // Shard: one isolated workspace per effective root (multi-root spec).
        // initialize always publishes (first shard set).
        self.reshard_from_folders(
            root_path.as_deref(),
            params.initialization_options.as_ref(),
            true,
        )
        .await;

        // Load type database
        let config = self.config.read().await;
        if let (Some(core), Some(game)) = (&config.core_json, &config.game_json) {
            match TypeIndex::load(core, game) {
                Ok(index) => {
                    *self.type_index.write().await = Some(Arc::new(index));
                    tracing::info!("Type database loaded successfully");
                }
                Err(e) => {
                    tracing::warn!("Failed to load type database: {}", e);
                }
            }
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        // Save triggers a rebuild of the owning shard so
                        // sibling files that referenced a just-declared
                        // symbol refresh their diagnostics without needing
                        // an edit (user report: squiggles stuck until the
                        // referencing file was touched).
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), ":".into(), "@".into(), "#".into()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    ..Default::default()
                }),
                rename_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["openplanet.regenerateTypeDb".into()],
                    ..Default::default()
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("Openplanet LSP initialized");
        self.spawn_update_check();
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.documents.insert(uri.clone(), text.clone());
        self.on_change(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents.insert(uri.clone(), change.text.clone());
            self.on_change(&uri, &change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
        // The closed buffer no longer overlays disk; rebuild so features
        // see the on-disk contents again. Only the owning shard (if any).
        if let Some(root) = self.workspace_root_for(&params.text_document.uri).await {
            self.rebuild_workspace(&root).await;
            return;
        }
        if !self.workspaces.read().await.is_empty() {
            return; // stray: no shard to rebuild.
        }
        self.refresh_snapshot().await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        // Disk now matches the open buffer (or nearly — a watcher event may
        // follow with the same effect). Rebuild the owning shard so pooled
        // symbols/parses pick up the save, then republish diagnostics for
        // EVERY open document in the shard: a save that declares a symbol
        // clears the "undefined identifier" squiggles in sibling files
        // without them needing an edit.
        if let Some(root) = self.workspace_root_for(&uri).await {
            if let Some(snapshot) = self.rebuild_workspace(&root).await {
                let config = self.config_for(&uri).await;
                let docs: Vec<(Url, String)> = self
                    .documents
                    .iter()
                    .map(|e| (e.key().clone(), e.value().clone()))
                    .collect();
                for (doc_uri, text) in docs {
                    if self.workspace_root_for(&doc_uri).await.as_ref() == Some(&root) {
                        self.publish_sharded_diagnostics(
                            &doc_uri, &text, &snapshot, &config, &root,
                        )
                        .await;
                    }
                }
            }
            return;
        }
        if !self.workspaces.read().await.is_empty() {
            return; // stray: no shard to rebuild.
        }
        // Legacy single-workspace: ONE snapshot refresh, then publish
        // every open doc from it (not on_change_legacy — that refreshes
        // per call and would make one save cost M+1 full workspace loads).
        let snapshot = self.refresh_snapshot().await;
        let docs: Vec<(Url, String)> = self
            .documents
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (doc_uri, text) in docs {
            self.publish_legacy_diagnostics(&doc_uri, &text, &snapshot)
                .await;
        }
    }

    /// t9: manifest/file watcher events. `**/info.toml` events re-shard
    /// (roots appear/disappear); other file events rebuild the owning
    /// shard (disk contents changed under us).
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let needs_reshard = params.changes.iter().any(|event| {
            let path = event.uri.path();
            path.ends_with("/info.toml") || path == "/info.toml"
        });
        if needs_reshard {
            let root_path = self.workspace_root.read().await.clone();
            let init_options = self.init_options.read().await.clone();
            self.reshard_from_folders(root_path.as_deref(), init_options.as_ref(), true)
                .await;
            return;
        }
        // Non-manifest events: rebuild the owning workspace of each changed
        // file (disk contents changed outside the editor). Dedup by root —
        // a single save can arrive as didSave AND a watcher event, and a
        // multi-file client save reports one event per file; rebuilding the
        // same shard repeatedly is pure waste.
        let mut rebuilt = std::collections::HashSet::new();
        for event in &params.changes {
            if let Some(root) = self.workspace_root_for(&event.uri).await {
                if rebuilt.insert(root.clone()) {
                    self.rebuild_workspace(&root).await;
                }
            }
        }
    }

    /// t9: workspace folder add/remove re-shards from the updated list.
    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        {
            let mut folders = self.folder_paths.write().await;
            for added in &params.event.added {
                if let Ok(path) = added.uri.to_file_path() {
                    let canon = path.canonicalize().unwrap_or(path);
                    if !folders.contains(&canon) {
                        folders.push(canon);
                    }
                }
            }
            for removed in &params.event.removed {
                if let Ok(path) = removed.uri.to_file_path() {
                    let canon = path.canonicalize().unwrap_or(path);
                    folders.retain(|f| *f != canon);
                }
            }
        }
        let root_path = self.workspace_root.read().await.clone();
        let init_options = self.init_options.read().await.clone();
        self.reshard_from_folders(root_path.as_deref(), init_options.as_ref(), true)
            .await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => String::new(),
        };
        let type_index = self.type_index.read().await;
        let snapshot = self.snapshot_for(uri).await;
        let cfg = self.config_for(uri).await;
        let shortcut_namespaces = cfg.shortcut_namespace_list();
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        let checked = snapshot.checked_file(uri, &scope);
        let items = completion::complete(
            &analysis,
            checked.as_ref(),
            pos,
            type_index.as_deref(),
            Some(snapshot.symbols()),
            &shortcut_namespaces,
            cfg.shortcut_match_enums,
        );
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => String::new(),
        };
        let type_index = self.type_index.read().await;
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        let checked = snapshot.checked_file(uri, &scope);
        Ok(hover::hover(&analysis, checked.as_ref(), pos, &scope))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        Ok(
            navigation::goto_definition(&analysis, pos, &scope, &workspace_files)
                .map(GotoDefinitionResponse::Scalar),
        )
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let refs = navigation::find_references(
            &analysis,
            pos,
            &workspace_files,
            params.context.include_declaration,
        );
        Ok(Some(refs))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        Ok(highlights::document_highlights(&analysis, pos))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        Ok(Some(folding::folding_ranges(&analysis)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let type_index = self.type_index.read().await;
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        let checked = snapshot.checked_file(uri, &scope);
        Ok(signature::signature_help(
            &analysis,
            checked.as_ref(),
            pos,
            &scope,
        ))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let snapshot = self.snapshot_for(uri).await;
        let empty = crate::analysis::DocumentAnalysis::analyze_plain("");
        let analysis = snapshot.analysis_of(uri).unwrap_or(&empty);
        Ok(symbols::document_symbols(analysis))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        // t8 union: query ALL workspaces, merge into one flat result set.
        let mut results = Vec::new();
        {
            let ws = self.workspaces.read().await;
            for shard in ws.iter() {
                let files = shard.snapshot.uri_map();
                results.extend(symbols::workspace_symbols(
                    &params.query,
                    shard.snapshot.symbols(),
                    &files,
                ));
            }
        }
        if results.is_empty() {
            // No shards (N=0) — legacy single-workspace view.
            let snapshot = self.snapshot.read().await.clone();
            let files = snapshot.uri_map();
            results.extend(symbols::workspace_symbols(
                &params.query,
                snapshot.symbols(),
                &files,
            ));
        }
        Ok(Some(results))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        Ok(navigation::rename(
            &analysis,
            pos,
            &params.new_name,
            &workspace_files,
        ))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let tokens = semantic_tokens::semantic_tokens(&analysis);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let formatted = formatter::format_source(&source);
        if formatted == source {
            return Ok(Some(vec![]));
        }
        let line_count = source.lines().count() as u32;
        let last_line_len = source.lines().last().map(|l| l.len() as u32).unwrap_or(0);
        Ok(Some(vec![TextEdit {
            range: Range::new(
                Position::new(0, 0),
                Position::new(line_count, last_line_len),
            ),
            new_text: formatted,
        }]))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let type_index = self.type_index.read().await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let hints = inlay_hints::inlay_hints(
            &analysis,
            params.range,
            type_index.as_deref(),
            snapshot.symbols(),
        );
        Ok(Some(hints))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        let items = call_hierarchy::prepare(&analysis, uri, pos, &scope, &ws_files);
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(items))
        }
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let uri = &params.item.uri;
        let snapshot = self.snapshot_for(uri).await;
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        Ok(Some(call_hierarchy::incoming(
            &params.item,
            &scope,
            &ws_files,
        )))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let uri = &params.item.uri;
        let snapshot = self.snapshot_for(uri).await;
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        let scope = GlobalScope::new(snapshot.symbols(), type_index.as_deref());
        Ok(Some(call_hierarchy::outgoing(
            &params.item,
            &scope,
            &ws_files,
        )))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let type_index_guard = self.type_index.read().await;
        let type_index = type_index_guard.as_deref();
        let actions = code_actions::code_actions(
            uri,
            &source,
            params.range,
            &params.context.diagnostics,
            snapshot.symbols(),
            type_index,
        );
        Ok(Some(actions))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        match params.command.as_str() {
            "openplanet.regenerateTypeDb" => Ok(Some(self.regenerate_type_db().await)),
            other => {
                tracing::warn!("unknown executeCommand: {}", other);
                Ok(None)
            }
        }
    }
}

impl Backend {
    /// Non-blocking update probe: writes `~/.config/openplanet-lsp/update-status.json`
    /// at most once per 24h and surfaces an info message when a newer npm release exists.
    fn spawn_update_check(&self) {
        let client = self.client.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(|| {
                let last = update::load_status().ok().flatten();
                if !update::should_auto_check(last.as_ref(), update::default_check_interval()) {
                    // Fresh enough — keep status file, do not re-notify.
                    return None;
                }
                match update::check_for_update() {
                    Ok(status) => Some(status),
                    Err(err) => {
                        tracing::debug!("update check failed: {err}");
                        None
                    }
                }
            })
            .await;

            let status = match result {
                Ok(Some(status)) => status,
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!("update task join error: {err}");
                    return;
                }
            };

            if status.update_available {
                let latest = status.latest_version.clone().unwrap_or_else(|| "?".into());
                let cmd = status
                    .update_command
                    .clone()
                    .unwrap_or_else(|| "openplanet-lsp update".into());
                let msg = format!(
                    "openplanet-lsp {latest} is available (current {}). Run `{cmd}` or `openplanet-lsp update`.",
                    status.current_version
                );
                tracing::info!("{msg}");
                client.show_message(MessageType::INFO, msg).await;
            } else if let Some(err) = status.error {
                tracing::debug!("update check error recorded: {err}");
            }
        });
    }

    async fn regenerate_type_db(&self) -> Value {
        let start = std::time::Instant::now();
        let (core, game) = {
            let config = self.config.read().await;
            (config.core_json.clone(), config.game_json.clone())
        };
        let (core, game) = match (core, game) {
            (Some(c), Some(g)) => (c, g),
            _ => {
                return serde_json::json!({
                    "ok": false,
                    "message": "type database paths not configured (set openplanet_dir or core_json/game_json)",
                });
            }
        };
        match TypeIndex::load(&core, &game) {
            Ok(index) => {
                let count = index.type_count() + index.function_count() + index.enum_count();
                *self.type_index.write().await = Some(Arc::new(index));
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::info!(
                    "type database regenerated: {} entries in {}ms",
                    count,
                    duration_ms
                );
                serde_json::json!({
                    "ok": true,
                    "message": format!("Type database reloaded ({} entries)", count),
                    "regenerated": count,
                    "durationMs": duration_ms,
                })
            }
            Err(e) => serde_json::json!({
                "ok": false,
                "message": format!("failed to load type database: {}", e),
            }),
        }
    }
}

pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tower_lsp::lsp_types::WorkspaceFolder;

    fn temp_tree(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "openplanet-lsp-server-roots-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_info_toml(dir: &Path) {
        fs::write(dir.join("info.toml"), "[meta]\n").unwrap();
    }

    fn canonic(p: &Path) -> PathBuf {
        p.canonicalize().unwrap()
    }

    fn init_params(root: &Path, folders: Option<Vec<PathBuf>>) -> InitializeParams {
        #[allow(deprecated)]
        let params = InitializeParams {
            root_uri: Some(Url::from_directory_path(root).unwrap()),
            workspace_folders: folders.map(|fs| {
                fs.into_iter()
                    .map(|uri| WorkspaceFolder {
                        uri: Url::from_directory_path(uri).unwrap(),
                        name: "folder".to_string(),
                    })
                    .collect()
            }),
            ..Default::default()
        };
        params
    }

    async fn roots_of(backend: &Backend) -> Vec<PathBuf> {
        backend.plugin_roots.read().await.clone()
    }

    async fn workspace_root_of(backend: &Backend) -> Option<PathBuf> {
        backend.workspace_root.read().await.clone()
    }

    #[tokio::test]
    async fn single_discovered_root_fixes_parent_as_root() {
        // Regression pin (t2): opening the PARENT of a single-plugin tree
        // used to treat the parent (no info.toml) as the workspace root.
        // Discovery must find the plugin dir and make it the root.
        let parent = temp_tree("single-parent");
        let plugin = parent.join("plugin");
        fs::create_dir_all(&plugin).unwrap();
        write_info_toml(&plugin);

        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(init_params(&parent, Some(vec![parent.clone()])))
            .await
            .expect("initialize should succeed");

        assert_eq!(
            workspace_root_of(backend).await,
            Some(canonic(&plugin)),
            "N=1: workspace_root must be the discovered plugin root, not the opened parent"
        );
        assert_eq!(roots_of(backend).await, vec![canonic(&plugin)]);
    }

    #[tokio::test]
    async fn multiple_roots_leave_workspace_root_as_opened_folder() {
        // N>1: phase 1 keeps serving the opened folder as workspace_root;
        // phase 2 will shard the backend per root.
        let parent = temp_tree("multi-parent");
        for name in ["plugin-a", "plugin-b"] {
            let p = parent.join(name);
            fs::create_dir_all(&p).unwrap();
            write_info_toml(&p);
        }

        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(init_params(&parent, Some(vec![parent.clone()])))
            .await
            .expect("initialize should succeed");

        assert_eq!(
            workspace_root_of(backend).await,
            Some(canonic(&parent)),
            "N>1: workspace_root stays the opened folder in phase 1"
        );
        assert_eq!(
            roots_of(backend).await,
            vec![
                canonic(&parent.join("plugin-a")),
                canonic(&parent.join("plugin-b"))
            ]
        );
    }

    #[tokio::test]
    async fn plugin_roots_init_option_overrides_discovery() {
        let parent = temp_tree("override-parent");
        let discovered = parent.join("plugin");
        fs::create_dir_all(&discovered).unwrap();
        write_info_toml(&discovered);
        let manual = parent.join("manual-plugin");
        fs::create_dir_all(&manual).unwrap();
        write_info_toml(&manual);

        let mut params = init_params(&parent, Some(vec![parent.clone()]));
        params.initialization_options = Some(serde_json::json!({
            "plugin_roots": [manual.to_str().unwrap()]
        }));

        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(params)
            .await
            .expect("initialize should succeed");

        assert_eq!(roots_of(backend).await, vec![canonic(&manual)]);
        assert_eq!(
            workspace_root_of(backend).await,
            Some(canonic(&manual)),
            "N=1 override: workspace_root follows the single effective root"
        );
    }

    #[tokio::test]
    async fn zero_found_falls_back_to_opened_folder() {
        // Zero-found fallback (t2): single root on the opened folder itself —
        // exactly the pre-discovery behavior for manifest-less workspaces.
        let folder = temp_tree("no-plugin");
        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(init_params(&folder, Some(vec![folder.clone()])))
            .await
            .expect("initialize should succeed");

        assert_eq!(workspace_root_of(backend).await, Some(canonic(&folder)));
        assert_eq!(roots_of(backend).await, vec![canonic(&folder)]);
    }

    #[tokio::test]
    async fn no_folders_falls_back_to_root_uri_for_discovery() {
        // workspace_folders absent (single-folder clients): discovery runs on
        // the root_uri path instead.
        let parent = temp_tree("root-uri-fallback");
        let plugin = parent.join("plugin");
        fs::create_dir_all(&plugin).unwrap();
        write_info_toml(&plugin);

        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(init_params(&parent, None))
            .await
            .expect("initialize should succeed");

        assert_eq!(workspace_root_of(backend).await, Some(canonic(&plugin)));
        assert_eq!(roots_of(backend).await, vec![canonic(&plugin)]);
    }

    #[tokio::test]
    async fn did_save_rebuilds_shard_so_new_symbols_resolve() {
        // User flow: A.as calls ts_log (undeclared) → declare it in B.as →
        // save B.as. Hover over the call in A must resolve immediately —
        // no edit to A required.
        let dir = temp_tree("did-save");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        write_info_toml(&dir);
        let a_path = dir.join("src").join("A.as");
        let b_path = dir.join("src").join("B.as");
        let a_src = "void caller() {\n    ts_log(\"x\");\n}\n";
        fs::write(&a_path, a_src).unwrap();
        fs::write(&b_path, "void helper() {}\n").unwrap();

        let (service, _socket) = LspService::new(Backend::new);
        let backend = service.inner();
        backend
            .initialize(init_params(&dir, Some(vec![dir.clone()])))
            .await
            .expect("initialize");
        let a_uri = Url::from_file_path(&a_path).unwrap();
        let b_uri = Url::from_file_path(&b_path).unwrap();
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: a_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: a_src.to_string(),
                },
            })
            .await;
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: b_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void helper() {}\n".to_string(),
                },
            })
            .await;

        // Declare ts_log in B.as (buffer + disk), then save B.
        let b_new = "void helper() {}\nvoid ts_log(const string &in s) {}\n";
        fs::write(&b_path, b_new).unwrap();
        backend
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: b_uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: b_new.to_string(),
                }],
            })
            .await;
        backend
            .did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: b_uri.clone() },
                text: None,
            })
            .await;

        // Hover the call site in A: the snapshot serving A must know ts_log.
        let snapshot = backend.snapshot_for(&a_uri).await;
        let symbols = snapshot.symbols();
        assert!(
            !symbols.lookup("ts_log").is_empty(),
            "post-save snapshot missing ts_log"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
