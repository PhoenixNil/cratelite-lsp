use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

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
                        // Trigger on characters that commonly appear at the start
                        // of a crate name or inside feature/version strings.
                        vec![
                            "\"".into(), "'".into(), ",".into(),
                            "-".into(), "_".into(),
                            "a".into(), "b".into(), "c".into(), "d".into(), "e".into(),
                            "f".into(), "g".into(), "h".into(), "i".into(), "j".into(),
                            "k".into(), "l".into(), "m".into(), "n".into(), "o".into(),
                            "p".into(), "q".into(), "r".into(), "s".into(), "t".into(),
                            "u".into(), "v".into(), "w".into(), "x".into(), "y".into(),
                            "z".into(),
                        ],
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
        self.documents.write().await.remove(&params.text_document.uri);
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

        // ── guard: must be in a [dependencies] section ─────────────────────
        if !toml_context::is_in_dependencies_section(&text, line) {
            return Ok(None);
        }

        // ── 1. Feature completions (inline table: `{ version = "…", features = ["…"] }`)
        if let Some(ctx) = toml_context::get_feature_completion_context(&text, line, character) {
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

            return Ok(Some(CompletionResponse::Array(items)));
        }

        // ── 2. Crate-name completions (typing the key on the left of `=`) ──
        let line_text: &str = text.lines().nth(line as usize).unwrap_or("");
        if toml_context::is_typing_crate_name(line_text, character) {
            let ctx = toml_context::get_crate_name_context(line_text, character);
            if ctx.prefix.len() >= 2 {
                let results = self.crate_index.search(&ctx.prefix, 30).await;
                let items: Vec<CompletionItem> = results
                    .into_iter()
                    .map(|e| {
                        let insert = format!("{} = \"{}\"", e.name, e.version);
                        CompletionItem {
                            label: e.name.clone(),
                            kind: Some(CompletionItemKind::MODULE),
                            detail: Some(format!("v{}", e.version)),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range::new(
                                    Position::new(line, ctx.start_character),
                                    Position::new(line, ctx.end_character),
                                ),
                                new_text: insert,
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();
                return Ok(Some(CompletionResponse::Array(items)));
            }
            return Ok(None);
        }

        // ── 3. Version-string completion (`crate_name = "…"`) ──────────────
        if let Some(ctx) = toml_context::get_version_context(&text, line, character) {
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
        }

        Ok(None)
    }
}
