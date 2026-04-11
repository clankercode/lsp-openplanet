pub mod code_actions;
pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod formatter;
pub mod highlights;
pub mod hover;
pub mod inlay_hints;
pub mod navigation;
pub mod references;
pub mod scope_query;
pub mod semantic_tokens;
pub mod signature;
pub mod symbols;

use std::collections::HashMap;
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
    pub fn new(client: Client) -> Self {
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

    /// Build an ad-hoc workspace snapshot from the currently open documents.
    ///
    /// Each open document is preprocessed, parsed, and its symbols extracted
    /// into a fresh `SymbolTable`. The returned map carries `file_id → (uri,
    /// source)` so callers can translate cross-file spans into concrete
    /// `Location`s.
    async fn build_workspace(&self) -> (SymbolTable, HashMap<usize, (Url, String)>) {
        let defines = {
            let config = self.config.read().await;
            config.defines.clone()
        };
        let mut table = SymbolTable::new();
        let mut files = HashMap::new();
        for entry in self.documents.iter() {
            let uri = entry.key().clone();
            let source = entry.value().clone();
            let pp = crate::preprocessor::preprocess(&source, &defines);
            let tokens = crate::lexer::tokenize_filtered(&pp.masked_source);
            let mut parser = crate::parser::Parser::new(&tokens, &pp.masked_source);
            let file = parser.parse_file();
            let fid = table.allocate_file_id();
            let symbols = SymbolTable::extract_symbols(fid, &pp.masked_source, &file);
            table.set_file_symbols(fid, symbols);
            files.insert(fid, (uri, source));
        }
        (table, files)
    }

    async fn on_change(&self, uri: &Url, text: &str) {
        let (workspace, _files) = self.build_workspace().await;
        let config = self.config.read().await;
        let type_index = self.type_index.read().await;
        let diags = diagnostics::compute_diagnostics(
            uri,
            text,
            &config,
            type_index.as_deref(),
            Some(&workspace),
        );
        drop(type_index);
        drop(config);
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
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => String::new(),
        };
        let type_index = self.type_index.read().await;
        let (table, _files) = self.build_workspace().await;
        let items = completion::complete(&source, pos, type_index.as_deref(), Some(&table));
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
        let (table, _files) = self.build_workspace().await;
        Ok(hover::hover(&source, pos, type_index.as_deref(), Some(&table)))
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
        let (table, files) = self.build_workspace().await;
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        Ok(navigation::goto_definition(&source, pos, &table, &workspace_files)
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let (_table, files) = self.build_workspace().await;
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        let refs = navigation::find_references(
            &source,
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
        Ok(highlights::document_highlights(&source, pos))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let (table, _files) = self.build_workspace().await;
        let type_index = self.type_index.read().await;
        Ok(signature::signature_help(
            &source,
            pos,
            type_index.as_deref(),
            Some(&table),
        ))
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

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let (table, files) = self.build_workspace().await;
        Ok(Some(symbols::workspace_symbols(
            &params.query,
            &table,
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
        let (_table, files) = self.build_workspace().await;
        let workspace_files = navigation::WorkspaceFiles { files: &files };
        Ok(navigation::rename(
            &source,
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
        let tokens = semantic_tokens::semantic_tokens(&source);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
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

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let (table, _files) = self.build_workspace().await;
        let type_index = self.type_index.read().await;
        let hints = inlay_hints::inlay_hints(
            &source,
            params.range,
            type_index.as_deref(),
            Some(&table),
        );
        Ok(Some(hints))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.documents.get(uri) {
            Some(doc) => doc.value().clone(),
            None => return Ok(None),
        };
        let (workspace, _files) = self.build_workspace().await;
        let type_index_guard = self.type_index.read().await;
        let type_index = type_index_guard.as_deref();
        let actions = code_actions::code_actions(
            uri,
            &source,
            params.range,
            &params.context.diagnostics,
            &workspace,
            type_index,
        );
        Ok(Some(actions))
    }
}

pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
