pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod hover;
pub mod references;
pub mod signature;
pub mod symbols;

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::config::LspConfig;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub struct Backend {
    client: Client,
    config: tokio::sync::RwLock<LspConfig>,
    type_index: tokio::sync::RwLock<Option<Arc<TypeIndex>>>,
    #[allow(dead_code)]
    symbol_table: tokio::sync::RwLock<SymbolTable>,
    /// Open document contents: URI → source text
    documents: DashMap<Url, String>,
    /// File path → file ID mapping
    #[allow(dead_code)]
    file_ids: DashMap<PathBuf, usize>,
    workspace_root: tokio::sync::RwLock<Option<PathBuf>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            config: tokio::sync::RwLock::new(LspConfig::load(None, None)),
            type_index: tokio::sync::RwLock::new(None),
            symbol_table: tokio::sync::RwLock::new(SymbolTable::new()),
            documents: DashMap::new(),
            file_ids: DashMap::new(),
            workspace_root: tokio::sync::RwLock::new(None),
        }
    }

    async fn on_change(&self, uri: &Url, text: &str) {
        let config = self.config.read().await;
        let diags = diagnostics::compute_diagnostics(uri, text, &config);
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;
    }
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
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("OpenPlanet LSP initialized");
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
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri);
        let source = doc.as_ref().map(|d| d.value().as_str()).unwrap_or("");
        let type_index = self.type_index.read().await;
        let items = completion::complete(source, pos, type_index.as_deref());
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri);
        let source = doc.as_ref().map(|d| d.value().as_str()).unwrap_or("");
        let type_index = self.type_index.read().await;
        Ok(hover::hover(source, pos, type_index.as_deref()))
    }

    async fn goto_definition(
        &self,
        _params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        Ok(None) // Task 16
    }

    async fn references(&self, _params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        Ok(None) // Task 16
    }

    async fn signature_help(&self, _params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        Ok(None) // Task 16
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri);
        let source = doc.as_ref().map(|d| d.value().as_str()).unwrap_or("");
        Ok(symbols::document_symbols(source))
    }
}

pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
