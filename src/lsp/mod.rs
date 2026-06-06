//! AScript language server (LSP over stdio), feature-gated behind `lsp`.
//!
//! Run it with `ascript lsp`; the process speaks LSP JSON-RPC over stdin/stdout and
//! exits when the client closes the stream (stdin EOF). Point an editor's LSP client
//! at the `ascript lsp` command for `.as` files to get inline diagnostics, document
//! symbols, completion, hover, and go-to-definition. For example, a Neovim
//! `vim.lsp.start` / a VS Code custom-language-client `serverOptions` need only the
//! command `["ascript", "lsp"]` with the `.as` filetype — no extra args.
//!
//! The server does STATIC analysis only — it runs the CST front-end
//! (`syntax::parse`/`tree_builder`/`resolve`) plus the checker, NEVER the
//! interpreter — so the whole layer stays `Send + Sync` and free of the runtime's
//! `Rc`/`RefCell` types and the legacy `ast`/`lexer`/`parser` front-end. Each
//! capability is a pure provider over a cached `SemanticModel` (see `model` and
//! `providers`); `server` is the thin async adapter and `convert` owns all
//! byte↔`Position`/`Range` coordinate conversion.

pub mod convert;
pub mod line_index;
pub mod model;
pub mod perf;
pub mod providers;
pub mod server;
pub mod workspace;

use server::Backend;
use tower_lsp::{LspService, Server};

/// Build the LSP service and serve it over stdio until the client disconnects.
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    // `window/workDoneProgress/cancel` is registered as a custom method: tower-lsp
    // 0.20 does not yet expose it as a `LanguageServer` trait method (a documented
    // crate TODO), so we route it to `Backend::on_work_done_progress_cancel`, which
    // flips the cancel flag the initial-indexing loop honors between files.
    let (service, socket) = LspService::build(Backend::new)
        .custom_method(
            "window/workDoneProgress/cancel",
            Backend::on_work_done_progress_cancel,
        )
        .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}
