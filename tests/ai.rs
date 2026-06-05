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

// ===========================================================================
// Phase F — embeddings.
// ===========================================================================

#[test]
fn embed_single() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [emb, err] = await ai.embed({ model: "openai:text-embedding-3-small", value: "sunny day" })
        if (err != nil) { print("ERR " + err.message); return }
        print(len(emb.embedding))
        print(emb.embedding[0] > 0.09 && emb.embedding[0] < 0.11)
        print(emb.usage.inputTokens)
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/embed.json"))],
    );
    assert_eq!(out, "3\ntrue\n3\n");
}

#[test]
fn embed_many() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [embs, err] = await ai.embedMany({ model: "openai:text-embedding-3-small", values: ["a", "b"] })
        if (err != nil) { print("ERR " + err.message); return }
        print(len(embs.embeddings))
        print(len(embs.embeddings[0]))
        print(embs.usage.inputTokens)
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/embed_many.json"))],
    );
    assert_eq!(out, "2\n2\n4\n");
}

#[test]
fn embed_error_is_tier1() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [emb, err] = await ai.embed({ model: "openai:text-embedding-3-small", value: "x" })
        if (err != nil) { print("status=" + type(err.status)); return }
        print("unexpected ok")
        "#,
        vec![ai_mock::Fixture::json_status(429, r#"{"error":{"message":"rate limited"}}"#)],
    );
    assert_eq!(out, "status=number\n");
}

// ===========================================================================
// Phase E — tools (the in-interpreter sequential tool-use loop).
// ===========================================================================

#[test]
fn tool_loop_single_step_then_final() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        import * as schema from "std/schema"
        let weather = ai.tool({
            description: "Get current weather for a city",
            input: schema.object({ city: schema.string() }),
            execute: (args) => { return [{ tempC: 18, sky: "clear", city: args.city }, nil] },
        })
        let [out, err] = await ai.generate({
            model: "openai:gpt-4.1",
            prompt: "What should I wear in Lisbon?",
            tools: { weather: weather },
            maxSteps: 5,
        })
        if (err != nil) { print("ERR " + err.message); return }
        print(out.text)
        print(len(out.steps))
        print(out.steps[0].tool)
        print(out.steps[0].arguments.city)
        "#,
        vec![
            ai_mock::Fixture::json(&fixture("openai/tool_turn1.json")),
            ai_mock::Fixture::json(&fixture("openai/tool_turn2.json")),
        ],
    );
    assert_eq!(
        out,
        "Wear a light jacket; it's 18C and clear in Lisbon.\n1\nweather\nLisbon\n"
    );
}

#[test]
fn tool_loop_async_execute() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        import * as schema from "std/schema"
        let weather = ai.tool({
            description: "weather",
            input: schema.object({ city: schema.string() }),
            execute: async (args) => { return [{ tempC: 18 }, nil] },
        })
        let [out, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "x", tools: { weather: weather } })
        if (err != nil) { print("ERR " + err.message); return }
        print(out.text)
        "#,
        vec![
            ai_mock::Fixture::json(&fixture("openai/tool_turn1.json")),
            ai_mock::Fixture::json(&fixture("openai/tool_turn2.json")),
        ],
    );
    assert_eq!(out, "Wear a light jacket; it's 18C and clear in Lisbon.\n");
}

#[test]
fn tool_execute_error_is_fed_back_not_aborted() {
    // The tool returns [nil, err]; the loop feeds it back and still reaches turn 2.
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        import * as schema from "std/schema"
        let weather = ai.tool({
            description: "weather",
            input: schema.object({ city: schema.string() }),
            execute: (args) => { return [nil, { message: "service down" }] },
        })
        let [out, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "x", tools: { weather: weather } })
        if (err != nil) { print("ERR " + err.message); return }
        print(out.text)
        print(out.steps[0].error)
        "#,
        vec![
            ai_mock::Fixture::json(&fixture("openai/tool_turn1.json")),
            ai_mock::Fixture::json(&fixture("openai/tool_turn2.json")),
        ],
    );
    assert_eq!(
        out,
        "Wear a light jacket; it's 18C and clear in Lisbon.\nservice down\n"
    );
}

#[test]
fn tool_malformed_definition_is_tier2_panic() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let res = local.block_on(&rt, async {
        ascript::run_source(
            r#"
            import * as ai from "std/ai"
            ai.tool({ description: "x", execute: (a) => { return [nil, nil] } })
            "#,
        )
        .await
    });
    let err = res.expect_err("missing 'input' must be a Tier-2 panic");
    assert!(err.message.contains("input"), "got: {}", err.message);
}

// ===========================================================================
// Phase C — streaming (generators + for await).
// ===========================================================================

#[test]
fn stream_openai_typed_chunks_and_result() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [s, err] = await ai.stream({ model: "openai:gpt-4.1", prompt: "Explain backpressure." })
        if (err != nil) { print("ERR " + err.message); return }
        let acc = ""
        for await (chunk in s) {
            if (chunk.type == "text") { acc = acc + chunk.text }
            if (chunk.type == "finish") { print("FINISH " + chunk.finishReason) }
        }
        print(acc)
        let final = s.result()
        print(final.text)
        print(final.usage.totalTokens)
        "#,
        vec![ai_mock::Fixture::sse(&fixture("openai/stream.sse"))],
    );
    assert_eq!(out, "FINISH stop\nBackpressure flows.\nBackpressure flows.\n6\n");
}

#[test]
fn stream_openai_text_only() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [s, err] = await ai.stream({ model: "openai:gpt-4.1", prompt: "go" })
        if (err != nil) { print("ERR " + err.message); return }
        for await (piece in s.textOnly()) {
            print(piece)
        }
        "#,
        vec![ai_mock::Fixture::sse(&fixture("openai/stream.sse"))],
    );
    assert_eq!(out, "Back\npressure\n flows.\n");
}

// ===========================================================================
// Phase D — structured output (class + std/schema) + JSON-Schema projector.
// ===========================================================================

#[test]
fn structured_output_class_instance() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        class Recipe { name: string  ingredients: array<string>  steps: array<string> }
        let [recipe, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "lasagna", shape: Recipe })
        if (err != nil) { print("ERR " + err.message); return }
        print(recipe.name)
        print(recipe.ingredients[0])
        print(len(recipe.steps))
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/recipe.json"))],
    );
    assert_eq!(out, "Lasagna\npasta\n2\n");
}

#[test]
fn structured_output_schema_object() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        import * as schema from "std/schema"
        let s = schema.object({ sentiment: schema.oneOf(["pos","neg","neu"]), score: schema.number() })
        let [obj, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "rate", shape: s })
        if (err != nil) { print("ERR " + err.message); return }
        print(obj.sentiment)
        print(obj.score)
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/sentiment.json"))],
    );
    assert_eq!(out, "pos\n0.9\n");
}

#[test]
fn structured_output_shape_violation_is_fused_tier1() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        class Recipe { name: string  ingredients: array<string>  steps: array<string> }
        let [recipe, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "x", shape: Recipe })
        if (err != nil) { print("tier1 err"); return }
        print("unexpected ok")
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/recipe_bad.json"))],
    );
    assert_eq!(out, "tier1 err\n");
}

#[test]
fn structured_output_non_json_is_tier1() {
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        class Recipe { name: string  ingredients: array<string>  steps: array<string> }
        let [recipe, err] = await ai.generate({ model: "openai:gpt-4.1", prompt: "x", shape: Recipe })
        if (err != nil) { print("tier1 err"); return }
        print("unexpected ok")
        "#,
        vec![ai_mock::Fixture::json(&fixture("openai/not_json.json"))],
    );
    assert_eq!(out, "tier1 err\n");
}

#[test]
fn stream_connection_error_is_tier1_on_next() {
    // genai sends the streaming request lazily, so a provider 5xx surfaces on the
    // first `next()` poll as a Tier-1 `[nil, err]` (not on the ai.stream open).
    let out = run_with_fixtures(
        r#"
        import * as ai from "std/ai"
        let [s, err] = await ai.stream({ model: "openai:gpt-4.1", prompt: "boom" })
        if (err != nil) { print("open status=" + type(err.status)); return }
        let [chunk, e2] = await s.next()
        if (e2 != nil) { print("next status=" + type(e2.status)); return }
        print("unexpected ok")
        "#,
        vec![ai_mock::Fixture::json_status(500, r#"{"error":{"message":"server error"}}"#)],
    );
    assert_eq!(out, "next status=number\n");
}

// ===========================================================================
// Phase F — telemetry GenAI spans (opt-in, through the SP12 soft hook).
// Gated on BOTH `ai` and `telemetry`: asserts a `chat <model>` span with the
// gen_ai.* attributes is captured when telemetry is initialized, through the
// runtime hook (no AI-specific exporter, no network).
// ===========================================================================
#[cfg(feature = "telemetry")]
mod telemetry_spans {
    use super::*;

    /// Like `run_with_fixtures` but returns the owning interp so the test can read
    /// `telemetry_spans_debug()`.
    fn run_with_fixtures_i(
        src: &str,
        fixtures: Vec<ai_mock::Fixture>,
    ) -> std::rc::Rc<ascript::interp::Interp> {
        let server = ai_mock::MockServer::start(fixtures);
        let base = server.base_url();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        let interp = local.block_on(&rt, async {
            ascript::stdlib::ai::set_test_endpoint(Some(base));
            let (_out, interp) = ascript::run_source_with_interp(src)
                .await
                .expect("program runs");
            ascript::stdlib::ai::set_test_endpoint(None);
            interp
        });
        server.stop();
        interp
    }

    const INIT: &str = r#"
import * as telemetry from "std/telemetry"
import * as ai from "std/ai"
telemetry.init({ service: "t", exporters: [ telemetry.otlp({ endpoint: "http://localhost:4318" }) ] })
"#;

    #[test]
    fn generate_emits_genai_span() {
        let interp = run_with_fixtures_i(
            &format!(
                r#"{INIT}
let [r, err] = await ai.generate({{ model: "openai:gpt-4.1", prompt: "hi", temperature: 0.2, maxTokens: 100 }})
print(r.text)
"#
            ),
            vec![ai_mock::Fixture::json(&fixture("openai/chat.json"))],
        );
        let spans = interp.telemetry_spans_debug();
        let chat = spans
            .iter()
            .find(|s| s.name.starts_with("chat "))
            .expect("a `chat <model>` span");
        assert_eq!(chat.name, "chat openai:gpt-4.1");
        let has = |k: &str| chat.attributes.iter().any(|(ak, _)| ak == k);
        assert!(has("gen_ai.operation.name"), "{:?}", chat.attributes);
        assert!(has("gen_ai.provider.name"));
        assert!(has("gen_ai.request.model"));
        assert!(has("gen_ai.request.temperature"));
        assert!(has("gen_ai.request.max_tokens"));
        assert!(has("gen_ai.response.finish_reasons"));
        assert!(has("gen_ai.usage.input_tokens"));
        assert!(has("gen_ai.usage.output_tokens"));
        // PII-safe default: no prompt/response CONTENT recorded.
        assert!(!has("gen_ai.prompt"), "content must be off by default");
        assert_eq!(chat.status_code, 1, "ok");
    }

    #[test]
    fn per_call_disabled_emits_no_span() {
        let interp = run_with_fixtures_i(
            &format!(
                r#"{INIT}
let [r, err] = await ai.generate({{ model: "openai:gpt-4.1", prompt: "hi", telemetry: {{ enabled: false }} }})
print(r.text)
"#
            ),
            vec![ai_mock::Fixture::json(&fixture("openai/chat.json"))],
        );
        let spans = interp.telemetry_spans_debug();
        assert!(
            !spans.iter().any(|s| s.name.starts_with("chat ")),
            "per-call telemetry:{{enabled:false}} suppresses the span"
        );
    }

    #[test]
    fn record_inputs_opt_in_records_prompt() {
        let interp = run_with_fixtures_i(
            &format!(
                r#"{INIT}
let [r, err] = await ai.generate({{ model: "openai:gpt-4.1", prompt: "secret", telemetry: {{ recordInputs: true }} }})
print(r.text)
"#
            ),
            vec![ai_mock::Fixture::json(&fixture("openai/chat.json"))],
        );
        let spans = interp.telemetry_spans_debug();
        let chat = spans.iter().find(|s| s.name.starts_with("chat ")).unwrap();
        assert!(
            chat.attributes.iter().any(|(k, _)| k == "gen_ai.prompt"),
            "recordInputs:true records the prompt"
        );
    }
}

// ===========================================================================
// Env-gated LIVE suite — hits real providers. Skipped (early return) unless
// `ASCRIPT_AI_LIVE=1` AND the provider key is set, so the default `cargo test`
// never opens a socket or needs a secret. NOT `#[ignore]` (it always "passes" by
// early-returning when the env is absent).
// ===========================================================================

fn live_enabled() -> bool {
    std::env::var("ASCRIPT_AI_LIVE").as_deref() == Ok("1")
}

/// Run a live `.as` program (no fixture seam) on a current-thread runtime.
fn run_live(src: &str) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local
        .block_on(&rt, async { ascript::run_source(src).await })
        .map_err(|e| e.message)
}

#[test]
fn live_openai_generate() {
    if !live_enabled() || std::env::var("OPENAI_API_KEY").is_err() {
        return; // skipped without ASCRIPT_AI_LIVE=1 + OPENAI_API_KEY
    }
    let out = run_live(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "openai:gpt-4.1-mini", prompt: "Reply with the single word: pong", maxTokens: 10 })
        if (err != nil) { print("ERR " + err.message) } else { print(len(r.text) > 0) }
        "#,
    )
    .expect("live openai run");
    assert!(out.contains("true") || out.contains("ERR"), "got: {}", out);
}

#[test]
fn live_anthropic_generate() {
    if !live_enabled() || std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }
    let out = run_live(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "anthropic:claude-haiku-4.5", prompt: "Reply: pong", maxTokens: 10 })
        if (err != nil) { print("ERR " + err.message) } else { print(len(r.text) > 0) }
        "#,
    )
    .expect("live anthropic run");
    assert!(out.contains("true") || out.contains("ERR"), "got: {}", out);
}

#[test]
fn live_gemini_generate() {
    if !live_enabled() || std::env::var("GEMINI_API_KEY").is_err() {
        return;
    }
    let out = run_live(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "google:gemini-2.5-flash", prompt: "Reply: pong" })
        if (err != nil) { print("ERR " + err.message) } else { print(len(r.text) > 0) }
        "#,
    )
    .expect("live gemini run");
    assert!(out.contains("true") || out.contains("ERR"), "got: {}", out);
}

#[test]
fn live_bedrock_generate() {
    if !live_enabled() || std::env::var("AWS_ACCESS_KEY_ID").is_err() {
        return;
    }
    let out = run_live(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "bedrock:anthropic.claude-3-5-haiku-20241022-v1:0", prompt: "Reply: pong", maxTokens: 10 })
        if (err != nil) { print("ERR " + err.message) } else { print(len(r.text) > 0) }
        "#,
    )
    .expect("live bedrock run");
    assert!(out.contains("true") || out.contains("ERR"), "got: {}", out);
}

#[test]
fn live_vertex_generate() {
    if !live_enabled() || std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_err() {
        return;
    }
    let out = run_live(
        r#"
        import * as ai from "std/ai"
        let [r, err] = await ai.generate({ model: "vertex:gemini-2.5-flash", prompt: "Reply: pong" })
        if (err != nil) { print("ERR " + err.message) } else { print(len(r.text) > 0) }
        "#,
    )
    .expect("live vertex run");
    assert!(out.contains("true") || out.contains("ERR"), "got: {}", out);
}
