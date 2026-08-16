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

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::LspConfig;
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
    /// read the same consistent view (GH #39).
    snapshot: tokio::sync::RwLock<Arc<AnalysisSnapshot>>,
    workspace_root: tokio::sync::RwLock<Option<PathBuf>>,
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
                        }
                    }
                }
            }
            // No workspace root: open documents only.
            None => crate::workspace::load::PluginWorkspaceLoad {
                root: PathBuf::new(),
                files: open_into_files(open),
                missing_required_dependencies: Vec::new(),
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
        let snapshot = self.refresh_snapshot().await;
        let config = self.config.read().await;
        let type_index = self.type_index.read().await;
        let mut diags = match snapshot.analysis_of(uri) {
            // Parsed as part of the snapshot build — reuse the analysis.
            Some(analysis) => diagnostics::compute_diagnostics_from_analysis(
                uri,
                analysis,
                &config,
                type_index.as_deref(),
                Some(snapshot.symbols()),
            ),
            // Not on disk (e.g. untitled documents) — parse on the fly.
            None => diagnostics::compute_diagnostics(
                uri,
                text,
                &config,
                type_index.as_deref(),
                Some(snapshot.symbols()),
            ),
        };
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

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Set workspace root
        if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                *self.workspace_root.write().await = Some(path.clone());
                let config = LspConfig::load(Some(&path), params.initialization_options.as_ref());
                *self.config.write().await = config;
            }
        }

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
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
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
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("OpenPlanet LSP initialized");
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
        // see the on-disk contents again.
        self.refresh_snapshot().await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => String::new(),
        };
        let type_index = self.type_index.read().await;
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let items = completion::complete(
            &analysis,
            pos,
            type_index.as_deref(),
            Some(snapshot.symbols()),
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        Ok(hover::hover(
            &analysis,
            pos,
            type_index.as_deref(),
            Some(snapshot.symbols()),
        ))
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        Ok(
            navigation::goto_definition(&analysis, pos, snapshot.symbols(), &workspace_files)
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        Ok(highlights::document_highlights(&analysis, pos))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        Ok(signature::signature_help(
            &analysis,
            pos,
            type_index.as_deref(),
            Some(snapshot.symbols()),
        ))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let snapshot = self.snapshot.read().await.clone();
        let empty = crate::analysis::DocumentAnalysis::analyze_plain("");
        let analysis = snapshot.analysis_of(uri).unwrap_or(&empty);
        Ok(symbols::document_symbols(analysis))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let snapshot = self.snapshot.read().await.clone();
        let files = snapshot.uri_map();
        Ok(Some(symbols::workspace_symbols(
            &params.query,
            snapshot.symbols(),
            &files,
        )))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
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
        let snapshot = self.snapshot.read().await.clone();
        let type_index = self.type_index.read().await;
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let hints = inlay_hints::inlay_hints(
            &analysis,
            params.range,
            type_index.as_deref(),
            Some(snapshot.symbols()),
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
        let snapshot = self.snapshot.read().await.clone();
        let owned =
            crate::analysis::DocumentAnalysis::analyze_plain(&source);
        let analysis = snapshot.analysis_of(uri).unwrap_or(&owned);
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        let items = call_hierarchy::prepare(&analysis, uri, pos, snapshot.symbols(), &ws_files);
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
        let snapshot = self.snapshot.read().await.clone();
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        Ok(Some(call_hierarchy::incoming(
            &params.item,
            snapshot.symbols(),
            &ws_files,
        )))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let snapshot = self.snapshot.read().await.clone();
        let files = snapshot.uri_map();
        let ws_files = navigation::WorkspaceFiles { files: &files };
        Ok(Some(call_hierarchy::outgoing(
            &params.item,
            snapshot.symbols(),
            &ws_files,
        )))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let snapshot = self.snapshot.read().await.clone();
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
