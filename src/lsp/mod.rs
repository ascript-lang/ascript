//! AScript language server (LSP over stdio), feature-gated behind `lsp`.
//!
//! Run it with `ascript lsp`; the process speaks LSP JSON-RPC over stdin/stdout and
//! exits when the client closes the stream (stdin EOF). Point an editor's LSP client
//! at the `ascript lsp` command for `.as` files to get inline diagnostics, document
//! symbols, completion, hover, and go-to-definition. For example, a Neovim
//! `vim.lsp.start` / a VS Code custom-language-client `serverOptions` need only the
//! command `["ascript", "lsp"]` with the `.as` filetype — no extra args.
//!
//! The server does STATIC analysis only (lex/parse) — it never runs the
//! interpreter — so the whole layer stays `Send + Sync` and free of the runtime's
//! `Rc`/`RefCell` types. See `analysis` (pure) and `server` (the thin adapter).

pub mod analysis;
pub mod convert;
pub mod line_index;
pub mod model;
pub mod server;
pub mod workspace;

use server::Backend;
use tower_lsp::{LspService, Server};

/// Build the LSP service and serve it over stdio until the client disconnects.
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
