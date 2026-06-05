//! SP11 `std/ai` integration tests — fixture-replay (no network, no secrets).
//!
//! The whole suite runs WITHOUT any API key or outbound socket: a local in-process
//! mock HTTP server (`mock`) serves recorded JSON/SSE bodies from
//! `tests/fixtures/ai/<provider>/<case>.{json,sse}`, and a genai
//! `ServiceTargetResolver` rewrites every request's endpoint to that mock + injects
//! a dummy auth key. `cargo test --features ai` exercises everything; default
//! `cargo test` skips this file (it is `#![cfg(feature = "ai")]`).
#![cfg(feature = "ai")]

use std::rc::Rc;

mod ai_mock;

// ===========================================================================
// Phase A — the !Send de-risk spike (BLOCKING).
//
// Proves genai's `Client::exec_chat` future runs on the current-thread
// `LocalSet` via `tokio::task::spawn_local`, with an `Rc<()>` (a `!Send` value)
// held live in scope across the genai await — i.e. genai coexists with the
// AScript `!Send` runtime and does NOT require `Send`/a multi-thread runtime.
// ===========================================================================

#[test]
fn spike_genai_runs_on_current_thread_localset_with_rc_in_scope() {
    use genai::Client;
    use genai::chat::ChatRequest;
    use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
    use genai::ServiceTarget;

    // A local mock server returning a fixed OpenAI-shaped chat-completion body.
    let server = ai_mock::MockServer::start_blocking(ai_mock::Fixture::json(
        r#"{"id":"cmpl-spike","object":"chat.completion","model":"gpt-4.1",
            "choices":[{"index":0,"finish_reason":"stop",
              "message":{"role":"assistant","content":"pong"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
    ));
    let base = server.base_url();

    // current-thread runtime + LocalSet == the exact AScript runtime shape.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let local = tokio::task::LocalSet::new();

    let got: String = local.block_on(&rt, async move {
        // A !Send value (Rc) held live across the spawn_local + the genai await,
        // proving genai coexists with the !Send interior-mutability runtime.
        let marker = Rc::new(());
        let base_for_resolver = base.clone();

        // Rewrite the endpoint to the local mock + inject a dummy key (no real
        // credential needed). The resolver closure is Send+Sync (plain String).
        let resolver = ServiceTargetResolver::from_resolver_fn(
            move |mut tgt: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                tgt.endpoint = Endpoint::from_owned(base_for_resolver.clone());
                tgt.auth = AuthData::from_single("test-key");
                Ok(tgt)
            },
        );
        let client = Client::builder()
            .with_service_target_resolver(resolver)
            .build();

        let req = ChatRequest::new(vec![genai::chat::ChatMessage::user("ping")]);

        // Drive the genai future on a spawn_local task (NOT the multi-thread
        // pool) and await its JoinHandle. If genai required Send this would not
        // compile; if it required a multi-thread runtime it would panic here.
        let handle = tokio::task::spawn_local(async move {
            let resp = client
                .exec_chat("gpt-4.1", req, None)
                .await
                .expect("exec_chat on current-thread LocalSet");
            resp.first_text().unwrap_or_default().to_string()
        });
        let out = handle.await.expect("spawn_local task joined");
        // Touch the Rc after the await so it is genuinely held across it.
        let _ = Rc::strong_count(&marker);
        out
    });

    assert_eq!(got, "pong", "genai ran on the current-thread LocalSet");
    server.stop();
}
