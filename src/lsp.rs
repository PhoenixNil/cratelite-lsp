use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tower_lsp::{jsonrpc::Result, lsp_types::*, Client, LanguageServer};

use crate::crate_index::CrateIndex;
use crate::feature_index::FeatureIndex;
use crate::toml_context;

pub struct Backend {
    _client: Client,
    documents: Arc<RwLock<HashMap<Url, String>>>,
    crate_index: Arc<CrateIndex>,
    feature_index: Arc<FeatureIndex>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            _client: client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            crate_index: CrateIndex::new(),
            feature_index: Arc::new(FeatureIndex::new()),
        }
    }
}

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(
                        IntoIterator::into_iter(['"', '\'', ',', '-', '_'])
                            .chain('a'..='z')
                            .map(|c| c.to_string())
                            .collect(),
                    ),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "cratelite-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let index = self.crate_index.clone();
        tokio::spawn(async move {
            index.initialize().await;
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    // ── document sync ──────────────────────────────────────────────────────

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.documents.write().await.insert(uri, text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents.write().await.insert(uri, change.text);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
    }

    // ── completions ────────────────────────────────────────────────────────

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;

        // Only handle Cargo.toml files
        let path = uri.path().to_lowercase();
        if !path.ends_with("cargo.toml") {
            return Ok(None);
        }

        let text = {
            let docs = self.documents.read().await;
            match docs.get(uri) {
                Some(t) => t.clone(),
                None => return Ok(None),
            }
        };

        let Position { line, character } = params.text_document_position.position;

        let Some(context) = toml_context::get_completion_context(&text, line, character) else {
            return Ok(None);
        };

        match context {
            //Features completions
            toml_context::CompletionContext::Feature(ctx) => {
                let features = self
                    .feature_index
                    .get_features(&ctx.crate_name, &ctx.version_requirement)
                    .await;

                let items: Vec<CompletionItem> = features
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| f.starts_with(&ctx.feature_prefix))
                    .filter(|f| !ctx.selected_features.contains(f))
                    .map(|f| CompletionItem {
                        label: f.clone(),
                        kind: Some(CompletionItemKind::VALUE),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: ctx.range,
                            new_text: f,
                        })),
                        ..Default::default()
                    })
                    .collect();

                Ok(Some(CompletionResponse::Array(items)))
            }
            // Crate name completions
            toml_context::CompletionContext::CrateName(ctx) => {
                let results = self.crate_index.search(&ctx.prefix, 30).await;
                let items: Vec<CompletionItem> = results
                    .into_iter()
                    .map(|e| CompletionItem {
                        label: e.name.clone(),
                        kind: Some(CompletionItemKind::MODULE),
                        detail: Some(format!("v{}", e.version)),
                        sort_text: Some(format!("{:08}", e.rank)),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range::new(
                                Position::new(line, ctx.start_character),
                                Position::new(line, ctx.end_character),
                            ),
                            new_text: e.name,
                        })),
                        ..Default::default()
                    })
                    .collect();

                Ok(Some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })))
            }
            // Version completions
            toml_context::CompletionContext::Version(ctx) => {
                if let Some(version) = self.crate_index.get_latest_version(&ctx.crate_name).await {
                    if version.starts_with(&ctx.version_prefix) {
                        let item = CompletionItem {
                            label: version.clone(),
                            kind: Some(CompletionItemKind::VALUE),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: ctx.range,
                                new_text: version,
                            })),
                            ..Default::default()
                        };
                        return Ok(Some(CompletionResponse::Array(vec![item])));
                    }
                }

                Ok(None)
            }
        }
    }
}
