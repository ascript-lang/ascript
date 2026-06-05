//! tower-lsp `LanguageServer` implementation: a thin async adapter over the cached
//! `SemanticModel` document store and the pure `providers`.
//!
//! Every capability runs on the CST front-end + cached `SemanticModel` — the legacy
//! `ast`/`lexer`/`parser` front-end is NOT used here. The backend is `Send + Sync`
//! because it holds only `Send + Sync` types: the `Client`, and a
//! `tokio::sync::Mutex<DocumentStore>` of per-document semantic models. No
//! interpreter state (`Rc`/`RefCell`/`Value`) is ever held, so this compiles on the
//! `current_thread` tokio runtime AND satisfies tower-lsp's `Send + Sync` bounds.

use crate::lsp::model::DocumentStore;
use crate::lsp::workspace::{self, WorkspaceIndex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

/// Client-configurable settings (subset). Updated by `didChangeConfiguration`.
#[derive(Debug, Clone, Default)]
pub struct LspSettings {
    /// `ascript.color.detectHexStringsEverywhere` — broaden hex-string color
    /// detection past the color-sink gate. Default `false`.
    pub detect_hex_strings_everywhere: bool,
}

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
    /// Client-configurable settings (`didChangeConfiguration`).
    settings: RwLock<LspSettings>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            documents: Mutex::new(DocumentStore::new()),
            index: RwLock::new(WorkspaceIndex::new()),
            roots: RwLock::new(Vec::new()),
            settings: RwLock::new(LspSettings::default()),
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

    /// Resolve a type-hierarchy step (`supertypes` when `up`, else `subtypes`) for
    /// a `TypeHierarchyItem`: re-anchor it in its document model by name, then map
    /// the provider's `(name, span)` pairs to `TypeHierarchyItem`s in the same file.
    async fn type_hierarchy_step(&self, item: TypeHierarchyItem, up: bool) -> Vec<TypeHierarchyItem> {
        let uri = item.uri.clone();
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Vec::new();
        };
        let offset = range_start_byte(&model.text, item.selection_range);
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_type(model, offset) else {
            return Vec::new();
        };
        let pairs = if up {
            crate::lsp::providers::hierarchy::supertypes(model, &anchor.name)
        } else {
            crate::lsp::providers::hierarchy::subtypes(model, &anchor.name)
        };
        pairs
            .into_iter()
            .map(|(name, span)| {
                let range = crate::lsp::convert::byte_span_to_range(
                    &model.text,
                    &model.line_index,
                    span,
                );
                type_item(name, SymbolKind::CLASS, uri.clone(), range)
            })
            .collect()
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
                    diags.push(crate::lsp::convert::byte_diagnostic_to_lsp(&text, &d));
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
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        document_symbol_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            // `.` for member-access completions; `"` / `'` for import-path strings.
            trigger_characters: Some(vec![".".to_string(), "\"".to_string(), "'".to_string()]),
            resolve_provider: Some(true),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        // Phase 3: declaration ≈ definition for AScript (no separate forward
        // declaration concept).
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        // Phase 3: jump to the inferred type's class/enum decl (in-file).
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        // Phase 3: subclasses of a class / variants of an enum (in-file).
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
        // Phase 3: structural folding (blocks/decls/literals/match + //region).
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        // Phase 3: smart-expand selection via CST ancestry.
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        // Phase 3: clickable import specifiers (relative → target file).
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::SOURCE_FIX_ALL,
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
            ]),
            resolve_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        // Phase 4: codeLens (run-test/run-main + reference counts) resolved lazily.
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(true),
        }),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                crate::lsp::providers::code_action::FIX_ALL_COMMAND.to_string(),
                // Phase 4: codeLens run/runTest commands (acknowledged, not executed
                // by the server — the static-only invariant: the editor extension
                // binds these to a terminal task).
                "ascript.run".to_string(),
                "ascript.runTest".to_string(),
            ],
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        // Phase 3: call hierarchy (prepare/incoming/outgoing) over the workspace
        // index call graph.
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        // Phase 3: type hierarchy (prepare/supertypes/subtypes) for classes/enums.
        // `lsp-types` 0.94 `ServerCapabilities` has NO `type_hierarchy_provider`
        // field, so it is advertised through the `experimental` escape hatch
        // (tower-lsp routes the `textDocument/prepareTypeHierarchy` method
        // regardless of the advertised capability shape).
        experimental: Some(serde_json::json!({ "typeHierarchyProvider": true })),
        // SP4 §4: cross-file navigation providers backed by the workspace index.
        // Phase 3: advertise lazy `workspaceSymbol/resolve` via the options form.
        workspace_symbol_provider: Some(OneOf::Right(WorkspaceSymbolOptions {
            resolve_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        // Phase 2: read/write occurrence highlighting of the symbol under the cursor.
        document_highlight_provider: Some(OneOf::Left(true)),
        // Phase 4: documentColor / colorPresentation swatches (color.rgb/bgRgb,
        // [r,g,b] tui arrays, gated hex/functional color strings).
        color_provider: Some(ColorProviderCapability::Simple(true)),
        // Phase 4: linkedEditingRange — live rename of a local identifier's same-file
        // occurrences (globals refused).
        linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
        // Phase 4: pull diagnostics (textDocument/diagnostic + workspace/diagnostic),
        // returning the same diagnostics as the push path.
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("ascript".to_string()),
            inter_file_dependencies: true,
            workspace_diagnostics: true,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        // Phase 2: signature help while typing a call's arguments. Triggered on `(`
        // (open the call) and `,` (advance the active parameter / retrigger).
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        // Phase 2: inferred-type + parameter-name inlay hints (with lazy resolve).
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                resolve_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        ))),
        // Phase 2: semantic-token highlighting (full document + range), with the
        // provider's legend.
        semantic_tokens_provider: Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                legend: crate::lsp::providers::semantic_tokens::legend(),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            }),
        ),
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
        // Incremental sync: apply the ranged content changes against the cached
        // text, then rebuild the model from the resulting full text.
        let uri = params.text_document.uri;
        let version = Some(params.text_document.version);
        let new_text = {
            let store = self.documents.lock().await;
            let base = store.get(&uri).map(|m| m.text.clone()).unwrap_or_default();
            crate::lsp::model::apply_changes(&base, &params.content_changes)
        };
        self.reindex_uri(&uri, &new_text);
        self.analyze_and_publish(uri, new_text, version).await;
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
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let symbols = crate::lsp::providers::symbols::document_symbols(model);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::hover::hover(model, offset))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        // The completion provider scans raw chars, so it takes a CHAR offset.
        let offset = model.line_index.offset(position);
        let items = crate::lsp::providers::completion::completions(model, offset);
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn completion_resolve(
        &self,
        mut item: CompletionItem,
    ) -> tower_lsp::jsonrpc::Result<CompletionItem> {
        // Resolve uses the docs table only (no document context needed); a
        // synthetic empty model is sufficient because `resolve_completion`
        // ignores the model for builtins/keywords.
        let model = crate::lsp::model::SemanticModel::build(
            String::new(),
            None,
            &crate::check::LintConfig::default(),
        );
        crate::lsp::providers::completion::resolve_completion(&model, &mut item);
        Ok(item)
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::formatting::format_document(model)))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::formatting::format_range(
            model,
            params.range,
        )))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::highlight::document_highlights(model, offset))
    }

    async fn document_color(
        &self,
        params: DocumentColorParams,
    ) -> tower_lsp::jsonrpc::Result<Vec<ColorInformation>> {
        let uri = params.text_document.uri;
        let everywhere = self
            .settings
            .read()
            .map(|s| s.detect_hex_strings_everywhere)
            .unwrap_or(false);
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(Vec::new());
        };
        Ok(crate::lsp::providers::color::document_colors(model, everywhere))
    }

    async fn color_presentation(
        &self,
        params: ColorPresentationParams,
    ) -> tower_lsp::jsonrpc::Result<Vec<ColorPresentation>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(Vec::new());
        };
        let rgba = crate::lsp::providers::color::Rgba::from_lsp(params.color);
        Ok(crate::lsp::providers::color::color_presentations(model, rgba, params.range))
    }

    async fn linked_editing_range(
        &self,
        params: LinkedEditingRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<LinkedEditingRanges>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::rename::linked_editing_ranges(model, offset)
            .map(|ranges| LinkedEditingRanges { ranges, word_pattern: None }))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::signature::signature_help(model, offset))
    }

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::inlay::inlay_hints(model, range)))
    }

    async fn inlay_hint_resolve(
        &self,
        hint: InlayHint,
    ) -> tower_lsp::jsonrpc::Result<InlayHint> {
        Ok(crate::lsp::providers::inlay::resolve(hint))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let tokens = crate::lsp::providers::semantic_tokens::semantic_tokens_full(model);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let tokens = crate::lsp::providers::semantic_tokens::semantic_tokens_range(model, range);
        Ok(Some(SemanticTokensRangeResult::Tokens(tokens)))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let actions = crate::lsp::providers::code_action::code_actions(
            model,
            &uri,
            params.range,
            &params.context,
        );
        Ok(Some(actions))
    }

    async fn code_action_resolve(
        &self,
        action: CodeAction,
    ) -> tower_lsp::jsonrpc::Result<CodeAction> {
        // The target URI rides in `action.data`.
        let uri = action
            .data
            .as_ref()
            .and_then(|d| serde_json::from_value::<Url>(d.clone()).ok());
        let Some(uri) = uri else { return Ok(action) };
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(action);
        };
        Ok(crate::lsp::providers::code_action::resolve_code_action(
            model, action,
        ))
    }

    async fn code_lens(
        &self,
        params: CodeLensParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::lens::code_lenses(
            model,
            uri.as_str(),
        )))
    }

    async fn code_lens_resolve(
        &self,
        mut lens: CodeLens,
    ) -> tower_lsp::jsonrpc::Result<CodeLens> {
        // A refs lens carries `{ kind:"refs", uri, name }`; fill its title.
        let Some(data) = lens.data.clone() else {
            return Ok(lens);
        };
        let (Some(uri_s), Some(name)) = (
            data.get("uri").and_then(|v| v.as_str()).map(str::to_string),
            data.get("name").and_then(|v| v.as_str()).map(str::to_string),
        ) else {
            return Ok(lens);
        };
        let Ok(uri) = Url::parse(&uri_s) else {
            return Ok(lens);
        };
        // Same-file count from the cached model + cross-file from the index.
        let same_file = {
            let store = self.documents.lock().await;
            store
                .get(&uri)
                .map(|m| crate::lsp::providers::lens::resolve_same_file_ref_count(m, &name))
                .unwrap_or(0)
        };
        let mut count = same_file;
        if let Some(path) = url_to_canon(&uri) {
            if let Ok(idx) = self.index.read() {
                // Cross-file references: every use of this name in an importer of
                // the def's file (the same-file count is authoritative; this adds
                // the importers' uses, anchored on the def's name range).
                if let Some(file) = idx.files.get(&path) {
                    if let Some(def) = file
                        .defs
                        .iter()
                        .find(|d| d.name == name)
                        .map(|d| d.name_range)
                    {
                        let cross = idx
                            .references_at(&path, def.start, false)
                            .into_iter()
                            .filter(|(p, _)| *p != path)
                            .count();
                        count += cross;
                    }
                }
            }
        }
        lens.command = Some(Command {
            title: format!("{count} reference(s)"),
            command: String::new(),
            arguments: None,
        });
        Ok(lens)
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> tower_lsp::jsonrpc::Result<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        match store.get(&uri) {
            Some(model) => Ok(crate::lsp::providers::diagnostic::document_report(model)),
            None => Ok(DocumentDiagnosticReportResult::Report(
                DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport::default()),
            )),
        }
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> tower_lsp::jsonrpc::Result<WorkspaceDiagnosticReportResult> {
        // Project-wide: every indexed file → a workspace full report. Build a model
        // per file off its cached text (config-aware via the per-path lint config),
        // returning the SAME diagnostics the push/document-pull paths compute.
        let files: Vec<(PathBuf, String)> = {
            match self.index.read().ok() {
                Some(idx) => idx
                    .files
                    .iter()
                    .map(|(p, f)| (p.clone(), f.text.clone()))
                    .collect(),
                None => Vec::new(),
            }
        };
        let mut items: Vec<WorkspaceDocumentDiagnosticReport> = Vec::with_capacity(files.len());
        for (path, text) in files {
            let Some(uri) = canon_to_url(&path) else {
                continue;
            };
            let config = crate::lsp::model::config_for_path(&path);
            let model = crate::lsp::model::SemanticModel::build(text, None, &config);
            items.push(WorkspaceDocumentDiagnosticReport::Full(
                WorkspaceFullDocumentDiagnosticReport {
                    uri,
                    version: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items: model.lsp_diagnostics(),
                    },
                },
            ));
        }
        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        if params.command == crate::lsp::providers::code_action::FIX_ALL_COMMAND {
            // The first argument is the target document URI.
            if let Some(arg) = params.arguments.first() {
                if let Ok(uri) = serde_json::from_value::<Url>(arg.clone()) {
                    let edit = {
                        let store = self.documents.lock().await;
                        store.get(&uri).and_then(|m| {
                            crate::lsp::providers::code_action::fix_all_action(m, &uri)
                                .and_then(|a| a.edit)
                        })
                    };
                    if let Some(edit) = edit {
                        let _ = self.client.apply_edit(edit).await;
                    }
                }
            }
        } else if params.command == "ascript.run" || params.command == "ascript.runTest" {
            // Static-only invariant: the server NEVER runs the interpreter. It only
            // acknowledges the intent; the editor extension binds these commands to
            // a terminal task that invokes `ascript run`/`ascript test` (Phase 6).
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("execute {}: {:?}", params.command, params.arguments),
                )
                .await;
        }
        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let byte = crate::lsp::providers::docs::byte_offset_at(model, position);
        // SP4 §4: try the cross-file index first — if the use at the cursor is an
        // imported/cross-file name (or a top-level/module global), return a
        // `Location` in the TARGET file.
        if let Some(path) = url_to_canon(&uri) {
            let xfile = self.index.read().ok().and_then(|idx| {
                idx.definition_at(&path, byte).map(|(def_path, span)| {
                    let target_text = idx_text(&idx, &def_path).unwrap_or_default();
                    (def_path, workspace::byte_span_to_range(&target_text, span))
                })
            });
            if let Some((def_path, range)) = xfile {
                if let Some(target_uri) = canon_to_url(&def_path) {
                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: target_uri,
                        range,
                    })));
                }
            }
        }
        // Fall back to the single-file resolver provider (same-file local/param/
        // upvalue bindings the cross-file index does not track).
        let Some(range) = crate::lsp::providers::navigation::definition_in_file(model, byte) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri,
            range,
        })))
    }

    async fn goto_declaration(
        &self,
        params: request::GotoDeclarationParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoDeclarationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        // Cross-file via the workspace index first (mirrors `goto_definition`).
        if let Some(path) = url_to_canon(&uri) {
            let xfile = self.index.read().ok().and_then(|idx| {
                idx.definition_at(&path, offset).map(|(def_path, span)| {
                    let target_text = idx_text(&idx, &def_path).unwrap_or_default();
                    (def_path, workspace::byte_span_to_range(&target_text, span))
                })
            });
            if let Some((def_path, range)) = xfile {
                if let Some(def_uri) = canon_to_url(&def_path) {
                    return Ok(Some(request::GotoDeclarationResponse::Scalar(Location {
                        uri: def_uri,
                        range,
                    })));
                }
            }
        }
        Ok(
            crate::lsp::providers::navigation::declaration_in_file(model, offset)
                .map(|range| request::GotoDeclarationResponse::Scalar(Location { uri, range })),
        )
    }

    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(
            crate::lsp::providers::navigation::type_definition_in_file(model, offset)
                .map(|range| request::GotoTypeDefinitionResponse::Scalar(Location { uri, range })),
        )
    }

    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        let locs: Vec<Location> =
            crate::lsp::providers::navigation::implementations_in_file(model, offset)
                .into_iter()
                .map(|range| Location {
                    uri: uri.clone(),
                    range,
                })
                .collect();
        if locs.is_empty() {
            return Ok(None);
        }
        Ok(Some(request::GotoImplementationResponse::Array(locs)))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(path) = url_to_canon(&uri) else {
            return Ok(None);
        };
        let offset = {
            let store = self.documents.lock().await;
            let Some(model) = store.get(&uri) else {
                return Ok(None);
            };
            crate::lsp::providers::docs::byte_offset_at(model, position)
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_call(&idx, &path, offset)
        else {
            return Ok(None);
        };
        let Some(item) = call_item(&idx, &anchor.path, &anchor.name, anchor.name_range) else {
            return Ok(None);
        };
        Ok(Some(vec![item]))
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let Some(path) = url_to_canon(&item.uri) else {
            return Ok(None);
        };
        let offset = {
            let Ok(idx) = self.index.read() else {
                return Ok(None);
            };
            let text = idx_text(&idx, &path).unwrap_or_default();
            range_start_byte(&text, item.selection_range)
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_call(&idx, &path, offset)
        else {
            return Ok(None);
        };
        // Group the incoming reference sites by the file they occur in.
        let mut by_file: HashMap<PathBuf, Vec<Range>> = HashMap::new();
        for (p, span) in crate::lsp::providers::hierarchy::incoming_calls(&idx, &anchor) {
            let text = idx_text(&idx, &p).unwrap_or_default();
            by_file
                .entry(p)
                .or_default()
                .push(workspace::byte_span_to_range(&text, span));
        }
        let mut out = Vec::new();
        for (p, from_ranges) in by_file {
            // The caller item: the FILE that contains the reference (its own name
            // is the file stem — a coarse but valid "from" item per LSP).
            let Some(from) = file_call_item(&idx, &p) else {
                continue;
            };
            out.push(CallHierarchyIncomingCall { from, from_ranges });
        }
        Ok(Some(out))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        let Some(path) = url_to_canon(&item.uri) else {
            return Ok(None);
        };
        // The anchor file's cached model drives the CST walk of the fn body.
        let model_text = {
            let store = self.documents.lock().await;
            store.get(&item.uri).map(|m| m.text.clone())
        };
        let Ok(idx) = self.index.read() else {
            return Ok(None);
        };
        let text = model_text
            .clone()
            .or_else(|| idx_text(&idx, &path))
            .unwrap_or_default();
        let offset = range_start_byte(&text, item.selection_range);
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_call(&idx, &path, offset)
        else {
            return Ok(None);
        };
        let model = crate::lsp::model::SemanticModel::build(
            text,
            None,
            &crate::check::LintConfig::default(),
        );
        let mut out = Vec::new();
        for call in crate::lsp::providers::hierarchy::outgoing_calls(&idx, &model, &anchor) {
            let Some((def_path, def_span)) = call.def else {
                continue; // unresolved callee — skip
            };
            let Some(to) = call_item(&idx, &def_path, &call.name, def_span) else {
                continue;
            };
            let from_range = workspace::byte_span_to_range(&model.text, call.call_site);
            out.push(CallHierarchyOutgoingCall {
                to,
                from_ranges: vec![from_range],
            });
        }
        Ok(Some(out))
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_type(model, offset) else {
            return Ok(None);
        };
        let range = crate::lsp::convert::byte_span_to_range(
            &model.text,
            &model.line_index,
            anchor.name_range,
        );
        let kind = if anchor.is_class {
            SymbolKind::CLASS
        } else {
            SymbolKind::ENUM
        };
        Ok(Some(vec![type_item(anchor.name, kind, uri, range)]))
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(Some(self.type_hierarchy_step(params.item, true).await))
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(Some(self.type_hierarchy_step(params.item, false).await))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<FoldingRange>>> {
        let store = self.documents.lock().await;
        let Some(model) = store.get(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::folding::folding_ranges(model)))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<SelectionRange>>> {
        let store = self.documents.lock().await;
        let Some(model) = store.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let mut out = Vec::with_capacity(params.positions.len());
        for pos in &params.positions {
            let offset = crate::lsp::providers::docs::byte_offset_at(model, *pos);
            match crate::lsp::providers::folding::selection_range_at(model, offset) {
                Some(sr) => out.push(sr),
                None => out.push(SelectionRange {
                    range: Range::new(*pos, *pos),
                    parent: None,
                }),
            }
        }
        Ok(Some(out))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let dir = uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::folding::document_links(
            model,
            dir.as_deref(),
        )))
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

    async fn symbol_resolve(
        &self,
        params: WorkspaceSymbol,
    ) -> tower_lsp::jsonrpc::Result<WorkspaceSymbol> {
        Ok(crate::lsp::providers::symbols::resolve_workspace_symbol(
            params,
        ))
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

/// Build a `CallHierarchyItem` for a named callable defined at `name_range` in
/// `path` (kind `FUNCTION`; `range`/`selection_range` are the name span).
fn call_item(
    idx: &WorkspaceIndex,
    path: &std::path::Path,
    name: &str,
    name_range: crate::check::ByteSpan,
) -> Option<CallHierarchyItem> {
    let uri = canon_to_url(path)?;
    let text = idx_text(idx, path).unwrap_or_default();
    let range = workspace::byte_span_to_range(&text, name_range);
    #[allow(deprecated)]
    Some(CallHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

/// Build a coarse file-level `CallHierarchyItem` for an INCOMING-call caller: the
/// item is the file itself (kind `FILE`, name = file stem), with the call sites in
/// `from_ranges`. Used when a reference's enclosing function is not separately
/// resolved.
fn file_call_item(idx: &WorkspaceIndex, path: &std::path::Path) -> Option<CallHierarchyItem> {
    let uri = canon_to_url(path)?;
    let _ = idx_text(idx, path); // ensure the file is known to the index
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "<file>".to_string());
    let zero = Range::new(Position::new(0, 0), Position::new(0, 0));
    #[allow(deprecated)]
    Some(CallHierarchyItem {
        name,
        kind: SymbolKind::FILE,
        tags: None,
        detail: None,
        uri,
        range: zero,
        selection_range: zero,
        data: None,
    })
}

/// Build a `TypeHierarchyItem` for a class/enum `name` at `range` in `uri`.
fn type_item(name: String, kind: SymbolKind, uri: Url, range: Range) -> TypeHierarchyItem {
    TypeHierarchyItem {
        name,
        kind,
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: None,
    }
}

/// The byte offset of `range`'s start within `text` (LSP position → char → byte).
fn range_start_byte(text: &str, range: Range) -> usize {
    let char_off = crate::lsp::line_index::LineIndex::new(text).offset(range.start);
    char_to_byte(text, char_off)
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
    fn capabilities_advertise_phase3_navigation() {
        let caps = server_capabilities();
        assert!(caps.declaration_provider.is_some());
        assert!(caps.type_definition_provider.is_some());
        assert!(caps.implementation_provider.is_some());
        assert!(caps.folding_range_provider.is_some());
        assert!(caps.selection_range_provider.is_some());
        assert!(caps.document_link_provider.is_some());
        assert!(caps.call_hierarchy_provider.is_some());
        // `lsp-types` 0.94 has no `type_hierarchy_provider` field — it is advertised
        // through `experimental.typeHierarchyProvider`.
        assert_eq!(
            caps.experimental
                .as_ref()
                .and_then(|e| e.get("typeHierarchyProvider")),
            Some(&serde_json::Value::Bool(true)),
        );
        // workspaceSymbol advertises lazy resolve.
        assert!(matches!(
            caps.workspace_symbol_provider,
            Some(OneOf::Right(WorkspaceSymbolOptions {
                resolve_provider: Some(true),
                ..
            }))
        ));
    }

    #[test]
    fn capabilities_advertise_incremental_sync() {
        let caps = server_capabilities();
        match caps.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(kind)) => {
                assert_eq!(kind, TextDocumentSyncKind::INCREMENTAL);
            }
            other => panic!("expected INCREMENTAL text document sync, got {:?}", other),
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

    #[test]
    fn capabilities_advertise_semantic_tokens() {
        let caps = server_capabilities();
        assert!(
            caps.semantic_tokens_provider.is_some(),
            "expected a semantic-tokens provider"
        );
    }

    #[test]
    fn capabilities_advertise_document_highlight() {
        let caps = server_capabilities();
        assert!(
            matches!(
                caps.document_highlight_provider,
                Some(OneOf::Left(true)) | Some(OneOf::Right(_))
            ),
            "expected a document-highlight provider"
        );
    }

    #[test]
    fn capabilities_advertise_color_provider() {
        let caps = server_capabilities();
        assert!(
            caps.color_provider.is_some(),
            "expected a documentColor provider"
        );
    }

    #[test]
    fn capabilities_advertise_code_lens_and_run_commands() {
        let caps = server_capabilities();
        let lens = caps.code_lens_provider.expect("code-lens provider");
        assert_eq!(lens.resolve_provider, Some(true));
        let exec = caps
            .execute_command_provider
            .expect("execute-command provider");
        assert!(exec.commands.iter().any(|c| c == "ascript.run"));
        assert!(exec.commands.iter().any(|c| c == "ascript.runTest"));
        // The Phase 1 fix-all command is still present (extended, not overwritten).
        assert!(exec
            .commands
            .iter()
            .any(|c| c == crate::lsp::providers::code_action::FIX_ALL_COMMAND));
    }

    #[test]
    fn capabilities_advertise_pull_diagnostics() {
        let caps = server_capabilities();
        match caps.diagnostic_provider {
            Some(DiagnosticServerCapabilities::Options(opts)) => {
                assert!(opts.workspace_diagnostics, "workspace diagnostics on");
                assert!(opts.inter_file_dependencies, "inter-file deps on");
            }
            other => panic!("expected DiagnosticOptions, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_advertise_linked_editing_range() {
        let caps = server_capabilities();
        assert!(
            caps.linked_editing_range_provider.is_some(),
            "expected a linkedEditingRange provider"
        );
    }

    #[test]
    fn capabilities_advertise_signature_help() {
        let caps = server_capabilities();
        let sig = caps
            .signature_help_provider
            .expect("signature-help provider");
        let triggers = sig.trigger_characters.expect("trigger chars");
        assert!(triggers.contains(&"(".to_string()));
        assert!(triggers.contains(&",".to_string()));
    }

    #[test]
    fn capabilities_advertise_inlay_hints() {
        let caps = server_capabilities();
        assert!(
            caps.inlay_hint_provider.is_some(),
            "expected an inlay-hint provider"
        );
    }

    #[test]
    fn capabilities_advertise_formatting() {
        let caps = server_capabilities();
        assert!(
            caps.document_formatting_provider.is_some(),
            "formatting advertised"
        );
        assert!(
            caps.document_range_formatting_provider.is_some(),
            "range formatting advertised"
        );
    }
}
