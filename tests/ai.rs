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

// ===========================================================================
// Shared fixture-replay harness for the `.as` integration tests.
// ===========================================================================

/// Read a recorded fixture body from `tests/fixtures/ai/<rel>`.
fn fixture(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ai")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {}: {}", rel, e))
}

/// Run an `.as` program with the genai client pointed at a mock server replaying
/// `fixtures` (in order). Returns the program's captured stdout. NO network, NO key.
fn run_with_fixtures(src: &str, fixtures: Vec<ai_mock::Fixture>) -> String {
    let server = ai_mock::MockServer::start(fixtures);
    let base = server.base_url();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let local = tokio::task::LocalSet::new();
    let out = local.block_on(&rt, async {
        // The thread-local seam is read by the genai ServiceTargetResolver, on this
        // same thread that run_source executes on.
        ascript::stdlib::ai::set_test_endpoint(Some(base));
        let r = ascript::run_source(src).await;
        ascript::stdlib::ai::set_test_endpoint(None);
        r
    });
    server.stop();
    out.unwrap_or_else(|e| panic!("run_source error: {}", e.message))
}

// ===========================================================================
// Phase B — providers/credentials + ai.generate non-streaming text.
// ===========================================================================

#[test]
fn generate_openai_text() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "Explain ownership." })
        if (err != nil) { print("ERR " + err.message); return }
        print(r.text)
        print(r.finishReason)
        print(r.usage.inputTokens)
        print(r.usage.outputTokens)
        print(r.usage.totalTokens)
        print(len(r.toolCalls))
        print(len(r.steps))
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/chat.json"))],
    );
    assert_eq!(
        out,
        "Ownership is Rust's memory model.\nstop\n12\n7\n19\n0\n0\n"
    );
}

#[test]
fn generate_anthropic_text() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({
            model: "anthropic:claude-sonnet-4.5",
            system: "Be terse.",
            prompt: "CAP theorem?",
            maxTokens: 64,
        })
        if (err != nil) { print("ERR " + err.message); return }
        print(r.text)
        print(r.usage.inputTokens)
        print(r.usage.outputTokens)
        "#,
        vec![ai_mock::Fixture::json(&fixture("anthropic/messages.json"))],
    );
    assert_eq!(
        out,
        "CAP: consistency, availability, partition tolerance — pick two.\n14\n16\n"
    );
}

#[test]
fn generate_google_text() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "google:gemini-2.5-flash", prompt: "Hi" })
        if (err != nil) { print("ERR " + err.message); return }
        print(r.text)
        print(r.usage.totalTokens)
        "#,
        vec![ai_mock::Fixture::json(&fixture("google/generate.json"))],
    );
    assert_eq!(out, "Gemini says hello.\n12\n");
}

#[test]
fn generate_openai_compatible_provider_handle() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let local = ai.provider("openai-compatible", { baseUrl: "http://localhost:11434/v1", apiKey: "ollama" })
        let [r, err] = await ai.generate({ model: local.model("llama3.1"), prompt: "hi" })
        if (err != nil) { print("ERR " + err.message); return }
        print(r.text)
        "#,
        vec![ai_mock::Fixture::json(&fixture("ollama/chat.json"))],
    );
    assert_eq!(out, "Hi from a local model.\n");
}

#[test]
fn generate_provider_http_4xx_is_tier1_with_status() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "boom" })
        if (err != nil) { print("status=" + type(err.status)); print(err.status); return }
        print("unexpected ok")
        "#,
        vec![ai_mock::Fixture::json_status(400, &fixture("openai/error_400.json"))],
    );
    assert_eq!(out, "status=number\n400\n");
}

#[test]
fn generate_prompt_and_messages_both_set_is_tier2_panic() {
    // Both set → Tier-2 panic → run_source returns Err. No fixture is consumed.
    let server = ai_mock::MockServer::start(vec![ai_mock::Fixture::json("{}")]);
    let base = server.base_url();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let res = local.block_on(&rt, async {
        ascript::stdlib::ai::set_test_endpoint(Some(base));
        let r = ascript::run_source(
            r#"
            import * as ai from "std/ai"
            await ai.generate({ model: "openai:gpt-4.1", prompt: "a", messages: [] })
            "#,
        )
        .await;
        ascript::stdlib::ai::set_test_endpoint(None);
        r
    });
    server.stop();
    let err = res.expect_err("both prompt+messages must be a Tier-2 panic");
    assert!(
        err.message.contains("mutually exclusive"),
        "got: {}",
        err.message
    );
}

#[test]
fn generate_unknown_provider_is_tier2_panic() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let res = local.block_on(&rt, async {
        ascript::run_source(
            r#"
            import * as ai from "std/ai"
            await ai.generate({ model: "noprov:x", prompt: "a" })
            "#,
        )
        .await
    });
    let err = res.expect_err("unknown provider must be a Tier-2 panic");
    assert!(err.message.contains("unknown provider"), "got: {}", err.message);
}

#[test]
fn generate_missing_credential_is_tier1() {
    // No fixture seam (real env path) + no key in env → clean Tier-1 error.
    // Ensure the key is absent for this test.
    std::env::remove_var("OPENAI_API_KEY");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let out = local.block_on(&rt, async {
        // No set_test_endpoint → credential pre-check runs against the real env.
        ascript::run_source(
            r#"
            import * as ai from "std/ai"
            let [r, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "hi" })
            if (err != nil) { print(err.message); return }
            print("unexpected ok")
            "#,
        )
        .await
    });
    let out = out.expect("missing credential is Tier-1, not a panic");
    assert_eq!(out, "no credential for provider 'openai'\n");
}
