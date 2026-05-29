//! tower-lsp `LanguageServer` implementation: a thin async adapter over the pure
//! `analysis` layer plus a document store.
//!
//! The backend is `Send + Sync` because it holds only `Send + Sync` types: the
//! `Client`, and a `tokio::sync::Mutex<HashMap<Url, String>>` of document text. No
//! interpreter state (`Rc`/`RefCell`/`Value`) is ever held, so this compiles on the
//! `current_thread` tokio runtime AND satisfies tower-lsp's `Send + Sync` bounds.

use crate::lsp::analysis;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

pub struct Backend {
    client: Client,
    documents: Mutex<HashMap<Url, String>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend { client, documents: Mutex::new(HashMap::new()) }
    }

    /// Store the document text and publish its diagnostics.
    async fn analyze_and_publish(&self, uri: Url, text: String, version: Option<i32>) {
        let diags = analysis::diagnostics(&text);
        self.documents.lock().await.insert(uri.clone(), text);
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}

/// The set of capabilities the server advertises. Factored out so it can be unit
/// tested without a live `Client`. Task 1 advertises full-document text sync;
/// later tasks add completion/hover/definition/documentSymbol providers.
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_symbol_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            // `.` for member-access completions; `"` / `'` for import-path strings.
            trigger_characters: Some(vec![".".to_string(), "\"".to_string(), "'".to_string()]),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            server_info: Some(ServerInfo {
                name: "ascript".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "ascript language server initialized")
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.analyze_and_publish(doc.uri, doc.text, Some(doc.version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Full-document sync: the last content change holds the entire new text.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.analyze_and_publish(
                params.text_document.uri,
                change.text,
                Some(params.text_document.version),
            )
            .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.lock().await.remove(&uri);
        // Clear diagnostics for the closed document.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> tower_lsp::jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let docs = self.documents.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };
        let symbols = analysis::document_symbols(text);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        Ok(analysis::hover(text, offset))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let docs = self.documents.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        let items = analysis::completions(text, offset);
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        let Some(range) = analysis::definition(text, offset) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location { uri, range })))
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_advertise_full_sync() {
        let caps = server_capabilities();
        match caps.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(kind)) => {
                assert_eq!(kind, TextDocumentSyncKind::FULL);
            }
            other => panic!("expected FULL text document sync, got {:?}", other),
        }
    }

    #[test]
    fn capabilities_advertise_document_symbol_and_hover() {
        let caps = server_capabilities();
        assert!(
            matches!(
                caps.document_symbol_provider,
                Some(OneOf::Left(true)) | Some(OneOf::Right(_))
            ),
            "expected a document symbol provider, got {:?}",
            caps.document_symbol_provider
        );
        assert!(
            caps.hover_provider.is_some(),
            "expected a hover provider, got {:?}",
            caps.hover_provider
        );
    }

    #[test]
    fn capabilities_advertise_completion_and_definition() {
        let caps = server_capabilities();
        let completion = caps.completion_provider.expect("expected a completion provider");
        let triggers = completion.trigger_characters.expect("expected trigger characters");
        assert!(triggers.contains(&".".to_string()), "triggers: {triggers:?}");
        assert!(
            triggers.contains(&"\"".to_string()) || triggers.contains(&"'".to_string()),
            "expected a quote trigger char, got {triggers:?}"
        );
        assert!(
            matches!(
                caps.definition_provider,
                Some(OneOf::Left(true)) | Some(OneOf::Right(_))
            ),
            "expected a definition provider, got {:?}",
            caps.definition_provider
        );
    }
}
