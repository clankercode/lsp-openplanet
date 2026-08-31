pub mod call_hierarchy;
pub mod code_actions;
pub mod colors;
pub mod commands;
pub mod completion;
pub mod diagnostics;
pub mod folding;
pub mod formatter;
pub mod highlights;
pub mod hover;
pub mod inlay_hints;
pub mod lexical_scope;
pub mod navigation;
pub mod regen_barrier;
pub mod rename;
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
use crate::cli::TypeDbVersions;
use crate::config::{LspConfig, ResolvedDefine};
use crate::config_watch::{self, ConfigReloadAction};
use crate::typecheck::GlobalScope;
use crate::typedb::TypeIndex;
use crate::update;

/// Turn a rename [`Refusal`](rename::Refusal) into the JSON-RPC error the
/// client shows the user. `prepareRename` answering with an error is the
/// spec'd way to say "this position is not renameable", and VS Code
/// surfaces the message in the rename box.
fn refusal_error(refusal: rename::Refusal) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::InvalidRequest,
        message: refusal.reason.into(),
        data: None,
    }
}

/// A scoped `openplanet/resolvedDefines` query that cannot be answered for the
/// URI it was given, because several plugin workspaces are live and the URI
/// belongs to none of them.
///
/// Only that case errors. A URI outside the single live shard — a parent
/// folder, an `untitled:` scratch buffer, a sibling file — is ORDINARY and
/// unambiguous, and answering it with the one shard's config is right; the
/// legacy `self.config` mirror already holds exactly that. Rejecting those
/// too (the first cut of this fix) would have degraded the feature in the
/// commonest single-plugin layout there is: opening the PARENT of a plugin
/// directory, which server-side discovery deliberately supports.
fn unscopable_defines_uri_error() -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::InvalidParams,
        message: "openplanet/resolvedDefines: this window serves several plugin workspaces \
                  and the given uri is inside none of them, so there is no single define \
                  set to report"
            .into(),
        data: None,
    }
}

/// The custom LSP request method name (B050).
///
/// Namespaced `openplanet/...` to match the `openplanet.` command ids and
/// LSP's own `namespace/method` convention. Exported so the extension's
/// integration test and any other client name it from one place.
pub const RESOLVED_DEFINES: &str = "openplanet/resolvedDefines";

/// Params for `openplanet/resolvedDefines` (B050).
///
/// tower-lsp's typed router rejects a request with NO `params` member
/// (`FromParams for (P,)` → "Missing params field"), so a client must always
/// send an object; every field inside it is optional.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDefinesParams {
    /// A document (or directory) whose plugin workspace should answer.
    /// Multi-root windows serve one config PER root, so a window-wide answer
    /// would be the wrong one for every root but the first. Omitted: the
    /// server-wide config, which is the N=1 view.
    #[serde(default)]
    pub uri: Option<Url>,
}

/// The `openplanet/resolvedDefines` answer (B050).
///
/// A query, not a mutation — hence a custom request rather than a fifth
/// `executeCommand`, whose `Option<Value>` response is untypeable on the
/// client side.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDefinesResult {
    /// The effective set, ordered by origin class then name.
    pub defines: Vec<ResolvedDefine>,
    /// The resolved `game_target`, so a client can name it without guessing
    /// which config layer won.
    pub game_target: String,
    /// The ManiaPlanet-generation families. Travels with the answer so the
    /// client's foreign-selector guard can ask instead of duplicating
    /// `LspConfig::PLATFORM_FAMILIES`.
    pub platform_families: Vec<Vec<String>>,
    /// The plugin workspace this answer is scoped to, or `None` when the
    /// server is serving no shard at all and the answer is the window-wide
    /// config. Naming it keeps a multi-root window from showing one plugin's
    /// define set as if it were the window's — the same "partial truth with
    /// no indication it is partial" this whole request exists to end.
    pub root: Option<String>,
}

#[derive(Clone)]
pub struct Backend {
    client: Client,
    config: Arc<tokio::sync::RwLock<LspConfig>>,
    /// The loaded type database, and — inside it — the build stamps that
    /// describe it (B039's `before`/`after` report).
    ///
    /// B064: the stamps used to live in a SECOND `RwLock` beside this one.
    /// Two locks written one after the other cannot be read as a pair: a
    /// reader takes one at a time, so between the two writes it saw a new
    /// index carrying the previous load's stamps, and two regenerations
    /// racing could leave that crossing permanent. Ordering the two writes
    /// would not have helped — that serialises WRITERS while readers still
    /// take one lock at a time. Nor would one lock over a struct of both, on
    /// its own: the stamps were also a second READ of the files, which is a
    /// separate defect with no lock in it (see [`TypeIndex::versions`]).
    ///
    /// So the stamps moved into the index, where the load puts them, and the
    /// second lock was deleted rather than merged. There is now one value; a
    /// crossed pair has nowhere to come from.
    type_index: Arc<tokio::sync::RwLock<Option<Arc<TypeIndex>>>>,
    /// Open document contents: URI → source text
    documents: Arc<DashMap<Url, String>>,
    /// Cached analysis snapshot: rebuilt on every document lifecycle event
    /// (`did_open` / `did_change` / `did_close`). Requests between rebuilds
    /// read the same consistent view (GH #39). Single-workspace view over
    /// the sharded state below (spec: multi-root phase 2).
    snapshot: Arc<tokio::sync::RwLock<Arc<AnalysisSnapshot>>>,
    workspace_root: Arc<tokio::sync::RwLock<Option<PathBuf>>>,
    /// Effective plugin roots, set once at `initialize` (t2 hybrid):
    /// `config.plugin_roots` (init-option override) when non-empty, else
    /// `discover_plugin_roots` over the workspace folders (root_uri
    /// fallback), else the opened folder itself.
    plugin_roots: Arc<tokio::sync::RwLock<Vec<PathBuf>>>,
    /// Sharded state (multi-root spec): one isolated workspace per effective
    /// plugin root — own config, own snapshot, own diagnostics. The legacy
    /// `snapshot` / `workspace_root` / `config` fields stay as the N=1 view;
    /// feature handlers route through `workspace_for` per request URI.
    workspaces: Arc<tokio::sync::RwLock<Vec<PluginWorkspace>>>,
    /// Workspace folder paths as of the last initialize/folder-change event
    /// (t9): input for re-walking plugin roots on watched-file events.
    folder_paths: Arc<tokio::sync::RwLock<Vec<PathBuf>>>,
    /// Cached initialization options (t9): re-sharding after watched-file
    /// events must re-apply the same layers (plugin_roots override, parent
    /// config base, extra plugins dirs).
    init_options: Arc<tokio::sync::RwLock<Option<serde_json::Value>>>,
    /// Keep the user-global config notify watcher alive. Shared so the
    /// background reload task and the LanguageServer value see one watcher.
    config_watch: Arc<
        tokio::sync::Mutex<Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>>,
    >,
    /// B017: config-file problems already shown to the user, so a re-shard
    /// (every watched-file event) does not repeat the same `showMessage`.
    /// Rendered issue text, not a struct — one line per distinct problem.
    reported_config_warnings: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    /// B035: the client advertised `codeAction.dataSupport` +
    /// `resolveSupport`, so code actions may be offered without their edit
    /// and materialised in `codeAction/resolve`. A client that did not must
    /// receive edits up front or its actions would do nothing.
    code_action_resolve_support: Arc<std::sync::atomic::AtomicBool>,
}

/// B035: may we hand back a code action with no `edit` and wait for
/// `codeAction/resolve`? Both halves are required — `dataSupport` so our
/// resolve token survives the round trip, and `resolveSupport` so the client
/// asks at all. Without them an edit-less action is a dead menu entry.
fn client_resolves_code_actions(caps: &ClientCapabilities) -> bool {
    caps.text_document
        .as_ref()
        .and_then(|td| td.code_action.as_ref())
        .is_some_and(|ca| {
            ca.data_support == Some(true)
                && ca.resolve_support.as_ref().is_some_and(|resolve| {
                    resolve.properties.iter().any(|property| property == "edit")
                })
        })
}

/// A snapshot may be rebuilding while the open-document map already contains
/// newer text. Code actions must bind their analysis and stale-action digest
/// to that current text, not to the previous snapshot revision.
fn analysis_for_current_source<'a>(
    snapshot_analysis: Option<&'a crate::analysis::DocumentAnalysis>,
    owned: &'a crate::analysis::DocumentAnalysis,
) -> &'a crate::analysis::DocumentAnalysis {
    snapshot_analysis
        .filter(|analysis| analysis.source == owned.source)
        .unwrap_or(owned)
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

    /// Notify-watch `$HOME/.config/openplanet-lsp/` (or `home` in tests)
    /// so the user-global file reloads without a client file-watcher.
    /// `extra_dirs` are plugin roots / opened folders (non-recursive) so
    /// `.openplanet-lsp.toml` is seen even when the client does not watch it.
    pub fn start_user_config_file_watch(&self, home: Option<PathBuf>) {
        self.start_config_file_watch(home, &[]);
    }

    fn start_config_file_watch(&self, home: Option<PathBuf>, extra_dirs: &[PathBuf]) {
        let this = self.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        match config_watch::watch_config_files_with_action(
            home.as_deref(),
            extra_dirs,
            move |action| {
                let _ = tx.send(action);
            },
        ) {
            Ok(debouncer) => {
                // Best-effort replace; a second call (tests) drops the old watch.
                if let Ok(mut slot) = self.config_watch.try_lock() {
                    *slot = Some(debouncer);
                } else {
                    tokio::spawn({
                        let watch = Arc::clone(&self.config_watch);
                        async move {
                            *watch.lock().await = Some(debouncer);
                        }
                    });
                }
            }
            Err(err) => {
                tracing::warn!("user config watch unavailable ({err})");
                return;
            }
        }
        tokio::spawn(async move {
            while let Some(action) = rx.recv().await {
                match action {
                    ConfigReloadAction::Reload => this.reload_config_from_disk().await,
                    ConfigReloadAction::KeepLastGood { issue } => {
                        this.report_config_issue(&issue).await;
                    }
                }
            }
        });
    }

    /// Re-run the config ladder and rebuild every shard. Used when a
    /// config file changes (workspace `.openplanet-lsp.toml` or the
    /// user-global file). Invalid TOML is rejected by the caller so last-good
    /// stays in place.
    pub async fn reload_config_from_disk(&self) {
        let root_path = self.workspace_root.read().await.clone();
        let init_options = self.init_options.read().await.clone();
        self.reshard_from_folders(root_path.as_deref(), init_options.as_ref(), true, true)
            .await;
        self.republish_open_document_diagnostics().await;
    }

    /// After a config reload, republish diagnostics for every open buffer
    /// so squiggles match the new defines / discovery / target.
    async fn republish_open_document_diagnostics(&self) {
        let docs: Vec<(Url, String)> = self
            .documents
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (uri, text) in docs {
            if let Some(root) = self.workspace_root_for(&uri).await {
                let snapshot = self.snapshot_for(&uri).await;
                let config = self.config_for(&uri).await;
                self.publish_sharded_diagnostics(&uri, &text, &snapshot, &config, &root)
                    .await;
            } else if self.workspaces.read().await.is_empty() {
                let snapshot = self.snapshot.read().await.clone();
                self.publish_legacy_diagnostics(&uri, &text, &snapshot)
                    .await;
            }
        }
    }

    /// t9: rebuild the shard set from the current folder list. Reuses
    /// config+snapshot Arcs for roots whose (config-relevant) inputs are
    /// unchanged so unrelated shards keep pointer identity.
    async fn reshard_from_folders(
        &self,
        root_path: Option<&std::path::Path>,
        init_options: Option<&serde_json::Value>,
        publish_diagnostics: bool,
        force_reload: bool,
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
            self.report_config_issues(&config).await;
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
            // identity for untouched siblings on re-shard. Config-file
            // reloads force a rebuild so last-good is replaced even when
            // a previously-ignored field (shortcuts, honor_gitignore)
            // is the only change.
            let existing = self
                .workspaces
                .read()
                .await
                .iter()
                .find(|w| w.root == *root)
                .cloned();
            if !force_reload {
                if let Some(prev) = &existing {
                    if configs_equivalent(&prev.config, &config) {
                        shards.push(prev.clone());
                        continue;
                    }
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
                        unresolved_optional_dependencies: Vec::new(),
                        usage_only_sources: Vec::new(),
                        usage_only_sources_complete: true,
                        usage_only_unreadable_paths: Vec::new(),
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
            self.publish_manifest_diagnostics_for_all_roots().await;
        }
    }

    /// Resolve the manifest URI and source a client is currently editing.
    ///
    /// A plugin may be opened through a symlink farm, so the URI visible to
    /// the client need not equal `root/info.toml`. Prefer that URI and its
    /// buffer over the canonical disk path; otherwise diagnostics disappear
    /// from the document the user can actually fix.
    fn manifest_diagnostic_target(&self, root: &std::path::Path) -> Option<(Url, Option<String>)> {
        let manifest_path = root.join("info.toml");
        let manifest_uri = Url::from_file_path(&manifest_path).ok()?;
        let canon = manifest_path.canonicalize().ok();
        let open_manifest_uri = self.documents.iter().find_map(|entry| {
            let doc_uri = entry.key();
            if *doc_uri == manifest_uri {
                return Some(doc_uri.clone());
            }
            let doc_path = doc_uri.to_file_path().ok()?.canonicalize().ok()?;
            (Some(&doc_path) == canon.as_ref()).then(|| doc_uri.clone())
        });
        let publish_uri = open_manifest_uri.unwrap_or(manifest_uri);
        let source = self
            .documents
            .get(&publish_uri)
            .map(|document| document.value().clone())
            .or_else(|| std::fs::read_to_string(&manifest_path).ok());
        Some((publish_uri, source))
    }

    /// Publish the info.toml diagnostic set for one live shard. Kept as the
    /// single refresh operation for close/watch/config lifecycle paths.
    async fn publish_manifest_diagnostics_for_root(&self, root: &std::path::Path) {
        let live = self
            .workspaces
            .read()
            .await
            .iter()
            .find(|workspace| workspace.root == root)
            .map(|workspace| (workspace.config.clone(), workspace.snapshot.clone()));
        let Some((config, snapshot)) = live else {
            return;
        };
        let Some((manifest_uri, source)) = self.manifest_diagnostic_target(root) else {
            return;
        };
        let search =
            crate::workspace::load::DependencySearch::with_defaults().finalize_with_config(&config);
        let workspace = crate::workspace::manifest::ManifestWorkspace {
            root,
            plugin_files_search_paths: &search.plugin_files_search_paths,
        };
        let diagnostics = source
            .as_deref()
            .map(|source| {
                diagnostics::compute_manifest_diagnostics_in_workspace(
                    source,
                    &workspace,
                    snapshot.missing_required_dependencies(),
                )
            })
            .unwrap_or_default();
        self.client
            .publish_diagnostics(manifest_uri, diagnostics, None)
            .await;
    }

    /// Publish the info.toml diagnostic set for every live shard, so a newly
    /// added root gets its manifest squiggles without anything being opened.
    ///
    /// Called from `reshard_from_folders` AND from `initialized`: tower-lsp
    /// suppresses client messages until the `initialize` response is out, so
    /// the reshard that runs during `initialize` publishes into the void.
    async fn publish_manifest_diagnostics_for_all_roots(&self) {
        let roots: Vec<PathBuf> = self
            .workspaces
            .read()
            .await
            .iter()
            .map(|workspace| workspace.root.clone())
            .collect();
        for root in roots {
            self.publish_manifest_diagnostics_for_root(&root).await;
        }
    }
}

impl Backend {
    /// The type database the server is serving, and the stamps that describe
    /// it, in ONE observation. `None` when no database is loaded.
    ///
    /// The single place server code answers "what is loaded?" — B039's
    /// `before`, and the atomicity tests, both come through here, so the
    /// answer the user is given and the answer a test checks cannot drift
    /// apart. Keeping it a real method rather than two field reads is also
    /// what makes B064 hard to reintroduce: the stamps are taken from the
    /// index in hand, so there is no version of this that can read them from
    /// somewhere else and disagree.
    pub async fn loaded_type_db(&self) -> Option<(Arc<TypeIndex>, TypeDbVersions)> {
        let index = self.type_index.read().await.clone()?;
        let versions = index.versions().clone();
        Some((index, versions))
    }

    pub fn new(client: Client) -> Self {
        Self {
            client,
            config: Arc::new(tokio::sync::RwLock::new(LspConfig::load(None, None))),
            type_index: Arc::new(tokio::sync::RwLock::new(None)),
            documents: Arc::new(DashMap::new()),
            snapshot: Arc::new(tokio::sync::RwLock::new(Arc::new(
                AnalysisSnapshot::from_files(&[], &LspConfig::default()),
            ))),
            workspace_root: Arc::new(tokio::sync::RwLock::new(None)),
            plugin_roots: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            workspaces: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            folder_paths: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            init_options: Arc::new(tokio::sync::RwLock::new(None)),
            config_watch: Arc::new(tokio::sync::Mutex::new(None)),
            reported_config_warnings: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            code_action_resolve_support: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// B017: surface unusable config keys once each. A mistyped key used to
    /// discard its whole file silently; now the key is skipped and the user
    /// is told which file, which key, and what was expected. Warnings also go
    /// to stderr from the config loader — this is the in-editor channel.
    async fn report_config_issues(&self, config: &LspConfig) {
        for issue in &config.config_issues {
            self.report_config_issue(issue).await;
        }
    }

    async fn report_config_issue(&self, issue: &crate::config::ConfigIssue) {
        self.report_config_warning(issue.to_string()).await;
    }

    /// Emit one distinct config warning to the editor. The dedup lock guards
    /// only the set insertion and is always released before the client await.
    async fn report_config_warning(&self, line: String) {
        let fresh = {
            let mut seen = self.reported_config_warnings.lock().await;
            seen.insert(line.clone())
        };
        if !fresh {
            return;
        }
        tracing::warn!("config: {line}");
        self.client
            .show_message(
                MessageType::WARNING,
                format!("openplanet-lsp config: {line}"),
            )
            .await;
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
                    unresolved_optional_dependencies: Vec::new(),
                    usage_only_sources: Vec::new(),
                    usage_only_sources_complete: true,
                    usage_only_unreadable_paths: Vec::new(),
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
                            unresolved_optional_dependencies: Vec::new(),
                            usage_only_sources: Vec::new(),
                            usage_only_sources_complete: true,
                            usage_only_unreadable_paths: Vec::new(),
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
                unresolved_optional_dependencies: Vec::new(),
                usage_only_sources: Vec::new(),
                usage_only_sources_complete: true,
                usage_only_unreadable_paths: Vec::new(),
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
    /// The "we are deliberately publishing nothing" note (B018, alongside
    /// GH #61). An author whose gitignored file silently stopped showing
    /// errors has no way to discover why, and that is a worse surprise than
    /// the squiggles GH #60 set out to remove — so name the file and the way
    /// out. INFO rather than DEBUG for the same reason: it has to be findable
    /// without turning on debug logging.
    fn log_usage_only_suppression(uri: &Url) {
        tracing::info!(
            uri = %uri,
            "publishing no diagnostics: this file is omitted from the checked set \
             (excluded by .gitignore, or filtered out by source_paths / ignore_paths), \
             so the editor matches what `check` reports. It still counts as usage for \
             unused-declaration warnings. To diagnose it, set honor_gitignore = false in \
             .openplanet-lsp.toml, or take its directory out of ignore_paths / add it to \
             source_paths."
        );
    }

    async fn publish_sharded_diagnostics(
        &self,
        uri: &Url,
        text: &str,
        snapshot: &Arc<AnalysisSnapshot>,
        config: &LspConfig,
        root: &std::path::Path,
    ) {
        if uri.path().ends_with("info.toml") {
            let search = crate::workspace::load::DependencySearch::with_defaults()
                .finalize_with_config(config);
            let workspace = crate::workspace::manifest::ManifestWorkspace {
                root,
                plugin_files_search_paths: &search.plugin_files_search_paths,
            };
            let diagnostics = diagnostics::compute_manifest_diagnostics_in_workspace(
                text,
                &workspace,
                snapshot.missing_required_dependencies(),
            );
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
            return;
        }
        let type_index = self.type_index.read().await;
        // Own-module symbol view: strict export files' decls are
        // consumer-only (game parity 2026-08-23) — hide them from name
        // resolution unless we're diagnosing the export file itself.
        let symbols_for_check =
            snapshot.table_for_own_file(crate::analysis_snapshot::FileRef::Uri(uri));
        // B031: greying out preprocessor-dead branches is an LSP-only
        // affordance — emitted here beside the shared analysis, never from
        // `compute_diagnostics_from_analysis` (which `check` also calls).
        let inactive = |analysis: &crate::analysis::DocumentAnalysis| {
            diagnostics::inactive_region_diagnostics(text, analysis, config.inactive_regions)
        };
        let mut diags = if snapshot.is_usage_only_uri(uri) {
            Self::log_usage_only_suppression(uri);
            Vec::new()
        } else {
            match snapshot.analysis_of(uri) {
                Some(analysis) => {
                    let mut d = diagnostics::compute_diagnostics_from_analysis(
                        uri,
                        analysis,
                        config,
                        type_index.as_deref(),
                        &symbols_for_check,
                    );
                    d.extend(inactive(analysis));
                    d
                }
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
                    let mut d = diagnostics::compute_diagnostics_from_analysis(
                        uri,
                        &analysis_owned,
                        config,
                        type_index.as_deref(),
                        &symbols,
                    );
                    d.extend(inactive(&analysis_owned));
                    d
                }
            }
        };
        // Unused-declaration warnings (workspace view: sibling usage
        // suppresses; own-manifest export files are exempt public API).
        if config.unused != crate::config::UnusedLevel::Ignore {
            if let Some(file_id) = snapshot.file_id_of(uri) {
                let usage = crate::analysis::unused::WorkspaceUsage::collect(
                    snapshot,
                    type_index.as_deref(),
                );
                diags.extend(diagnostics::unused_declaration_diagnostics(
                    text,
                    &usage,
                    snapshot,
                    file_id,
                    snapshot.export_paths(),
                    config.unused,
                ));
            }
        }
        drop(type_index);
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;
        // Non-manifest files also refresh the owning workspace's manifest
        // diagnostics (same shape as the legacy path).
        if let Some((publish_uri, manifest_source)) = self.manifest_diagnostic_target(root) {
            let search = crate::workspace::load::DependencySearch::with_defaults()
                .finalize_with_config(config);
            let workspace = crate::workspace::manifest::ManifestWorkspace {
                root,
                plugin_files_search_paths: &search.plugin_files_search_paths,
            };
            let diagnostics = manifest_source
                .as_deref()
                .map(|source| {
                    diagnostics::compute_manifest_diagnostics_in_workspace(
                        source,
                        &workspace,
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
        let mut diags = if snapshot.is_usage_only_uri(uri) {
            Self::log_usage_only_suppression(uri);
            Vec::new()
        } else {
            match snapshot.analysis_of(uri) {
                // Parsed as part of the snapshot build — reuse the analysis.
                Some(analysis) => {
                    let mut d = diagnostics::compute_diagnostics_from_analysis(
                        uri,
                        analysis,
                        &config,
                        type_index.as_deref(),
                        snapshot.symbols(),
                    );
                    // B031: LSP-only, same as the sharded path above.
                    d.extend(diagnostics::inactive_region_diagnostics(
                        text,
                        analysis,
                        config.inactive_regions,
                    ));
                    d
                }
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
                    let mut d = diagnostics::compute_diagnostics_from_analysis(
                        uri,
                        &analysis_owned,
                        &config,
                        type_index.as_deref(),
                        &symbols,
                    );
                    d.extend(diagnostics::inactive_region_diagnostics(
                        text,
                        &analysis_owned,
                        config.inactive_regions,
                    ));
                    d
                }
            }
        };
        // Unused-declaration warnings — workspace view (same as sharded).
        if config.unused != crate::config::UnusedLevel::Ignore {
            if let Some(file_id) = snapshot.file_id_of(uri) {
                let usage = crate::analysis::unused::WorkspaceUsage::collect(
                    &snapshot,
                    type_index.as_deref(),
                );
                diags.extend(diagnostics::unused_declaration_diagnostics(
                    text,
                    &usage,
                    &snapshot,
                    file_id,
                    snapshot.export_paths(),
                    config.unused,
                ));
            }
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
                    .map(|manifest_uri| (manifest_uri, manifest_path, root.clone()))
            });
            manifest_uri.map(|(manifest_uri, manifest_path, root)| {
                let manifest_source = self
                    .documents
                    .get(&manifest_uri)
                    .map(|document| document.value().clone())
                    .or_else(|| std::fs::read_to_string(manifest_path).ok());
                // Root-explicit, exactly like the sharded path: the URI is
                // publication identity only, never the directory `exports`
                // resolve against.
                let search = crate::workspace::load::DependencySearch::with_defaults()
                    .finalize_with_config(&config);
                let workspace = crate::workspace::manifest::ManifestWorkspace {
                    root: &root,
                    plugin_files_search_paths: &search.plugin_files_search_paths,
                };
                let diagnostics = manifest_source
                    .as_deref()
                    .map(|source| {
                        diagnostics::compute_manifest_diagnostics_in_workspace(
                            source,
                            &workspace,
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

fn is_config_watch_event(path: Option<&std::path::Path>, uri_path: &str) -> bool {
    if path.is_some_and(config_watch::is_config_path) {
        return true;
    }
    uri_path.ends_with("/.openplanet-lsp.toml")
        || uri_path.ends_with("/.config/openplanet-lsp/config.toml")
        || uri_path == "/.openplanet-lsp.toml"
}

/// Config fields that participate in shard reuse (t9): everything that
/// changes what gets loaded/parsed *or* what features read off the cached
/// shard config (shortcuts, unused-callback exemptions).
fn configs_equivalent(a: &LspConfig, b: &LspConfig) -> bool {
    a.defines == b.defines
        && a.game_target == b.game_target
        && a.plugins_dir == b.plugins_dir
        && a.source_paths == b.source_paths
        && a.ignore_paths == b.ignore_paths
        && a.extra_plugins_dirs == b.extra_plugins_dirs
        && a.openplanet_dir == b.openplanet_dir
        && a.honor_gitignore == b.honor_gitignore
        && a.shortcut_namespaces == b.shortcut_namespaces
        && a.shortcut_disable_default_namespaces == b.shortcut_disable_default_namespaces
        && a.shortcut_match_enums == b.shortcut_match_enums
        && a.unused_callbacks == b.unused_callbacks
        && a.unused == b.unused
        && a.errors == b.errors
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // B035: only defer code-action edits to `codeAction/resolve` when the
        // client can actually round-trip `data` and ask for the resolve.
        self.code_action_resolve_support.store(
            client_resolves_code_actions(&params.capabilities),
            std::sync::atomic::Ordering::Relaxed,
        );

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
            false,
        )
        .await;

        // Load type database
        let config = self.config.read().await;
        if let (Some(core), Some(game)) = (&config.core_json, &config.game_json) {
            match TypeIndex::load(core, game) {
                Ok(index) => {
                    // GH #59: name the build, not just the success — a
                    // "confidently wrong" diagnostic is traceable to a DB
                    // version only if the log says which one loaded. B064:
                    // asked of the index, so it names what LOADED rather than
                    // what a fresh read of the files would find.
                    tracing::info!(
                        "Type database loaded successfully; {}",
                        index.versions().summary_line()
                    );
                    // B039: the stamps travel with the index, so a later
                    // regeneration can say what CHANGED rather than just that
                    // it ran.
                    *self.type_index.write().await = Some(Arc::new(index));
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
                // B049: `OneOf::Left(true)` cannot advertise prepare
                // support, so F2 went straight to the destructive path
                // with no chance to tell the client "not renameable here".
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                code_action_provider: Some(code_actions::code_action_provider()),
                document_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                // B041: vec3/vec4 literals and Openplanet `$xxx` escapes.
                color_provider: Some(ColorProviderCapability::Simple(true)),
                // B039: from the one list `execute_command` dispatches on,
                // never a literal here — an advertised name with no handler
                // behind it is invisible to every client.
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: commands::advertised(),
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
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut extra = self.plugin_roots.read().await.clone();
        extra.extend(self.folder_paths.read().await.iter().cloned());
        self.start_config_file_watch(home, &extra);
        // B040: the reshard during `initialize` published while tower-lsp was
        // still suppressing client messages, so the manifest set never
        // reached the editor. Now is the first moment it can.
        self.publish_manifest_diagnostics_for_all_roots().await;
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
            self.publish_manifest_diagnostics_for_root(&root).await;
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
    /// (roots appear/disappear); config-file events reload the config
    /// ladder; other file events rebuild the owning shard (disk contents
    /// changed under us).
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut config_reload = false;
        let mut config_issues = Vec::new();
        for event in &params.changes {
            let file_path = event.uri.to_file_path().ok();
            if !is_config_watch_event(file_path.as_deref(), event.uri.path()) {
                continue;
            }
            match file_path
                .as_deref()
                .map(config_watch::config_reload_action)
                .unwrap_or(ConfigReloadAction::Reload)
            {
                ConfigReloadAction::Reload => config_reload = true,
                ConfigReloadAction::KeepLastGood { issue } => {
                    config_issues.push(issue);
                }
            }
        }
        // Report every unusable config file, THEN still reload if any other
        // changed config was valid. Suppressing the reload whenever one file
        // in the batch is broken would let a half-saved user-global config
        // silently swallow a workspace edit — the one-bad-file-blocks-
        // everything shape B017 exists to delete, moved from the parser to
        // the watcher. Guarded by
        // `invalid_config_in_a_batch_does_not_suppress_a_valid_sibling_reload`.
        let had_issues = !config_issues.is_empty();
        for issue in config_issues {
            self.report_config_issue(&issue).await;
        }
        if config_reload {
            self.reload_config_from_disk().await;
            return;
        }
        if had_issues {
            return;
        }
        let needs_reshard = params.changes.iter().any(|event| {
            let path = event.uri.path();
            path.ends_with("/info.toml") || path == "/info.toml"
        });
        if needs_reshard {
            let root_path = self.workspace_root.read().await.clone();
            let init_options = self.init_options.read().await.clone();
            self.reshard_from_folders(root_path.as_deref(), init_options.as_ref(), true, false)
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
                    self.publish_manifest_diagnostics_for_root(&root).await;
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
        self.reshard_from_folders(root_path.as_deref(), init_options.as_ref(), true, false)
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
        // B054: `receiver.member` resolution needs the checker's recorded
        // expression types, the same view hover consumes.
        let checked = snapshot.checked_file(uri, &scope);
        Ok(
            navigation::goto_definition(&analysis, checked.as_ref(), pos, &scope, &workspace_files)
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
        // Own files only, for the same reason rename uses them: a
        // dependency plugin is a SEPARATE module, so a symbol there that
        // merely shares a spelling is a different symbol, and reporting
        // its occurrences as references to this one is the very
        // false-positive class B049 exists to remove. B055: a strict
        // `[script] exports` file is a separate module by the same
        // argument, inside this plugin.
        let boundary_name = navigation::name_at_position(analysis, pos);
        let files = snapshot.own_uri_map_for_module_of(
            crate::analysis_snapshot::FileRef::Uri(uri),
            boundary_name.as_deref(),
        );
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        // B055: the requesting file's MODULE, not the whole plugin. A
        // plugin with strict `[script] exports` compiles as two units that
        // cannot see each other, and the same spelling may legally be
        // declared on both sides — so a rename or a reference set that
        // crossed the boundary would report one symbol's occurrences as
        // another's. See `own_uri_map_for_module_of` for the probe.
        let symbols = snapshot.table_for_module_of(crate::analysis_snapshot::FileRef::Uri(uri));
        let scope = GlobalScope::new(symbols.as_ref(), type_index.as_deref());
        let refs = rename::find_references(
            analysis,
            uri,
            pos,
            &scope,
            &workspace_files,
            params.context.include_declaration,
        );
        Ok(Some(refs))
    }

    /// `textDocument/documentColor` (B041): swatches for `vec3`/`vec4`
    /// literals and Openplanet `$xxx` escape codes.
    ///
    /// Reads the open document directly — the whole feature is decidable from
    /// the token stream, so there is no snapshot or type index to consult and
    /// nothing to go stale between keystrokes.
    async fn document_color(&self, params: DocumentColorParams) -> Result<Vec<ColorInformation>> {
        let Some(source) = self.documents.get(&params.text_document.uri) else {
            return Ok(Vec::new());
        };
        Ok(colors::document_colors(source.value()))
    }

    /// `textDocument/colorPresentation` (B041): what the picker writes back.
    ///
    /// The form comes from the source at `range`, never from the request, so
    /// the user's representation survives: a `$f00` stays a `$f00` and a
    /// `vec3` never becomes a `vec4`.
    async fn color_presentation(
        &self,
        params: ColorPresentationParams,
    ) -> Result<Vec<ColorPresentation>> {
        let Some(source) = self.documents.get(&params.text_document.uri) else {
            return Ok(Vec::new());
        };
        Ok(colors::color_presentations(
            source.value(),
            params.range,
            params.color,
        ))
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
        // Own files only: a WorkspaceEdit naming a dependency plugin's
        // export file would have the editor rewrite someone else's source.
        // B055: narrowed further to the requesting file's own module.
        let boundary_name = navigation::name_at_position(analysis, pos);
        let files = snapshot.own_uri_map_for_module_of(
            crate::analysis_snapshot::FileRef::Uri(uri),
            boundary_name.as_deref(),
        );
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        // B055 — see the note on `references`.
        let symbols = snapshot.table_for_module_of(crate::analysis_snapshot::FileRef::Uri(uri));
        let scope = GlobalScope::new(symbols.as_ref(), type_index.as_deref());
        rename::rename(
            analysis,
            uri,
            pos,
            &params.new_name,
            &scope,
            &workspace_files,
        )
        .map(Some)
        .map_err(refusal_error)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let pos = params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot_for(uri).await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        // Own files only: a WorkspaceEdit naming a dependency plugin's
        // export file would have the editor rewrite someone else's source.
        // B055: narrowed further to the requesting file's own module.
        let boundary_name = navigation::name_at_position(analysis, pos);
        let files = snapshot.own_uri_map_for_module_of(
            crate::analysis_snapshot::FileRef::Uri(uri),
            boundary_name.as_deref(),
        );
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let type_index = self.type_index.read().await;
        // B055 — see the note on `references`.
        let symbols = snapshot.table_for_module_of(crate::analysis_snapshot::FileRef::Uri(uri));
        let scope = GlobalScope::new(symbols.as_ref(), type_index.as_deref());
        rename::prepare_rename(analysis, uri, pos, &scope, &workspace_files)
            .map(|range| Some(PrepareRenameResponse::Range(range)))
            .map_err(refusal_error)
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
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = analysis_for_current_source(snapshot.analysis_of(uri), &owned);
        let uri_map = snapshot.uri_map();
        let workspace_files: Vec<_> = uri_map.into_values().collect();
        let actions = code_actions::code_actions(&code_actions::CodeActionRequest {
            uri,
            analysis,
            range: params.range,
            context: &params.context,
            workspace: snapshot.symbols(),
            type_index: type_index_guard.as_deref(),
            files: code_actions::fix::WorkspaceFileResolver::new(&workspace_files),
            lazy_edits: self
                .code_action_resolve_support
                .load(std::sync::atomic::Ordering::Relaxed),
        });
        Ok(Some(actions))
    }

    /// `codeAction/resolve`: fill in the edit for the one action the user
    /// picked. Unknown or already-resolved actions come back untouched — the
    /// client may resolve anything it was handed.
    async fn code_action_resolve(&self, params: CodeAction) -> Result<CodeAction> {
        let Some(uri) = code_actions::resolve_target_uri(&params) else {
            return Ok(params);
        };
        let Some(source) = self.documents.get(&uri).map(|d| d.value().clone()) else {
            return Ok(params);
        };
        let snapshot = self.snapshot_for(&uri).await;
        let type_index_guard = self.type_index.read().await;
        let owned = crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = analysis_for_current_source(snapshot.analysis_of(&uri), &owned);
        let uri_map = snapshot.uri_map();
        let workspace_files: Vec<_> = uri_map.into_values().collect();
        Ok(code_actions::resolve_code_action(
            params,
            analysis,
            snapshot.symbols(),
            type_index_guard.as_deref(),
            code_actions::fix::WorkspaceFileResolver::new(&workspace_files),
        ))
    }

    /// `workspace/executeCommand` (B039).
    ///
    /// The match on [`ServerCommand`](commands::ServerCommand) is exhaustive
    /// on purpose: a command added to that enum without an arm here does not
    /// compile. Unknown names are an ERROR, not `Ok(None)` — a client cannot
    /// tell a null answer apart from a command that ran and had nothing to
    /// say, which is exactly how B039's command stayed unreachable.
    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        let Some(command) = commands::ServerCommand::parse(&params.command) else {
            tracing::warn!("unknown executeCommand: {}", params.command);
            return Err(commands::unknown_command_error(&params.command));
        };
        match command {
            commands::ServerCommand::RegenerateTypeDb => Ok(Some(self.regenerate_type_db().await)),
        }
    }
}

impl Backend {
    /// `openplanet/resolvedDefines` (B050): the effective define set with an
    /// ORIGIN per entry, plus whether each is currently active.
    ///
    /// The set is computed entirely server-side by a five-layer ladder
    /// (B017/B019/B024/B028) and until now no client could read it, so the
    /// extension's Manage Defines list could only show the user's own array —
    /// a partial truth with no indication it was partial. Re-deriving it
    /// client-side was rejected: it would be a second implementation of
    /// precedence rules that four tickets have each had to fix once.
    ///
    /// Read-only: nothing here mutates server state, which is why it is a
    /// request rather than a command.
    pub async fn resolved_defines(
        &self,
        params: ResolvedDefinesParams,
    ) -> Result<ResolvedDefinesResult> {
        let (config, root) = self.config_for_defines_query(params.uri.as_ref()).await?;
        Ok(ResolvedDefinesResult {
            defines: config.resolved_defines(),
            game_target: config.game_target.clone(),
            platform_families: LspConfig::platform_families()
                .iter()
                .map(|family| family.iter().map(|name| (*name).to_string()).collect())
                .collect(),
            root: root.map(|root| root.to_string_lossy().into_owned()),
        })
    }

    /// Pick the config a resolved-defines query should answer from, and name
    /// the shard it came from.
    ///
    /// Deliberately NOT `config_for`. That helper answers a stray document
    /// with the legacy `self.config`, which mirrors the FIRST shard — a
    /// sensible "best effort" for a feature handler asked about a file it
    /// does not own, and a silent lie for this request, whose entire job is
    /// to report the define set that actually applies. So:
    ///
    /// * owned URI → that shard, named;
    /// * unowned URI with exactly ONE shard → that shard, named. There is no
    ///   ambiguity to protect the caller from, and this is the ordinary case:
    ///   a parent folder, an `untitled:` buffer, a file beside the plugin.
    /// * unowned URI with SEVERAL shards → refuse. Any answer here would be
    ///   some other plugin's define set wearing this one's name.
    /// * no shards (pre-init, rootless) → the window-wide config, unnamed.
    async fn config_for_defines_query(
        &self,
        uri: Option<&Url>,
    ) -> Result<(LspConfig, Option<PathBuf>)> {
        let workspaces = self.workspaces.read().await;
        let Some((first, rest)) = workspaces.split_first() else {
            drop(workspaces);
            return Ok((self.config.read().await.clone(), None));
        };
        let owner = uri
            .and_then(|uri| uri.to_file_path().ok())
            .map(|path| path.canonicalize().unwrap_or(path))
            .and_then(|path| {
                workspaces
                    .iter()
                    .filter(|workspace| path.starts_with(&workspace.root))
                    // Deepest root wins, matching `workspace_root_for`: a
                    // nested plugin is served by itself, not by its parent.
                    .max_by_key(|workspace| workspace.root.as_os_str().len())
            });
        match owner {
            Some(workspace) => Ok((workspace.config.clone(), Some(workspace.root.clone()))),
            None if rest.is_empty() => Ok((first.config.clone(), Some(first.root.clone()))),
            None => Err(unscopable_defines_uri_error()),
        }
    }

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

    /// `openplanet.regenerateTypeDb` (B039): re-read the type database off
    /// disk and swap it in.
    ///
    /// The answer reports the stamps BEFORE and AFTER, not just success.
    /// This command exists for one situation — the user updated Openplanet
    /// or the game and the symbol surface went stale — and in that situation
    /// "done" does not answer the question they asked. `before` is the
    /// database the server had loaded; `after` is the one it has now. Equal
    /// stamps mean the reload picked up nothing new, which is a real and
    /// common outcome (the dump has not been regenerated by the game yet) and
    /// one a user otherwise mistakes for success.
    ///
    /// B064: `after` describes the database that was LOADED, not whatever is
    /// on disk by the time the answer is written. Those are the same thing
    /// only when nothing rewrites the dump mid-reload — and the game
    /// rewriting the dump is the event this command exists to report on.
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
        // B064: the stamps of what is loaded NOW, asked of the index itself
        // and through the same accessor the atomicity tests observe, so
        // `before` and what a reader sees cannot drift apart.
        let before = self.loaded_type_db().await.map(|(_, versions)| versions);
        match TypeIndex::load(&core, &game) {
            Ok(index) => {
                let count = index.type_count() + index.function_count() + index.enum_count();
                // B064, both halves of it.
                //
                // `after` is asked of the INDEX, not of the files. It used to
                // be `TypeDbVersions::probe(&core, &game)` — a second read of
                // the two files `TypeIndex::load` had just parsed — so a dump
                // the game rewrote in between produced a report describing a
                // database that was never loaded. On one thread, with no lock
                // anywhere near it.
                //
                // And the index and its stamps are ONE value under ONE lock,
                // so publishing them is one step. It used to be two writes to
                // two locks, which a reader taking one lock at a time could
                // catch half-done, and which two concurrent regenerations
                // could interleave into a pair that permanently described
                // different loads.
                let after = index.versions().clone();
                let index = Arc::new(index);
                // The one point a test can stop the clock at: parsed, nothing
                // published. Inert unless a test armed it.
                regen_barrier::wait_if_armed().await;
                *self.type_index.write().await = Some(index);
                let duration_ms = start.elapsed().as_millis() as u64;
                let changed = before.as_ref() != Some(&after);
                tracing::info!(
                    "type database regenerated: {} entries in {}ms; {}",
                    count,
                    duration_ms,
                    after.summary_line()
                );
                serde_json::json!({
                    "ok": true,
                    "message": format!("Type database reloaded ({} entries)", count),
                    "regenerated": count,
                    "durationMs": duration_ms,
                    "before": before.as_ref().map(TypeDbVersions::to_json),
                    "after": after.to_json(),
                    "changed": changed,
                })
            }
            Err(e) => serde_json::json!({
                "ok": false,
                "message": format!("failed to load type database: {}", e),
                "before": before.as_ref().map(TypeDbVersions::to_json),
            }),
        }
    }
}

/// Build the service the server actually serves, with every custom method
/// registered.
///
/// The ONE registration site. `run_stdio` and the end-to-end tests both go
/// through here, so a handler cannot be implemented on `Backend`, unit-tested
/// green, and left off the router — the failure mode B040 was filed for.
pub fn build_service() -> (LspService<Backend>, tower_lsp::ClientSocket) {
    LspService::build(Backend::new)
        .custom_method(RESOLVED_DEFINES, Backend::resolved_defines)
        .finish()
}

pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = build_service();
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::fs;
    use std::path::Path;
    use tower::{Service, ServiceExt};
    use tower_lsp::jsonrpc::Request;
    use tower_lsp::lsp_types::WorkspaceFolder;

    fn test_service() -> (
        std::sync::MutexGuard<'static, ()>,
        LspService<Backend>,
        tower_lsp::ClientSocket,
    ) {
        let env = crate::testing::env_lock();
        let (service, socket) = LspService::new(Backend::new);
        (env, service, socket)
    }

    fn code_action_caps(
        data_support: Option<bool>,
        resolve_properties: Option<Vec<&str>>,
    ) -> ClientCapabilities {
        ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                code_action: Some(CodeActionClientCapabilities {
                    data_support,
                    resolve_support: resolve_properties.map(|properties| {
                        CodeActionCapabilityResolveSupport {
                            properties: properties.into_iter().map(str::to_string).collect(),
                        }
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn lazy_code_action_edits_require_data_and_explicit_edit_resolve_support() {
        assert!(client_resolves_code_actions(&code_action_caps(
            Some(true),
            Some(vec!["edit"]),
        )));
        for (name, capabilities) in [
            (
                "no data support",
                code_action_caps(None, Some(vec!["edit"])),
            ),
            (
                "data explicitly false",
                code_action_caps(Some(false), Some(vec!["edit"])),
            ),
            ("no resolve support", code_action_caps(Some(true), None)),
            (
                "empty properties",
                code_action_caps(Some(true), Some(vec![])),
            ),
            (
                "non-edit property",
                code_action_caps(Some(true), Some(vec!["command"])),
            ),
        ] {
            assert!(
                !client_resolves_code_actions(&capabilities),
                "{name} must receive eager edits"
            );
        }
    }

    #[test]
    fn code_actions_prefer_owned_analysis_when_snapshot_source_is_stale() {
        let stale = crate::analysis::DocumentAnalysis::analyze_plain("void f(string s) {}");
        let current = crate::analysis::DocumentAnalysis::analyze_plain("void f(int n) {}");
        assert!(std::ptr::eq(
            analysis_for_current_source(Some(&stale), &current),
            &current
        ));
        assert!(std::ptr::eq(
            analysis_for_current_source(Some(&current), &current),
            &current
        ));
    }

    fn temp_tree(name: &str) -> crate::testing::TempDir {
        let base = crate::testing::TempDir::new(&format!("openplanet-lsp-server-roots-{}", name));
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

    async fn initialize_service(service: &mut LspService<Backend>, params: InitializeParams) {
        let request = Request::build("initialize")
            .params(serde_json::to_value(params).unwrap())
            .id(1)
            .finish();
        let response = service
            .ready()
            .await
            .expect("service ready")
            .call(request)
            .await
            .expect("initialize request succeeds");
        assert!(response.is_some(), "initialize returns a response");
    }

    async fn capture_diagnostics_for<F>(
        socket: &mut tower_lsp::ClientSocket,
        uri: &Url,
        event: F,
    ) -> PublishDiagnosticsParams
    where
        F: std::future::Future<Output = ()>,
    {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::pin!(event);
            let mut event_done = false;
            let mut wanted = None;
            loop {
                if event_done {
                    if let Some(params) = wanted {
                        return params;
                    }
                }
                tokio::select! {
                    _ = &mut event, if !event_done => {
                        event_done = true;
                    }
                    request = socket.next() => {
                        let request = request.expect("client socket remains open");
                        if request.method() != "textDocument/publishDiagnostics" {
                            continue;
                        }
                        let params: PublishDiagnosticsParams = serde_json::from_value(
                            request
                                .params()
                                .expect("publishDiagnostics carries params")
                                .clone(),
                        )
                        .expect("valid publishDiagnostics params");
                        if &params.uri == uri {
                            wanted = Some(params);
                        }
                    }
                }
            }
        })
        .await
        .expect("server publishes diagnostics before timeout")
    }

    // === B055: rename / references around a STRICT export file ==========
    //
    // Ground truth, live RemoteBuild probe 2026-08-30 (a strict module
    // `module = "T"` with `exports = ["Exports_Bodies.as"]`):
    //
    //   Main.as calling the exported `SendEvent()`     -> ERROR
    //                                        "No matching symbol 'SendEvent'"
    //   the export file's body calling a name that
    //   exists NOWHERE                                 -> clean
    //   the export file containing a SYNTAX ERROR      -> clean, plugin runs
    //   `SendEvent` declared in BOTH Main.as and the
    //   export file                                    -> clean, no collision
    //   the same with `shared_exports` instead         -> clean, resolves
    //
    // So the export file is not merely invisible to its own plugin's other
    // files — it is not compiled into the defining plugin at all, and its
    // declarations inhabit a separate namespace that may legally reuse a
    // spelling the own module also uses.

    async fn open_document(backend: &Backend, path: &Path, source: &str) -> Url {
        let uri = Url::from_file_path(path).unwrap();
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: source.to_string(),
                },
            })
            .await;
        uri
    }

    fn text_position_params(uri: Url, source: &str, needle: &str) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: diagnostics::offset_to_position(
                source,
                source.find(needle).expect("needle in source"),
            ),
        }
    }

    async fn references_at(
        backend: &Backend,
        position: TextDocumentPositionParams,
    ) -> Vec<Location> {
        backend
            .references(ReferenceParams {
                text_document_position: position,
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .unwrap()
    }

    async fn rename_at(
        backend: &Backend,
        position: TextDocumentPositionParams,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>> {
        backend
            .rename(RenameParams {
                text_document_position: position,
                new_name: new_name.into(),
                work_done_progress_params: Default::default(),
            })
            .await
    }

    /// Write a plugin tree and start a server on it. Returns the root.
    fn strict_export_plugin(
        name: &str,
        manifest_script: &str,
        files: &[(&str, &str)],
    ) -> crate::testing::TempDir {
        let root = temp_tree(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("info.toml"),
            format!(
                "[meta]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[script]\n{manifest_script}"
            ),
        )
        .unwrap();
        for (rel, source) in files {
            fs::write(root.join("src").join(rel), source).unwrap();
        }
        root
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rename_handlers_refuse_a_strict_export_symbol_from_an_own_module_file() {
        // From Main.as the exported name resolves to NOTHING — the game
        // rejects the call outright — so there is no symbol to rename and
        // no reference to report.
        let main = "void Main() { SendEvent(); }\n";
        let root = strict_export_plugin(
            "rename-strict-export",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\n",
            &[
                ("Main.as", main),
                ("Exports_Bodies.as", "void SendEvent() {}\n"),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let uri = open_document(backend, &root.join("src/Main.as"), main).await;
        let position = text_position_params(uri, main, "SendEvent");

        assert!(
            backend.prepare_rename(position.clone()).await.is_err(),
            "a strict export is invisible from Main.as"
        );
        assert!(
            rename_at(backend, position.clone(), "RenamedEvent")
                .await
                .is_err(),
            "rename must agree with prepareRename"
        );
        assert!(
            references_at(backend, position).await.is_empty(),
            "an unresolved strict call has no references"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_strict_export_declaration_renames_within_its_own_file() {
        // ALLOWED, and the probe is why. The refusal this test replaces was
        // justified as "the rename would rewrite only the declaration and
        // leave callers broken" — but there can BE no own-file caller: the
        // game rejects one, and the export file is not compiled into the
        // defining plugin at all. Inside the plugin the rename is complete.
        //
        // The remaining objection is that other plugins consume the
        // surface. That is equally true of `shared_exports`, which the
        // test below requires to stay renameable, and the server cannot
        // see any consumer of either. Refusing here would make the most
        // common legitimate rename impossible while claiming a safety
        // property the server has never had.
        let export = "void SendEvent() {}\n";
        let root = strict_export_plugin(
            "rename-strict-export-decl",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\n",
            &[
                ("Main.as", "void Main() {}\n"),
                ("Exports_Bodies.as", export),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let export_path = root.join("src/Exports_Bodies.as");
        let export_uri = open_document(backend, &export_path, export).await;
        let position = text_position_params(export_uri.clone(), export, "SendEvent");

        assert!(
            backend.prepare_rename(position.clone()).await.is_ok(),
            "a file always sees its own declarations"
        );
        let edit = rename_at(backend, position.clone(), "RenamedEvent")
            .await
            .expect("the declaration is renameable")
            .expect("a rename carries an edit");
        let changes = edit.changes.expect("a rename must carry changes");
        assert_eq!(
            changes.keys().collect::<Vec<_>>(),
            vec![&export_uri],
            "the edit must be confined to the export file"
        );

        // NOT empty, for `include_declaration: true`. An empty reference
        // list is a positive claim the user acts on by deleting the
        // declaration, and it would be false even about the declaration
        // itself. What the server can honestly report is what it can see:
        // this module's occurrences. It cannot express "a consumer plugin
        // I cannot see may call this" — no list can, and returning nothing
        // does not say it either. With `include_declaration: false` the
        // declaration is excluded as everywhere else; see
        // `references_honour_include_declaration_at_a_strict_export`.
        let refs = references_at(backend, position).await;
        assert_eq!(refs.len(), 1, "the declaration itself, got {refs:?}");
        assert_eq!(refs[0].uri, export_uri);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_strict_export_rename_leaves_a_same_named_own_module_symbol_alone() {
        // The game accepts `SendEvent` declared in BOTH the own module and
        // a strict export file (probe, 2026-08-30) — they are separate
        // modules. So each rename must stay inside its own module's view.
        let main = "void SendEvent() {}\nvoid Main() { SendEvent(); }\n";
        let export = "void SendEvent() {}\n";
        let root = strict_export_plugin(
            "rename-strict-export-collision",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\n",
            &[("Main.as", main), ("Exports_Bodies.as", export)],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let main_uri = open_document(backend, &root.join("src/Main.as"), main).await;
        let export_uri = open_document(backend, &root.join("src/Exports_Bodies.as"), export).await;

        let export_edit = rename_at(
            backend,
            text_position_params(export_uri.clone(), export, "SendEvent"),
            "RenamedExport",
        )
        .await
        .expect("export declaration is renameable")
        .expect("edit")
        .changes
        .expect("changes");
        assert_eq!(
            export_edit.keys().collect::<Vec<_>>(),
            vec![&export_uri],
            "renaming the export must not touch the own module's own SendEvent"
        );

        let main_edit = rename_at(
            backend,
            text_position_params(main_uri.clone(), main, "SendEvent"),
            "RenamedOwn",
        )
        .await
        .expect("own declaration is renameable")
        .expect("edit")
        .changes
        .expect("changes");
        assert_eq!(
            main_edit.keys().collect::<Vec<_>>(),
            vec![&main_uri],
            "renaming the own-module symbol must not touch the export file"
        );
        assert_eq!(
            main_edit[&main_uri].len(),
            2,
            "the own declaration and its own call site"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_strict_export_rename_reaches_a_sibling_export_file() {
        // Export files compile TOGETHER into a consumer: in the
        // two-plugin probe a consumer called `AFn` and `BFn` from two
        // different export files unqualified, and `Exports_B.as`'s call to
        // `Exports_A.as`'s `AFn` ran. So the export surface is one module
        // and a rename inside it must reach the whole of it — while still
        // leaving the own module's homonym alone.
        let a = "void AFn() {}\n";
        let b = "void BFn() { AFn(); }\n";
        let root = strict_export_plugin(
            "rename-strict-export-siblings",
            "module = \"T\"\nexports = [\"Exports_A.as\", \"Exports_B.as\"]\n",
            &[
                ("Main.as", "void AFn() {}\nvoid Main() { AFn(); }\n"),
                ("Exports_A.as", a),
                ("Exports_B.as", b),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let a_uri = open_document(backend, &root.join("src/Exports_A.as"), a).await;
        let b_uri = open_document(backend, &root.join("src/Exports_B.as"), b).await;
        let main_uri = Url::from_file_path(root.join("src/Main.as")).unwrap();
        let position = text_position_params(a_uri.clone(), a, "AFn");

        let changes = rename_at(backend, position.clone(), "RenamedA")
            .await
            .expect("renameable")
            .expect("edit")
            .changes
            .expect("changes");
        let mut touched: Vec<&Url> = changes.keys().collect();
        touched.sort_by_key(|u| u.as_str().to_string());
        let mut want = vec![&a_uri, &b_uri];
        want.sort_by_key(|u| u.as_str().to_string());
        assert_eq!(touched, want, "both export files, and only those");
        assert!(
            !changes.contains_key(&main_uri),
            "the own module's own AFn is a different symbol"
        );

        let mut ref_uris: Vec<String> = references_at(backend, position)
            .await
            .into_iter()
            .map(|l| l.uri.to_string())
            .collect();
        ref_uris.sort();
        ref_uris.dedup();
        assert_eq!(
            ref_uris,
            vec![a_uri.to_string(), b_uri.to_string()],
            "the declaration and the sibling export's call"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn own_module_references_stop_at_the_strict_export_boundary() {
        // The read-only twin of the collision case: a homonym across the
        // boundary must not be reported as a reference to this symbol.
        // Reporting it would be the false positive B049 exists to remove,
        // pointed inward instead of at a dependency plugin.
        let main = "void SendEvent() {}\nvoid Main() { SendEvent(); }\n";
        let root = strict_export_plugin(
            "references-strict-export-boundary",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\n",
            &[
                ("Main.as", main),
                ("Exports_Bodies.as", "void SendEvent() {}\n"),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let main_uri = open_document(backend, &root.join("src/Main.as"), main).await;
        let refs = references_at(
            backend,
            text_position_params(main_uri.clone(), main, "SendEvent"),
        )
        .await;
        assert_eq!(refs.len(), 2, "the declaration and its call, got {refs:?}");
        assert!(
            refs.iter().all(|r| r.uri == main_uri),
            "no reference may name the export file, got {refs:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_strict_export_file_can_rename_a_shared_export_symbol_everywhere() {
        // `shared_exports` ship to consumers AND compile into the defining
        // plugin, so a shared symbol lives in BOTH modules. Narrowing the
        // export side to strict files alone would have made it unresolvable
        // from inside an export file — and, worse, renameable there without
        // reaching the own module's use of it. Nothing is narrowed unless
        // the name is declared on both sides, so this rename stays whole.
        let export = "void SendEvent() { SharedFn(); }\n";
        let root = strict_export_plugin(
            "rename-strict-plus-shared",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\nshared_exports = [\"Shared.as\"]\n",
            &[
                ("Main.as", "void Main() { SharedFn(); }\n"),
                ("Shared.as", "void SharedFn() {}\n"),
                ("Exports_Bodies.as", export),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let export_uri = open_document(backend, &root.join("src/Exports_Bodies.as"), export).await;
        let position = text_position_params(export_uri.clone(), export, "SharedFn");

        let changes = rename_at(backend, position.clone(), "RenamedShared")
            .await
            .expect("a shared-export symbol is renameable from an export file")
            .expect("edit")
            .changes
            .expect("changes");
        for expected in ["Main.as", "Shared.as", "Exports_Bodies.as"] {
            let uri = Url::from_file_path(root.join("src").join(expected)).unwrap();
            assert!(
                changes.contains_key(&uri),
                "{expected} must be edited, got {:?}",
                changes.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(
            references_at(backend, position).await.len(),
            3,
            "the declaration and both call sites"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn references_honour_include_declaration_at_a_strict_export() {
        // The "not empty" decision is about `include_declaration: true`,
        // which is what a client sends for "find all references". With it
        // false the declaration is excluded like anywhere else, and here it
        // is the only occurrence — so empty is the correct answer, not a
        // claim that the symbol is unused.
        let export = "void SendEvent() {}\n";
        let root = strict_export_plugin(
            "references-strict-export-decl-flag",
            "module = \"T\"\nexports = [\"Exports_Bodies.as\"]\n",
            &[
                ("Main.as", "void Main() {}\n"),
                ("Exports_Bodies.as", export),
            ],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let export_uri = open_document(backend, &root.join("src/Exports_Bodies.as"), export).await;
        let position = text_position_params(export_uri, export, "SendEvent");

        assert_eq!(references_at(backend, position.clone()).await.len(), 1);
        let without = backend
            .references(ReferenceParams {
                text_document_position: position,
                context: ReferenceContext {
                    include_declaration: false,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .unwrap();
        assert!(
            without.is_empty(),
            "the declaration is the only occurrence, got {without:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rename_handlers_keep_shared_export_symbols_visible() {
        // `shared_exports` are compiled into the defining plugin too
        // (probe: a call from Main.as is clean), so the whole workspace
        // edit is the normal one.
        let main = "void Main() { SharedFn(); }\n";
        let root = strict_export_plugin(
            "rename-shared-export",
            "module = \"T\"\nshared_exports = [\"Shared.as\"]\n",
            &[("Main.as", main), ("Shared.as", "void SharedFn() {}\n")],
        );

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&root, Some(vec![root.to_path_buf()])))
            .await
            .expect("initialize");
        let uri = open_document(backend, &root.join("src/Main.as"), main).await;
        let position = text_position_params(uri.clone(), main, "SharedFn");

        assert!(backend.prepare_rename(position.clone()).await.is_ok());
        let changes = rename_at(backend, position.clone(), "VisibleFn")
            .await
            .unwrap()
            .unwrap()
            .changes
            .unwrap();
        assert!(changes.contains_key(&uri), "the call site must be edited");
        assert!(
            changes.contains_key(&Url::from_file_path(root.join("src/Shared.as")).unwrap()),
            "the shared-export declaration must be edited"
        );
        assert_eq!(
            references_at(backend, position).await.len(),
            2,
            "the shared declaration plus its call"
        );
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

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
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

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
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

        let mut params = init_params(&parent, Some(vec![parent.to_path_buf()]));
        params.initialization_options = Some(serde_json::json!({
            "plugin_roots": [manual.to_str().unwrap()]
        }));

        let (_env, service, _socket) = test_service();
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
        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&folder, Some(vec![folder.to_path_buf()])))
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

        let (_env, service, _socket) = test_service();
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

        let (_env, service, _socket) = test_service();
        let backend = service.inner();
        backend
            .initialize(init_params(&dir, Some(vec![dir.to_path_buf()])))
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn omitted_open_buffers_publish_no_diagnostics_across_lifecycle() {
        let root = temp_tree("usage-only-diagnostics");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        write_info_toml(&root);
        fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        fs::write(
            root.join(".openplanet-lsp.toml"),
            "honor_gitignore = true\n",
        )
        .unwrap();
        fs::write(
            root.join("src/Main.as"),
            "void UsedOnlyByOmitted() {}\nvoid Main() {}\n",
        )
        .unwrap();
        let omitted_path = root.join("generated/Gen.as");
        fs::write(&omitted_path, "void GenCaller() { UsedOnlyByOmitted(); }\n").unwrap();

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        let omitted_uri = Url::from_file_path(&omitted_path).unwrap();

        let parse_error =
            "void GenCaller() { UsedOnlyByOmitted(); }\nvoid Dead() {}\nvoid Broken( {\n";
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &omitted_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: omitted_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: parse_error.into(),
                },
            }),
        )
        .await;
        assert_eq!(
            diagnostics.diagnostics,
            Vec::<Diagnostic>::new(),
            "didOpen must publish an empty set for an omitted source"
        );

        let type_and_unused_errors = "void GenCaller() {\n\
             UsedOnlyByOmitted();\n\
             int wrong = \"text\";\n\
         }\n\
         void Dead() {}\n";
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &omitted_uri,
            backend.did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: omitted_uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: type_and_unused_errors.into(),
                }],
            }),
        )
        .await;
        assert!(
            diagnostics.diagnostics.is_empty(),
            "didChange must publish an empty set for an omitted source"
        );

        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &omitted_uri,
            backend.did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: omitted_uri.clone(),
                },
                text: None,
            }),
        )
        .await;
        assert!(
            diagnostics.diagnostics.is_empty(),
            "didSave must publish an empty set for an omitted source"
        );

        let snapshot = backend.snapshot_for(&omitted_uri).await;
        assert!(snapshot.file_id_of(&omitted_uri).is_none());
        assert!(snapshot.symbols().lookup("GenCaller").is_empty());
        assert!(!snapshot.symbols().dependency_closure_complete());
        let unused =
            crate::analysis::unused::unused_declarations(&snapshot, None, snapshot.export_paths());
        assert!(unused
            .iter()
            .all(|finding| finding.qualified_name != "UsedOnlyByOmitted"));

        let new_path = root.join("src/NewUnsaved.as");
        let new_uri = Url::from_file_path(&new_path).unwrap();
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &new_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: new_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Broken( {\n".into(),
                },
            }),
        )
        .await;
        assert!(
            !diagnostics.diagnostics.is_empty(),
            "a genuinely new non-omitted buffer keeps diagnostics"
        );

        // Exercise the legacy publisher too: the legacy N=1 mirror uses the
        // same disk load/open overlay once shard state is absent.
        backend.workspaces.write().await.clear();
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &omitted_uri,
            backend.did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: omitted_uri.clone(),
                    version: 3,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: parse_error.into(),
                }],
            }),
        )
        .await;
        assert!(
            diagnostics.diagnostics.is_empty(),
            "legacy publisher must also suppress omitted-source diagnostics"
        );

        let untitled_uri = Url::parse("untitled:NewUnsaved.as").unwrap();
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &untitled_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: untitled_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Broken( {\n".into(),
                },
            }),
        )
        .await;
        assert!(
            !diagnostics.diagnostics.is_empty(),
            "the legacy one-file fallback stays active for a genuine untitled buffer"
        );
    }

    /// B018's motivating case, as its own named test: a gitignored file whose
    /// SIBLINGS declare the symbols it uses.
    ///
    /// This is the false positive that made B018 worth a task. The old
    /// fallback pooled symbols from the opened buffer ALONE, so every name
    /// declared in `src/` came back unknown — `check` and the editor
    /// disagreed about what was being compiled, and the squiggles landed on
    /// the one file the author had open. Routing on snapshot membership
    /// (B018 resolution 1) is what fixes it.
    ///
    /// The second half is the non-vacuity control: the same harness, on a
    /// file that is NOT omitted, must still report an unknown name. Without
    /// it, an empty diagnostic set proves nothing about this test.
    #[tokio::test(flavor = "current_thread")]
    async fn omitted_buffer_using_sibling_declarations_is_not_diagnosed() {
        let root = temp_tree("usage-only-sibling-decls");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        write_info_toml(&root);
        fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        fs::write(
            root.join("src/Types.as"),
            "class Config {\n    int value;\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/Api.as"),
            "void Register(Config@ c) {}\nvoid Main() { Config c; Register(c); }\n",
        )
        .unwrap();
        let omitted_path = root.join("generated/Gen.as");
        // Uses a sibling class, a sibling member and a sibling function —
        // all invisible to a one-file symbol pool.
        let uses_siblings =
            "void GenCaller() {\n    Config c;\n    c.value = 1;\n    Register(c);\n}\n";
        fs::write(&omitted_path, uses_siblings).unwrap();

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        let omitted_uri = Url::from_file_path(&omitted_path).unwrap();

        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &omitted_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: omitted_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: uses_siblings.into(),
                },
            }),
        )
        .await;
        assert_eq!(
            diagnostics.diagnostics,
            Vec::<Diagnostic>::new(),
            "a gitignored buffer using sibling declarations must not be diagnosed"
        );

        // Non-vacuity: the same harness DOES report an unknown name on a
        // file that is part of the checked set.
        let probe_path = root.join("src/Probe.as");
        let probe_uri = Url::from_file_path(&probe_path).unwrap();
        let unknown_name = "void Probe() { NoSuchSiblingFunction(); }\n";
        fs::write(&probe_path, unknown_name).unwrap();
        let diagnostics = capture_diagnostics_for(
            &mut socket,
            &probe_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: probe_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: unknown_name.into(),
                },
            }),
        )
        .await;
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .any(|d| d.message.contains("NoSuchSiblingFunction")),
            "control: a checked file must still report an unknown name; got {:?}",
            diagnostics.diagnostics
        );
    }

    // ---------------------------------------------------------------
    // B040: info.toml diagnostics reach the editor.
    //
    // `Manifest::validate` was fully implemented and called by nobody, so a
    // manifest that stops the plugin loading in-game produced no editor
    // signal. These pin the publish path end to end: the diagnostics go to
    // the info.toml URI (never to a `.as` file — they are unfixable from
    // there), they carry a usable range, and they CLEAR when fixed.
    // ---------------------------------------------------------------

    /// A plugin root with the given manifest and one source file.
    fn manifest_tree(name: &str, info_toml: &str) -> crate::testing::TempDir {
        let root = temp_tree(name);
        fs::write(root.join("info.toml"), info_toml).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        root
    }

    fn manifest_uri_of(root: &Path) -> Url {
        Url::from_file_path(root.join("info.toml")).unwrap()
    }

    fn open_params(uri: &Url, text: &str) -> DidOpenTextDocumentParams {
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "toml".into(),
                version: 1,
                text: text.into(),
            },
        }
    }

    fn change_params(uri: &Url, version: i32, text: &str) -> DidChangeTextDocumentParams {
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.into(),
            }],
        }
    }

    /// Open `info.toml` in a fresh session and return what the server
    /// published against it.
    async fn manifest_diagnostics_on_open(root: &Path, info_toml: &str) -> Vec<Diagnostic> {
        let uri = manifest_uri_of(root);
        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_open(open_params(&uri, info_toml)),
        )
        .await
        .diagnostics
    }

    /// The text a diagnostic's range selects, so range assertions read as
    /// "it points at this", not as line/column arithmetic.
    fn selected(source: &str, range: Range) -> String {
        let start = diagnostics::position_to_offset(source, range.start);
        let end = diagnostics::position_to_offset(source, range.end);
        source[start..end].to_string()
    }

    /// Every published range must address real text in the document — an
    /// out-of-bounds range is dropped or mis-rendered by clients.
    fn assert_ranges_within(source: &str, published: &[Diagnostic]) {
        let lines: Vec<&str> = source.split('\n').collect();
        for diagnostic in published {
            for (label, pos) in [
                ("start", diagnostic.range.start),
                ("end", diagnostic.range.end),
            ] {
                let line = lines
                    .get(pos.line as usize)
                    .unwrap_or_else(|| panic!("{label} line {} past end of document", pos.line));
                assert!(
                    pos.character as usize <= line.encode_utf16().count(),
                    "{label} column {} past end of line {:?}",
                    pos.character,
                    line
                );
            }
            assert!(
                (
                    diagnostic.range.start.line,
                    diagnostic.range.start.character
                ) <= (diagnostic.range.end.line, diagnostic.range.end.character),
                "inverted range {:?}",
                diagnostic.range
            );
        }
    }

    const NO_VERSION: &str = "# Counter\n[meta]\nname = \"Counter\"\n";

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_diagnostics_are_published_for_info_toml() {
        let root = manifest_tree("manifest-no-version", NO_VERSION);
        let published = manifest_diagnostics_on_open(&root, NO_VERSION).await;

        let version = published
            .iter()
            .find(|d| d.message.contains("version"))
            .unwrap_or_else(|| panic!("missing `version` must be reported; got {published:?}"));
        assert_eq!(version.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            version.source.as_deref(),
            Some(diagnostics::DIAGNOSTIC_SOURCE)
        );
        // The key is absent, so the anchor is the containing table's header.
        assert_eq!(selected(NO_VERSION, version.range), "meta");
        assert_ranges_within(NO_VERSION, &published);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_export_missing_file_is_reported() {
        let source = "[meta]\nversion = \"1.0\"\n\n[script]\nexports = [\"Gone.as\"]\n";
        let root = manifest_tree("manifest-missing-export", source);
        let published = manifest_diagnostics_on_open(&root, source).await;

        assert_eq!(published.len(), 1, "{published:?}");
        assert_eq!(published[0].message, "Export file not found: Gone.as");
        assert_eq!(published[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(selected(source, published[0].range), "\"Gone.as\"");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_shared_export_missing_file_is_reported() {
        let source = "[meta]\nversion = \"1.0\"\n\n[script]\nshared_exports = [\"Gone.as\"]\n";
        let root = manifest_tree("manifest-missing-shared-export", source);
        let published = manifest_diagnostics_on_open(&root, source).await;

        assert_eq!(published.len(), 1, "{published:?}");
        assert_eq!(
            published[0].message,
            "Shared export file not found: Gone.as"
        );
        assert_eq!(published[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(selected(source, published[0].range), "\"Gone.as\"");
    }

    /// An export that IS on disk — including through the loader's `src/`
    /// search path, which is where most plugins keep their sources — must
    /// not be reported. This is the false-positive guard for the rule above.
    #[tokio::test(flavor = "current_thread")]
    async fn present_exports_publish_nothing() {
        let source = "[meta]\nversion = \"1.0\"\n\n[script]\nexports = [\"Ex/Fns.as\"]\nshared_exports = [\"Shared.as\"]\n";
        let root = manifest_tree("manifest-exports-present", source);
        // `Ex/Fns.as` under `src/`, `Shared.as` at the root: both resolve.
        fs::create_dir_all(root.join("src/Ex")).unwrap();
        fs::write(root.join("src/Ex/Fns.as"), "void F() {}\n").unwrap();
        fs::write(root.join("Shared.as"), "void S() {}\n").unwrap();

        let published = manifest_diagnostics_on_open(&root, source).await;
        assert_eq!(published, Vec::<Diagnostic>::new(), "{published:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deprecated_manifest_field_is_published_as_a_warning() {
        let source = "[meta]\nversion = \"1.0\"\nperms = \"full\"\n";
        let root = manifest_tree("manifest-deprecated", source);
        let published = manifest_diagnostics_on_open(&root, source).await;

        assert_eq!(published.len(), 1, "{published:?}");
        assert!(published[0].message.contains("deprecated"), "{published:?}");
        assert_eq!(
            published[0].severity,
            Some(DiagnosticSeverity::WARNING),
            "a deprecated field does not stop the plugin loading"
        );
        assert_eq!(selected(source, published[0].range), "perms = \"full\"");
    }

    /// Every rule fires in one publish — the first problem must not shadow
    /// the rest, or fixing them becomes a guessing game one save at a time.
    #[tokio::test(flavor = "current_thread")]
    async fn every_manifest_problem_is_published_together() {
        let source = "[meta]\nname = \"T\"\nperms = \"full\"\n\n[script]\nexports = [\"A.as\", \"B.as\"]\nshared_exports = [\"S.as\"]\n";
        let root = manifest_tree("manifest-all-rules", source);
        let published = manifest_diagnostics_on_open(&root, source).await;

        let messages: Vec<&str> = published.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "Missing required field: [meta].version",
                "Export file not found: A.as",
                "Export file not found: B.as",
                "Shared export file not found: S.as",
                "'perms' is deprecated",
            ],
            "{published:?}"
        );
        // Each missing entry points at its OWN array element.
        assert_eq!(selected(source, published[1].range), "\"A.as\"");
        assert_eq!(selected(source, published[2].range), "\"B.as\"");
        assert_ranges_within(source, &published);
    }

    /// A stale published set is worse than none: once the manifest is valid
    /// the server must publish an EMPTY set for it, not simply stop talking.
    #[tokio::test(flavor = "current_thread")]
    async fn fixing_the_manifest_republishes_an_empty_set() {
        let root = manifest_tree("manifest-stale-clear", NO_VERSION);
        let uri = manifest_uri_of(&root);
        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();

        let broken = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_open(open_params(&uri, NO_VERSION)),
        )
        .await;
        assert!(!broken.diagnostics.is_empty(), "control: starts broken");

        let fixed = "# Counter\n[meta]\nname = \"Counter\"\nversion = \"1.0\"\n";
        fs::write(root.join("info.toml"), fixed).unwrap();
        let cleared = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_change(change_params(&uri, 2, fixed)),
        )
        .await;
        assert_eq!(
            cleared.diagnostics,
            Vec::<Diagnostic>::new(),
            "a fixed manifest must clear its squiggles"
        );
    }

    /// Closing a manifest discards its buffer overlay. The client must then
    /// receive the on-disk diagnostic set, or diagnostics from an unsaved
    /// edit stay stale until another unrelated workspace event happens.
    #[tokio::test(flavor = "current_thread")]
    async fn closing_manifest_republishes_the_on_disk_diagnostics() {
        let valid = "[meta]\nversion = \"1.0\"\n";
        let invalid = "[meta]\nname = \"Counter\"\n";

        for (label, disk, buffer, expects_error) in [
            ("discard-invalid-buffer", valid, invalid, false),
            ("discard-valid-buffer", invalid, valid, true),
        ] {
            let root = manifest_tree(&format!("manifest-close-{label}"), disk);
            let uri = manifest_uri_of(&root);
            let (_env, mut service, mut socket) = test_service();
            initialize_service(
                &mut service,
                init_params(&root, Some(vec![root.to_path_buf()])),
            )
            .await;
            let backend = service.inner();

            let buffered = capture_diagnostics_for(
                &mut socket,
                &uri,
                backend.did_open(open_params(&uri, buffer)),
            )
            .await;
            assert_eq!(
                buffered
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("[meta].version")),
                !expects_error,
                "{label}: the open buffer must be the initial source of truth"
            );

            let after_close = capture_diagnostics_for(
                &mut socket,
                &uri,
                backend.did_close(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                }),
            )
            .await;
            assert_eq!(
                after_close
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("[meta].version")),
                expects_error,
                "{label}: closing must restore the on-disk manifest diagnostics"
            );
        }
    }

    /// Export-file changes commonly arrive through the watched-file channel
    /// (renames outside the editor, generators, VCS operations). Rebuilding
    /// the shard alone is not enough: the manifest URI needs a new published
    /// set for both the missing and restored transitions.
    #[tokio::test(flavor = "current_thread")]
    async fn watched_export_changes_republish_manifest_diagnostics() {
        let source = "[meta]\nversion = \"1.0\"\n\n[script]\nexports = [\"Ex.as\"]\n";
        let root = manifest_tree("manifest-watched-export", source);
        let uri = manifest_uri_of(&root);
        let export = root.join("src/Ex.as");
        fs::write(&export, "void Exported() {}\n").unwrap();
        let export_uri = Url::from_file_path(&export).unwrap();
        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();

        let clean = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_open(open_params(&uri, source)),
        )
        .await;
        assert_eq!(
            clean.diagnostics,
            Vec::<Diagnostic>::new(),
            "control: export exists"
        );

        fs::remove_file(&export).unwrap();
        let missing = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_change_watched_files(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(export_uri.clone(), FileChangeType::DELETED)],
            }),
        )
        .await;
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|d| d.message == "Export file not found: Ex.as"),
            "a watched deletion must refresh manifest diagnostics: {:?}",
            missing.diagnostics
        );

        fs::write(&export, "void Exported() {}\n").unwrap();
        let restored = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_change_watched_files(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(export_uri, FileChangeType::CREATED)],
            }),
        )
        .await;
        assert_eq!(
            restored.diagnostics,
            Vec::<Diagnostic>::new(),
            "a watched creation must clear the export diagnostic"
        );
    }

    /// A manifest opened through a symlink URI is the document the editor can
    /// display. A manifest watcher refresh must keep using that URI and its
    /// unsaved buffer, not publish the on-disk result to the canonical path.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn manifest_watch_republishes_the_open_symlink_alias() {
        use std::os::unix::fs::symlink;

        let valid = "[meta]\nversion = \"1.0\"\n";
        let alias_buffer = "[meta]\nversion = \"1.0\"\n\n[script]\nexports = [\"Ex.as\"]\n";
        let root = manifest_tree("manifest-watch-alias", valid);
        fs::write(root.join("src/Ex.as"), "void Exported() {}\n").unwrap();
        let aliases = temp_tree("manifest-watch-alias-dir");
        let alias_path = aliases.join("info.toml");
        symlink(root.join("info.toml"), &alias_path).unwrap();
        let alias_uri = Url::from_file_path(&alias_path).unwrap();
        let manifest_uri = manifest_uri_of(&root);
        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();

        let buffered = capture_diagnostics_for(
            &mut socket,
            &alias_uri,
            backend.did_open(open_params(&alias_uri, alias_buffer)),
        )
        .await;
        assert_eq!(
            buffered.diagnostics,
            Vec::<Diagnostic>::new(),
            "the real workspace's src/Ex.as must resolve for an alias buffer: {:?}",
            buffered.diagnostics
        );

        let source_uri = Url::from_file_path(root.join("src/Main.as")).unwrap();
        let after_source_open = capture_diagnostics_for(
            &mut socket,
            &alias_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: source_uri,
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Main() {}\n".into(),
                },
            }),
        )
        .await;
        assert_eq!(
            after_source_open.diagnostics,
            Vec::<Diagnostic>::new(),
            "a source edit must retain the real-root export resolution for the alias manifest: {:?}",
            after_source_open.diagnostics
        );

        let refreshed = capture_diagnostics_for(
            &mut socket,
            &alias_uri,
            backend.did_change_watched_files(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(manifest_uri, FileChangeType::CHANGED)],
            }),
        )
        .await;
        assert_eq!(
            refreshed.diagnostics,
            Vec::<Diagnostic>::new(),
            "manifest watch must keep the visible alias URI while resolving exports from the real root: {:?}",
            refreshed.diagnostics
        );
    }

    /// A manifest the TOML parser rejects is the loudest failure there is —
    /// the plugin cannot load at all. It must be published, not skipped.
    #[tokio::test(flavor = "current_thread")]
    async fn malformed_manifest_publishes_a_parse_error() {
        let source = "[meta]\nname = \"T\"\nversion = = \"1.0\"\n";
        let root = manifest_tree("manifest-malformed", source);
        let published = manifest_diagnostics_on_open(&root, source).await;

        assert_eq!(published.len(), 1, "{published:?}");
        assert_eq!(published[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(
            published[0].message.contains("TOML parse error"),
            "{published:?}"
        );
        assert_ranges_within(source, &published);
    }

    /// Generalise the parse-error case over the ways TOML can be broken:
    /// none of them may panic, and every one must produce an in-bounds range.
    #[tokio::test(flavor = "current_thread")]
    async fn malformed_manifests_never_panic_and_stay_in_bounds() {
        for (label, source) in [
            ("unterminated table", "[meta\nname = \"T\"\n"),
            ("bare garbage", "this is not toml [[[\n"),
            (
                "duplicate key",
                "[meta]\nversion = \"1\"\nversion = \"2\"\n",
            ),
            ("unterminated string", "[meta]\nname = \"T\n"),
            ("empty file", ""),
            ("wrong type", "[meta]\nversion = 3\n"),
        ] {
            let root = manifest_tree(
                &format!("manifest-broken-{}", label.replace(' ', "-")),
                source,
            );
            let published = manifest_diagnostics_on_open(&root, source).await;
            assert!(
                !published.is_empty(),
                "{label}: malformed or invalid TOML must publish a parse or validation diagnostic"
            );
            assert_ranges_within(source, &published);
        }
    }

    /// Editing a `.as` file refreshes its root's manifest diagnostics too —
    /// the manifest is validated against files on disk, so renaming an
    /// export away has to show up without opening info.toml.
    #[tokio::test(flavor = "current_thread")]
    async fn editing_a_source_file_refreshes_its_root_manifest_diagnostics() {
        let source = "[meta]\nversion = \"1.0\"\n\n[script]\nexports = [\"Gone.as\"]\n";
        let root = manifest_tree("manifest-from-source-edit", source);
        let manifest_uri = manifest_uri_of(&root);
        let main_uri = Url::from_file_path(root.join("src/Main.as")).unwrap();

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        let published = capture_diagnostics_for(
            &mut socket,
            &manifest_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: main_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Main() {}\n".into(),
                },
            }),
        )
        .await;
        assert!(
            published
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Gone.as")),
            "manifest diagnostics must follow a source edit; got {:?}",
            published.diagnostics
        );
    }

    /// Manifest problems belong to info.toml and nowhere else: attaching
    /// them to a `.as` file would put them where they cannot be fixed.
    #[tokio::test(flavor = "current_thread")]
    async fn manifest_diagnostics_never_attach_to_source_files() {
        let source = "[meta]\n\n[script]\nexports = [\"Gone.as\"]\n";
        let root = manifest_tree("manifest-not-on-sources", source);
        let main_uri = Url::from_file_path(root.join("src/Main.as")).unwrap();

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        let published = capture_diagnostics_for(
            &mut socket,
            &main_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: main_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Main() {}\n".into(),
                },
            }),
        )
        .await;
        assert_eq!(
            published.diagnostics,
            Vec::<Diagnostic>::new(),
            "the source file carries no manifest findings"
        );
    }

    /// No info.toml at all is not an error — plenty of folders are opened
    /// that are not plugins.
    #[tokio::test(flavor = "current_thread")]
    async fn a_workspace_without_a_manifest_publishes_no_manifest_diagnostics() {
        let root = temp_tree("manifest-absent");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        let main_uri = Url::from_file_path(root.join("src/Main.as")).unwrap();

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();
        let published = capture_diagnostics_for(
            &mut socket,
            &main_uri,
            backend.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: main_uri.clone(),
                    language_id: "angelscript".into(),
                    version: 1,
                    text: "void Main() {}\n".into(),
                },
            }),
        )
        .await;
        assert_eq!(published.diagnostics, Vec::<Diagnostic>::new());
    }

    /// Multi-root: each plugin's problems go to ITS OWN info.toml URI.
    #[tokio::test(flavor = "current_thread")]
    async fn each_root_publishes_its_own_manifest_diagnostics() {
        let parent = temp_tree("manifest-multi-root");
        let a_source = "[meta]\nname = \"A\"\n";
        let b_source = "[meta]\nversion = \"1\"\n\n[script]\nexports = [\"Gone.as\"]\n";
        let mut roots = Vec::new();
        for (name, source) in [("plugin-a", a_source), ("plugin-b", b_source)] {
            let root = parent.join(name);
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(root.join("info.toml"), source).unwrap();
            fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
            roots.push(root);
        }

        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&parent, Some(vec![parent.to_path_buf()])),
        )
        .await;
        let backend = service.inner();

        for (root, source, want) in [
            (
                &roots[0],
                a_source,
                "Missing required field: [meta].version",
            ),
            (&roots[1], b_source, "Export file not found: Gone.as"),
        ] {
            let uri = manifest_uri_of(root);
            let published = capture_diagnostics_for(
                &mut socket,
                &uri,
                backend.did_open(open_params(&uri, source)),
            )
            .await;
            let messages: Vec<&str> = published
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect();
            assert_eq!(messages, vec![want], "{}", root.display());
        }
    }

    /// A dependency that lives inside a `.op` archive has a manifest too,
    /// and it may well be invalid — but the user cannot open a path inside
    /// a ZIP, so nothing may be published against one.
    #[tokio::test(flavor = "current_thread")]
    async fn op_archive_manifests_are_never_published_against() {
        let parent = temp_tree("manifest-op-archive");
        let plugins = parent.join("Plugins");
        fs::create_dir_all(&plugins).unwrap();
        // The archived dependency's own manifest is missing `version` and
        // names an export that is not in the archive.
        {
            use std::io::Write;
            let file = fs::File::create(plugins.join("ArchivedDep.op")).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("info.toml", opts).unwrap();
            zip.write_all(
                b"[meta]\nname = \"ArchivedDep\"\n\n[script]\nmodule = \"ArchivedDep\"\nexports = [\"Gone.as\"]\n",
            )
            .unwrap();
            zip.finish().unwrap();
        }

        let root = parent.join("plugin");
        fs::create_dir_all(root.join("src")).unwrap();
        let source = "[meta]\nname = \"Consumer\"\nversion = \"1\"\n\n[script]\ndependencies = [\"ArchivedDep\"]\n";
        fs::write(root.join("info.toml"), source).unwrap();
        fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        fs::write(
            root.join(".openplanet-lsp.toml"),
            format!("plugins_dir = \"{}\"\n", plugins.display()),
        )
        .unwrap();

        let uri = manifest_uri_of(&root);
        let (_env, mut service, mut socket) = test_service();
        initialize_service(&mut service, init_params(&root, Some(vec![root.clone()]))).await;
        let backend = service.inner();
        let published = capture_diagnostics_for(
            &mut socket,
            &uri,
            backend.did_open(open_params(&uri, source)),
        )
        .await;

        // Nothing from the archive's manifest leaked into the consumer's,
        // and the archive's own path was never a publish target.
        assert_eq!(
            published.diagnostics,
            Vec::<Diagnostic>::new(),
            "{:?}",
            published.diagnostics
        );
        assert!(
            !published.uri.path().contains(".op"),
            "published against {}",
            published.uri
        );
    }

    /// tower-lsp suppresses every client message until the `initialize`
    /// RESPONSE has gone out, so anything published from inside `initialize`
    /// is dropped on the floor. The manifest set has to be (re)published
    /// from `initialized`, or a project whose info.toml is broken shows an
    /// empty Problems panel until the user happens to open a file.
    #[tokio::test(flavor = "current_thread")]
    async fn manifest_diagnostics_are_published_on_initialized_without_opening_a_file() {
        let root = manifest_tree("manifest-on-initialized", NO_VERSION);
        let uri = manifest_uri_of(&root);
        let (_env, mut service, mut socket) = test_service();
        initialize_service(
            &mut service,
            init_params(&root, Some(vec![root.to_path_buf()])),
        )
        .await;
        let backend = service.inner();

        let published =
            capture_diagnostics_for(&mut socket, &uri, backend.initialized(InitializedParams {}))
                .await;
        assert!(
            published
                .diagnostics
                .iter()
                .any(|d| d.message.contains("[meta].version")),
            "no file was opened, so `initialized` is the only chance to \
             publish; got {:?}",
            published.diagnostics
        );
    }
}
