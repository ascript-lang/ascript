//! tower-lsp `LanguageServer` implementation: a thin async adapter over the pure
//! `analysis` layer plus a cached `SemanticModel` document store.
//!
//! The backend is `Send + Sync` because it holds only `Send + Sync` types: the
//! `Client`, and a `tokio::sync::Mutex<DocumentStore>` of per-document semantic
//! models. No interpreter state (`Rc`/`RefCell`/`Value`) is ever held, so this
//! compiles on the `current_thread` tokio runtime AND satisfies tower-lsp's
//! `Send + Sync` bounds.

use crate::lsp::analysis;
use crate::lsp::model::DocumentStore;
use crate::lsp::workspace::{self, WorkspaceIndex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

pub struct Backend {
    client: Client,
    documents: Mutex<DocumentStore>,
    /// The cross-file symbol index (SP4 §4). `RwLock` (not the interpreter's
    /// `RefCell`) keeps the backend `Send + Sync`; it holds only owned
    /// `String`/`PathBuf`/range data, never a `Value`.
    index: RwLock<WorkspaceIndex>,
    /// Workspace root folders captured in `initialize`, walked for `*.as` files
    /// when the index is first built.
    roots: RwLock<Vec<PathBuf>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            documents: Mutex::new(DocumentStore::new()),
            index: RwLock::new(WorkspaceIndex::new()),
            roots: RwLock::new(Vec::new()),
        }
    }

    /// Incrementally re-index the file behind `uri` (if it maps to a path).
    fn reindex_uri(&self, uri: &Url, text: &str) {
        if let Some(path) = url_to_canon(uri) {
            if let Ok(mut idx) = self.index.write() {
                idx.reindex_file(&path, text);
            }
        }
    }

    /// Build/cache the document's `SemanticModel` and publish its diagnostics.
    /// Merges the model's config-aware single-file diagnostics with the
    /// index-backed file-module call-arity (D-arity), which the single-file
    /// analysis cannot compute.
    async fn analyze_and_publish(&self, uri: Url, text: String, version: Option<i32>) {
        let mut diags;
        {
            let mut store = self.documents.lock().await;
            store.set(uri.clone(), text.clone(), version);
            diags = store.get(&uri).map(|m| m.lsp_diagnostics()).unwrap_or_default();
        }
        // Index-backed cross-file arity (cannot be computed single-file).
        if let Some(path) = url_to_canon(&uri) {
            if let Ok(idx) = self.index.read() {
                for d in idx.file_module_arity(&path, &text) {
                    diags.push(analysis::byte_diagnostic_to_lsp(&text, &d));
                }
            }
        }
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
        // SP4 §4: cross-file navigation providers backed by the workspace index.
        workspace_symbol_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        ..ServerCapabilities::default()
    }
}

/// Convert a `Url` to a canonical filesystem path key for the index.
fn url_to_canon(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok().map(|p| workspace::canon(&p))
}

/// Convert a canonical path back into a `file://` `Url`.
fn canon_to_url(path: &std::path::Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        // Capture workspace root folders for index discovery (SP4 §4).
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(folders) = &params.workspace_folders {
            for f in folders {
                if let Ok(p) = f.uri.to_file_path() {
                    roots.push(p);
                }
            }
        }
        #[allow(deprecated)]
        if roots.is_empty() {
            if let Some(uri) = &params.root_uri {
                if let Ok(p) = uri.to_file_path() {
                    roots.push(p);
                }
            }
        }
        if let Ok(mut guard) = self.roots.write() {
            *guard = roots;
        }
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            server_info: Some(ServerInfo {
                name: "ascript".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        // Warm the cross-file index: walk each root for `*.as` and index them.
        let roots = self.roots.read().map(|r| r.clone()).unwrap_or_default();
        let mut files: Vec<(PathBuf, String)> = Vec::new();
        for root in &roots {
            for path in workspace::discover_as_files(root) {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    files.push((path, text));
                }
            }
        }
        if let Ok(mut idx) = self.index.write() {
            for (path, text) in &files {
                idx.reindex_file(path, text);
            }
        }
        self.client
            .log_message(MessageType::INFO, "ascript language server initialized")
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.reindex_uri(&doc.uri, &doc.text);
        self.analyze_and_publish(doc.uri, doc.text, Some(doc.version))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Full-document sync: the last content change holds the entire new text.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.reindex_uri(&params.text_document.uri, &change.text);
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
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
            return Ok(None);
        };
        let symbols = analysis::document_symbols(text);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
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
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
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
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        // SP4 §4: try the cross-file index first — if the use at the cursor is an
        // imported/cross-file name, return a `Location` in the TARGET file.
        if let Some(path) = url_to_canon(&uri) {
            let xfile = self
                .index
                .read()
                .ok()
                .and_then(|idx| idx.definition_at(&path, char_to_byte(text, offset)).map(
                    |(def_path, span)| {
                        let target_text = idx_text(&idx, &def_path).unwrap_or_default();
                        (def_path, workspace::byte_span_to_range(&target_text, span))
                    },
                ));
            if let Some((def_path, range)) = xfile {
                if let Some(target_uri) = canon_to_url(&def_path) {
                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: target_uri,
                        range,
                    })));
                }
            }
        }
        // Fall back to the single-file analysis (same-file local/decl).
        let Some(range) = analysis::definition(text, offset) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri,
            range,
        })))
    }

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_decl = params.context.include_declaration;
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        let byte = char_to_byte(text, offset);
        let Some(path) = url_to_canon(&uri) else {
            return Ok(None);
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let refs = idx.references_at(&path, byte, include_decl);
        let mut locations = Vec::new();
        for (p, span) in refs {
            let Some(file_uri) = canon_to_url(&p) else {
                continue;
            };
            let file_text = idx_text(&idx, &p).unwrap_or_default();
            locations.push(Location {
                uri: file_uri,
                range: workspace::byte_span_to_range(&file_text, span),
            });
        }
        Ok(Some(locations))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<SymbolInformation>>> {
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for def in idx.workspace_symbols(&params.query) {
            let Some(uri) = canon_to_url(&def.path) else {
                continue;
            };
            let text = idx_text(&idx, &def.path).unwrap_or_default();
            #[allow(deprecated)]
            out.push(SymbolInformation {
                name: def.name.clone(),
                kind: def_symbol_kind(def.kind),
                tags: None,
                deprecated: None,
                location: Location {
                    uri,
                    range: workspace::byte_span_to_range(&text, def.name_range),
                },
                container_name: None,
            });
        }
        Ok(Some(out))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let position = params.position;
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        let byte = char_to_byte(text, offset);
        let Some(path) = url_to_canon(&uri) else {
            return Ok(None);
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        match idx.prepare_rename(&path, byte) {
            Some((_name, span)) => Ok(Some(PrepareRenameResponse::Range(
                workspace::byte_span_to_range(text, span),
            ))),
            None => Ok(None),
        }
    }

    async fn rename(
        &self,
        params: RenameParams,
    ) -> tower_lsp::jsonrpc::Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        let store = self.documents.lock().await;
        let Some(text) = store.get(&uri).map(|m| m.text.as_str()) else {
            return Ok(None);
        };
        let offset = crate::lsp::line_index::LineIndex::new(text).offset(position);
        let byte = char_to_byte(text, offset);
        let Some(path) = url_to_canon(&uri) else {
            return Ok(None);
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let Some(edits) = idx.rename_edits(&path, byte, &new_name) else {
            return Ok(None); // refused (collision / parse error / not renameable)
        };
        // Group edits by file into a WorkspaceEdit.
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (p, span) in edits {
            let Some(file_uri) = canon_to_url(&p) else {
                continue;
            };
            let file_text = idx_text(&idx, &p).unwrap_or_default();
            changes.entry(file_uri).or_default().push(TextEdit {
                range: workspace::byte_span_to_range(&file_text, span),
                new_text: new_name.clone(),
            });
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

/// The text of an indexed file (from its `FileIndex.text`).
fn idx_text(idx: &WorkspaceIndex, path: &std::path::Path) -> Option<String> {
    idx.files.get(path).map(|f| f.text.clone())
}

/// Char offset → byte offset within `text` (the index keys on bytes; LSP
/// positions convert to char offsets via `LineIndex`).
fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices()
        .nth(char_off)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Map a workspace `DefKind` to an LSP `SymbolKind`.
fn def_symbol_kind(kind: workspace::DefKind) -> SymbolKind {
    use workspace::DefKind::*;
    match kind {
        Fn => SymbolKind::FUNCTION,
        Class => SymbolKind::CLASS,
        Enum => SymbolKind::ENUM,
        Const => SymbolKind::CONSTANT,
        Let => SymbolKind::VARIABLE,
        Import => SymbolKind::MODULE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion (SP4 §4): the LSP backend never holds a non-`Send`
    /// interpreter type, so it stays `Send + Sync` for tower-lsp.
    #[test]
    fn backend_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Backend>();
        assert_send_sync::<WorkspaceIndex>();
    }

    #[test]
    fn capabilities_advertise_cross_file_providers() {
        let caps = server_capabilities();
        assert!(
            matches!(caps.references_provider, Some(OneOf::Left(true)) | Some(OneOf::Right(_))),
            "expected a references provider"
        );
        assert!(
            matches!(
                caps.workspace_symbol_provider,
                Some(OneOf::Left(true)) | Some(OneOf::Right(_))
            ),
            "expected a workspace symbol provider"
        );
        assert!(caps.rename_provider.is_some(), "expected a rename provider");
    }

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
        let completion = caps
            .completion_provider
            .expect("expected a completion provider");
        let triggers = completion
            .trigger_characters
            .expect("expected trigger characters");
        assert!(
            triggers.contains(&".".to_string()),
            "triggers: {triggers:?}"
        );
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
