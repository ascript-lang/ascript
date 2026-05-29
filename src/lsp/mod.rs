//! AScript language server (LSP over stdio), feature-gated behind `lsp`.
//!
//! Run it with `ascript lsp`; the process speaks LSP JSON-RPC over stdin/stdout and
//! exits when the client closes the stream. Point an editor's LSP client at the
//! `ascript lsp` command for `.as` files to get inline diagnostics (and, as later
//! M16 tasks land, document symbols / completion / hover / go-to-definition).
//!
//! The server does STATIC analysis only (lex/parse) — it never runs the
//! interpreter — so the whole layer stays `Send + Sync` and free of the runtime's
//! `Rc`/`RefCell` types. See `analysis` (pure) and `server` (the thin adapter).

pub mod analysis;
pub mod line_index;
pub mod server;

use server::Backend;
use tower_lsp::{LspService, Server};

/// Build the LSP service and serve it over stdio until the client disconnects.
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
