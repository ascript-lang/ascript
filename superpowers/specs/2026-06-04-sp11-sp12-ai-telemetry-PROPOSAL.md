# `std/ai` + `std/telemetry` — Design Proposal

> **PROPOSAL FOR DISCUSSION (not a final spec).**
> This document researches the AI-SDK and observability landscapes and proposes an
> AScript-idiomatic API shape for two new stdlib modules. It is intended to drive a
> design discussion — it surfaces the real forks and leaves them open rather than
> over-deciding. No code, no final grammar, no commitments.
>
> Author: research/design pass. Date: 2026-06-04. Branch context: `feat/sp1-engine-parity`.

---

## 0. TL;DR — top recommendations

1. **Build native over the existing reqwest-based `std/http`, not a wrapped Rust crate.**
   Per-provider request/response *adapters* (thin Rust functions translating an internal
   neutral request to each wire format and back). This matches the repo's stated bias
   (control, no heavy/unstable deps), keeps the `!Send` single-threaded async model intact,
   and reuses the SSE machinery already in `net_http.rs` verbatim for streaming. Rust LLM
   crates (`genai`, `rig-core`, `async-openai`) are good *reference designs* but pull in
   their own runtime/abstraction assumptions and a large dep tree we don't want in core.

2. **v1 provider tiers.** **Tier A (v1 core):** an *OpenAI-compatible* adapter (one adapter
   covers OpenAI, Azure OpenAI w/ key auth, Ollama, LM Studio, OpenRouter, LiteLLM, groq,
   together, xAI/Grok, DeepSeek — anything speaking `/v1/chat/completions`), plus a native
   **Anthropic Messages** adapter and a native **Google Gemini** adapter. **Tier B (later):**
   AWS Bedrock (SigV4) and GCP Vertex (ADC/service-account) — deferred because their auth is
   heavyweight and pulls real crypto/credential-chain deps.

3. **AScript-idiomatic surface.** Model selection via a `"provider:model"` string + a
   credential resolver (env-var by default, explicit config object to override). Non-streaming
   calls return Tier-1 `[value, err]` pairs; streaming uses **generators + `for await`** (the
   exact idiom `std/stream` and SSE already establish). Structured output reuses **classes +
   `validate_into`** (`ai.generate({..., shape: Recipe})` → `[Recipe-instance, err]`) and/or
   `std/schema`. Tools are AScript functions described by a `std/schema` input schema; the
   tool-use loop is driven inside the interpreter.

4. **`std/telemetry` is a thin, vendor-neutral tracing/metrics/event facade** with pluggable
   exporters (OTLP, Sentry, PostHog). **`std/ai` emits *through* `std/telemetry`** when it is
   present (soft, runtime-optional dependency) following the OpenTelemetry **GenAI semantic
   conventions** (`gen_ai.*` span attributes, token usage) — so Langfuse / any OTel backend
   "just works" — but `std/ai` must also be fully usable with `std/telemetry` absent.

5. **Testing without API keys:** a record/replay HTTP fixture layer. The adapters must take an
   injectable HTTP "send" seam so unit tests run against recorded JSON/SSE fixtures (no
   network, no secrets). A small `--features net` integration suite hits a local mock server
   (mirrors how `tests/` already spawns the binary). Real-provider tests are opt-in, gated on
   env keys, and never run in CI by default.

---

## 1. AScript idioms these modules must honor (grounding)

Read from the repo so the proposal maps cleanly onto what exists:

- **Async**: an `async fn` returns a `future<T>`; `await` drives it. Networking is
  inline/single-threaded (`!Send`, current-thread tokio + `LocalSet`). Almost every net op is
  `await`ed. (`docs/content/stdlib/net.md`, `CLAUDE.md` M17 notes.)
- **Result `[value, err]` (Tier-1) vs panic (Tier-2)**: fallible I/O returns a two-element pair,
  `err` is `nil` on success; *misuse* (wrong arg types, malformed options) is a Tier-2 panic.
  `?` propagates, `!` force-unwraps. This is exactly how `net_http.rs` is shaped.
- **Streaming**: the established idiom is a native handle whose `await h.next() → [item, err]`
  drives a pull loop, consumable with `for await` (see `SseState`/`std/stream`). New streaming
  APIs should produce the same shape.
- **Structured/typed parse**: `ClassName.from(obj, strict?)` and `resp.json(Class)` run
  `validate_into` (recurses into nested class / `array<Class>` / `map<K,Class>`, applies
  defaults, Object→Map coercion, fuses parse+shape failure into ONE Tier-1 pair). `std/schema`
  is the composable alternative (tagged objects, `schema.parse(s, v) → [value, err]`, fluent
  chaining). **Both already exist** — `std/ai` should accept either.
- **Module shape**: native Rust over `Value`, `exports()` + `call(module, func, args, span)`,
  registered in both arms of `src/stdlib/mod.rs`, `#[cfg(feature=...)]`-gated, declared in
  `Cargo.toml [features]`. Stateful native resources live in `Interp.resources` behind a
  `Value::Native` handle id (never embedded in `Value`); never hold a `resources`/`RefCell`
  borrow across `.await` (take-out → await → return). HTTP client is the pooled
  `reqwest::Client` in `net_http.rs`.
- **Logging precedent**: `std/log` is `Interp`-stateful (`log_level`/`log_format`, routed via
  `self.call_log`, emits to stderr Live / capture buffer in tests). `std/telemetry` should
  follow the same stateful-singleton + capture-in-tests pattern.

---

## 2. `std/ai` — proposed API (AScript syntax)

```ascript
import * as ai from "std/ai"
```

### 2.1 Model selection & credentials

A model is named with a `"provider:model"` string (the AI-SDK registry convention). Credentials
resolve from environment variables by default (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`GEMINI_API_KEY`, …), with an explicit config object to override per call or to configure a
custom/compatible endpoint.

```ascript
// Simplest form — env-var credential, default base URL:
let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  prompt: "Write a haiku about Rust ownership.",
})
if (err != nil) { print("ai failed: " + err.message) } else { print(out.text) }

// Switch provider with one string change:
let [out2, _] = await ai.generate({ model: "anthropic:claude-sonnet-4.5", prompt: "..." })

// Explicit credentials / OpenAI-compatible endpoint (Ollama, LM Studio, OpenRouter, LiteLLM):
let local = ai.provider("openai-compatible", {
  baseUrl: "http://localhost:11434/v1",
  apiKey:  "ollama",                 // many local servers ignore the key
})
let [out3, _] = await ai.generate({ model: local.model("llama3.1"), prompt: "..." })
```

`ai.provider(kind, config)` returns a small provider handle; `provider.model(id)` returns a
model handle. The `model:` field of any call accepts **either** a `"provider:model"` string
(env-resolved) **or** a model handle (explicit config). This is the AScript spelling of the
AI-SDK's `createProvider` / `registry.languageModel("openai:...")` split.

> **Credential precedence** (proposed): explicit `config.apiKey` › `provider(...)` handle ›
> env var › Tier-1 error `[nil, {message:"no credential for provider 'openai'"}]` at call time
> (NOT a panic — a missing key is an expected operational failure).

### 2.2 Text generation (non-streaming)

```ascript
let [out, err] = await ai.generate({
  model: "anthropic:claude-sonnet-4.5",
  system: "You are a terse assistant.",
  prompt: "Summarize the CAP theorem in one sentence.",
  maxTokens: 256,
  temperature: 0.2,
})
// out == {
//   text: "...",
//   finishReason: "stop",                 // "stop" | "length" | "tool_calls" | "content_filter"
//   usage: { inputTokens: 41, outputTokens: 32, totalTokens: 73 },
//   toolCalls: [],                        // see §2.5
//   raw: { ... }                          // provider-native response, escape hatch
// }
```

`prompt` (a string) and `messages` (an array, §2.6) are mutually exclusive; supplying both is a
Tier-2 misuse panic. `system` is hoisted to the provider's system slot (top-level for
Anthropic/Gemini, a `role:"system"` message for OpenAI-compatible) by the adapter.

### 2.3 Streaming (generators + `for await`)

Streaming returns a Tier-1 `[stream, err]`; the stream is the established native-handle shape
(`await stream.next() → [chunk, err]`, consumable by `for await`):

```ascript
let [stream, err] = await ai.stream({
  model: "openai:gpt-4.1",
  prompt: "Explain backpressure.",
})
if (err != nil) { print(err.message); return }

for await (let chunk in stream) {
  // chunk == { type: "text", text: "..." }   (text delta)
  if (chunk.type == "text") { print(chunk.text, { end: "" }) }
}

// After the loop, terminal info is available:
let final = stream.result()    // { text, finishReason, usage, toolCalls }
```

`chunk.type` mirrors the AI-SDK `fullStream` event kinds, projected to AScript:
`"text"` (delta), `"toolCall"`, `"toolResult"`, `"finish"` (carries usage). A text-only consumer
can ignore the non-text chunks. Under the hood this is the SSE reader from `net_http.rs` plus a
per-provider line→chunk decoder (OpenAI `data: {...}` deltas, Anthropic `event:`-typed SSE).

> **Open fork** (see §5): do we expose only the rich `fullStream`-style chunk objects, or also a
> convenience `ai.streamText(...)` that yields *bare text strings* for the 90% case? Leaning
> toward: one `ai.stream` with typed chunks + a `stream.textOnly()` adapter generator.

### 2.4 Structured / typed output (classes + `validate_into`, or `std/schema`)

Reuse the existing typed-parse machinery. Pass a `shape:` (a class **or** a `std/schema`) and the
result's `value` is a validated instance / validated object — failure (decode OR shape mismatch)
fuses into ONE Tier-1 `[nil, err]`, exactly like `resp.json(Class)`:

```ascript
class Recipe {
  name: string
  ingredients: array<string>
  steps: array<string>
}

let [recipe, err] = await ai.generate({
  model: "openai:gpt-4.1",
  prompt: "Give me a lasagna recipe.",
  shape: Recipe,                 // class → validate_into; the adapter requests JSON mode
})
if (err != nil) { print("bad output: " + err.message); return }
print(recipe.name)               // recipe is a Recipe instance, fields type-checked

// std/schema alternative (composable, no class needed):
import * as schema from "std/schema"
let s = schema.object({ sentiment: schema.oneOf(["pos", "neg", "neu"]), score: schema.number() })
let [obj, e] = await ai.generate({ model: "anthropic:...", prompt: "...", shape: s })
```

The adapter turns `shape:` into the provider's structured-output mechanism (OpenAI
`response_format: json_schema`, Anthropic tool-forcing / `output` JSON, Gemini
`responseSchema`). Streaming structured output (`ai.stream({shape})` → partial-object stream) is
a **deferred follow-up** (the AI-SDK `partialOutputStream`) — propose v1 ships structured output
non-streaming only.

> The class→JSON-Schema projection is new work: `validate_into` checks an *already-parsed* value,
> but the provider needs a JSON Schema up front. Proposed: derive a minimal JSON Schema from the
> class `FieldSchema` / from a `std/schema` tagged object. This is the one genuinely new
> mechanism `std/ai` needs (everything else reuses existing code). See §5.

### 2.5 Tool calling

A tool is an AScript object: a `std/schema` (or class) input schema + a `description` + an
`execute` fn (which may be `async fn`). The tool-use loop runs inside the interpreter: the model
emits tool calls, `std/ai` validates args against the schema, calls `execute`, feeds results back,
and loops up to `maxSteps`.

```ascript
import * as schema from "std/schema"

let weather = ai.tool({
  description: "Get current weather for a city",
  input: schema.object({ city: schema.string() }),
  execute: async fn(args) {
    let [resp, err] = await getWeather(args.city)     // your code
    if (err != nil) { return [nil, err] }             // tool errors are Tier-1
    return [{ tempC: resp.temp, sky: resp.sky }, nil]
  },
})

let [out, err] = await ai.generate({
  model: "anthropic:claude-sonnet-4.5",
  prompt: "What should I wear in Lisbon today?",
  tools: { weather },
  maxSteps: 5,                    // cap the tool-use loop (AI-SDK stopWhen analog)
})
// out.text is the final natural-language answer; out.steps describes each turn.
```

A tool whose `execute` returns `[nil, err]` surfaces the error back to the model as a tool-result
(so the model can recover) rather than aborting — matching AScript's Tier-1 philosophy. A
malformed tool *definition* (no `input`, `execute` not callable) is a Tier-2 panic.

### 2.6 Message / content model

For multi-turn / multimodal, pass `messages:` instead of `prompt:`. A message is
`{ role, content }`; `content` is a string (text shorthand) or an array of typed parts:

```ascript
let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  messages: [
    { role: "system", content: "You are a vision assistant." },
    { role: "user", content: [
        { type: "text",  text: "What is in this image?" },
        { type: "image", data: imageBytes, mediaType: "image/png" },   // bytes or a URL
    ]},
  ],
})
```

Part types (projected from the AI-SDK content-part model): `"text"`, `"image"`, `"file"`,
`"toolCall"`, `"toolResult"`. Roles: `"system"`, `"user"`, `"assistant"`, `"tool"`. Each adapter
maps these to/from its wire format (e.g. Anthropic image blocks vs OpenAI `image_url`).

### 2.7 Embeddings

```ascript
let [emb, err] = await ai.embed({
  model: "openai:text-embedding-3-small",
  value: "sunny day at the beach",
})
// emb == { embedding: [0.01, -0.2, ...], usage: { inputTokens: 6 } }

let [embs, err2] = await ai.embedMany({
  model: "openai:text-embedding-3-small",
  values: ["a", "b", "c"],
})
// embs == { embeddings: [[...],[...],[...]], usage: {...} }
```

### 2.8 Error handling summary (Tier-1 vs Tier-2)

| Situation | Result |
|---|---|
| Network/TLS/timeout, HTTP 4xx/5xx from provider, missing credential, content-filter refusal, structured-output validation failure | **Tier-1** `[nil, err]` (`err.message`, `err.status?`, `err.code?`) |
| Wrong argument *types*, `prompt`+`messages` both set, malformed tool definition, unknown provider kind | **Tier-2** panic (programmer error) |
| Tool `execute` returns `[nil, err]` | Surfaced back to the model as a tool-result (recoverable), not an abort |

This mirrors `net_http.rs` exactly (a non-2xx is a normal value there, but for an AI call a
provider error is more useful as a Tier-1 `err` — propose `errorOnStatus`-style default ON, with
`raw` always present as an escape hatch).

---

## 3. Provider coverage strategy & adapter architecture

### 3.1 Tiers

- **Tier A — v1 core (3 adapters, covers ~20 providers):**
  - **`openai-compatible`** — the `/v1/chat/completions` + `/v1/embeddings` shape. One adapter
    serves OpenAI, **Azure OpenAI** (key auth + `api-version` query param + deployment-as-model),
    **Ollama**, **LM Studio**, **OpenRouter**, **LiteLLM**, groq, together, **xAI/Grok**,
    **DeepSeek**. Differences are just `baseUrl` + header/credential + a couple of quirks
    (Azure's `api-version`), handled by config, not new code.
  - **`anthropic`** — native Messages API (`/v1/messages`): `x-api-key` + `anthropic-version`
    headers, **required `max_tokens`**, top-level `system`, content-block messages, `event:`-typed
    SSE for streaming, native tool-use blocks.
  - **`google`** — native Gemini `generateContent` / `streamGenerateContent`: `?key=` auth,
    `contents`/`parts` shape, `responseSchema` for structured output.

- **Tier B — deferred (heavier auth, own follow-up):**
  - **AWS Bedrock** (incl. Claude-on-Bedrock) — **SigV4** request signing (needs an HMAC/SHA256
    signing path + credential chain: env, profile, IMDS). Real new crypto + credential code.
  - **GCP Vertex AI** — **ADC / service-account** OAuth2 (JWT-sign a service-account key → token
    exchange, or metadata-server token). Real new auth code.
  - Rationale: both are "same body, hard auth." Defer behind their own feature flags
    (`ai-bedrock`, `ai-vertex`) so v1 stays lean. Note: Claude-on-Bedrock and Claude-on-Vertex
    can reuse the **Anthropic body adapter** — only the *transport/auth* differs, which validates
    the body/transport split below.

### 3.2 Architecture: neutral request → body adapter → transport

```
ai.generate(opts)
   │  parse opts → NeutralRequest { messages, system, tools, shape, params }   (Rust struct)
   ▼
ProviderAdapter (trait, in Rust):
   - build_request(&NeutralRequest) -> HttpRequestSpec { method, url, headers, json_body }
   - parse_response(bytes) -> NeutralResponse { text, usage, finish, tool_calls, raw }
   - decode_stream_chunk(sse_event) -> Option<NeutralChunk>     // streaming
   ▼
Transport: the existing reqwest client in net_http.rs (pooled), or the SSE reader for streaming.
   ▼
Auth: a small AuthScheme per provider (Bearer / x-api-key / ?key= / [Tier B] SigV4 / OAuth).
```

The **body adapter** and the **transport/auth** are deliberately separate so Tier-B
(Bedrock/Vertex) can pair the Anthropic/OpenAI body adapter with a different auth/transport.
This is the single most important structural decision and it directly reuses `net_http.rs` for
transport + SSE.

### 3.3 Why native adapters over a Rust crate

- `genai` (jeremychone) and `rig-core` (0xPlaygrounds) both prove the multi-provider native-
  adapter approach is the right model — and both are reqwest-based. But adopting either as a
  dependency drags in their *own* type system, runtime assumptions, and (for rig) an agent
  framework we don't want. `async-openai` is OpenAI-only.
- The interpreter is `!Send` / current-thread; we already own a tuned reqwest client, SSE parser,
  retry/timeout/proxy/TLS plumbing, and the `Value`↔JSON converter (`stdlib::json`). The adapters
  are *mostly* JSON shaping — a few hundred lines each — and reuse all of the above.
- This keeps the dep tree small and avoids unstable/heavy crates, matching the repo's stated
  bias and the `http3`-is-opt-in-because-unstable precedent.
- **We should still read `genai`'s provider-quirk handling as a reference** (it encodes years of
  per-provider edge cases) — borrow the knowledge, not the dependency.

---

## 4. `std/telemetry` — proposed API

```ascript
import * as telemetry from "std/telemetry"
```

A thin, vendor-neutral facade over three observability primitives + pluggable exporters. Stateful
singleton on `Interp` (like `std/log`): configured once, captures in tests.

### 4.1 Setup & exporters

```ascript
telemetry.init({
  service: "my-app",
  exporters: [
    telemetry.otlp({ endpoint: "http://localhost:4318", protocol: "http/protobuf" }),
    telemetry.sentry({ dsn: env.get("SENTRY_DSN") }),
    telemetry.posthog({ apiKey: env.get("POSTHOG_KEY"), host: "https://us.i.posthog.com" }),
  ],
})
```

Each exporter is a small handle describing where signals go. `otlp` covers any OpenTelemetry
backend (Jaeger, Grafana Tempo, **Langfuse via its OTLP endpoint**, Datadog, etc.). `sentry` is
error/performance. `posthog` is product-event capture. Absent `init`, telemetry is a no-op
(zero-cost) — same "safe to leave in production" guarantee as `std/log`.

### 4.2 Tracing / spans

```ascript
// Explicit span with manual end:
let span = telemetry.startSpan("handle-request", { attributes: { route: "/users" } })
span.setAttribute("user.id", uid)
span.addEvent("cache-miss")
// ... work ...
span.setStatus("ok")        // "ok" | "error"
span.end()

// Scoped helper that times + auto-ends + records a thrown panic as error status,
// and returns the inner value as a Tier-1 pair:
let [result, err] = await telemetry.span("db-query", async fn() {
  return await db.query("...")
})
```

Spans nest via the current-task context (a thread-local current-span stack, analogous to the
generator stack in `coro.rs`). A child span started inside `telemetry.span(...)`'s callback
parents to it automatically.

### 4.3 Metrics

```ascript
let reqCount = telemetry.counter("http.requests", { unit: "1" })
reqCount.add(1, { route: "/users", method: "GET" })

let latency = telemetry.histogram("http.latency", { unit: "ms" })
latency.record(12.4, { route: "/users" })

let inflight = telemetry.gauge("http.inflight")
inflight.set(7)
```

Counter / histogram / gauge — the OTel metric instrument set, projected to AScript handles.

### 4.4 Event capture (analytics)

```ascript
telemetry.capture("signup_completed", {
  distinctId: userId,
  properties: { plan: "pro", referrer: "hn" },
})
telemetry.identify(userId, { email: "a@b.com", plan: "pro" })
```

`capture`/`identify` map to the **PostHog** `/capture` + `/batch` HTTP API (api_key + distinct_id
+ event + properties). When an OTLP exporter is also configured, `capture` can *additionally*
emit an OTel log/event — propose: events go to PostHog by default; mirroring to OTel is a config
flag.

### 4.5 Exporter mapping (research-grounded)

| Exporter | Backed by | Signals | Notes |
|---|---|---|---|
| `otlp` | OTLP HTTP (`http/protobuf` or `http/json`) over reqwest | traces, metrics, logs | Vendor-neutral; **Langfuse** ingests via its OTLP endpoint, so Langfuse needs no special exporter. Honors `OTEL_EXPORTER_OTLP_ENDPOINT`. |
| `sentry` | Sentry ingest (envelope) HTTP API | errors, transactions (spans) | Map a span tree → Sentry transaction; map a Tier-2 panic / `span.setStatus("error")` → Sentry event. |
| `posthog` | PostHog `/capture` + `/batch` HTTP API | events, identify | Batch + flush on interval / size (20MB cap). |

> **Rust dependency fork (see §5):** do we depend on the official `opentelemetry` + `opentelemetry-otlp`
> crates (heavy, but spec-correct, supports gRPC) — or hand-roll the OTLP **HTTP/JSON** export over
> the reqwest client we already have (tiny, no new deps, HTTP-only)? Leaning **hand-rolled
> OTLP/HTTP-JSON for v1** to stay lean (consistent with §3.3), revisiting the official crate if
> gRPC/full-fidelity is needed. Sentry + PostHog are plain HTTP APIs → always hand-rolled over
> reqwest, no `sentry`/`posthog` crate needed.

### 4.6 The `std/ai` ↔ `std/telemetry` relationship

- **`std/ai` emits OTel GenAI spans *through* `std/telemetry` when present.** Each `ai.generate` /
  `ai.stream` / `ai.embed` opens a span named per the GenAI conventions
  (`gen_ai.system`=provider, `gen_ai.request.model`, `gen_ai.request.temperature/max_tokens`,
  `gen_ai.response.finish_reasons`, `gen_ai.usage.input_tokens` / `output_tokens`); a tool call →
  a child `ai.toolCall` span. This is the AI-SDK `experimental_telemetry` analog and makes
  **Langfuse / any OTel backend** light up automatically.
- **Soft, runtime-optional coupling.** `std/ai` checks at runtime whether telemetry is configured;
  if not, it's a no-op. Propose `std/ai` does **NOT** hard-depend on the `telemetry` Cargo feature
  — it calls an internal hook on `Interp` that telemetry installs, so both build independently
  under `--no-default-features` + their own feature. (This is the §5 fork: feature dep vs runtime
  hook — leaning runtime hook.)
- **Per-call control** mirrors `experimental_telemetry`:
  `ai.generate({ ..., telemetry: { enabled: true, functionId: "summarize", recordInputs: false } })`
  (off by default for prompt/response *content* to avoid logging PII; usage/timing always on when
  telemetry is configured).

---

## 5. Open design questions for the owner (the real forks)

1. **v1 provider set.** Confirm Tier A = `openai-compatible` + `anthropic` + `google`, with
   Bedrock/Vertex deferred to Tier B. Is xAI/Grok wanted as its *own* named provider or just
   "openai-compatible with a baseUrl"? Is Azure OpenAI v1 (key auth) enough, or is Azure AI
   Foundry / Entra-ID auth needed early?

2. **Streaming surface.** One `ai.stream` yielding typed chunks (`{type,...}`) + a `textOnly()`
   convenience, or *also* a separate `ai.streamText` yielding bare strings? Do we want streaming
   **structured** output (`partialOutputStream`) in v1, or defer it?

3. **Does `std/ai` depend on `std/telemetry`?** Runtime hook (both build independently; leaning
   this) vs a Cargo feature dependency (`ai` ⇒ `telemetry`). And: do AI spans default ON when
   telemetry is configured, or strictly opt-in per call?

4. **Credential handling.** Env-var-by-default + explicit-config-override (proposed) — is that the
   right default, or should AScript require explicit credential objects (no implicit env reads) for
   auditability? Missing credential = Tier-1 `err` (proposed) vs Tier-2 panic?

5. **Structured-output mechanism.** Accept BOTH class (`shape: Recipe` → `validate_into`) AND
   `std/schema` (proposed), or pick one? Either way we need a **new class/schema → JSON-Schema
   projector** to send to the provider — confirm that's in scope (it's the only genuinely new core
   mechanism `std/ai` requires).

6. **Tool-calling ergonomics.** `ai.tool({description, input, execute})` with `execute` returning
   Tier-1 `[value, err]` and the loop capped by `maxSteps` (proposed). Should tool errors be
   fed back to the model (recoverable, proposed) or abort the call? Do we want parallel tool-call
   execution (providers can emit several at once), or sequential in v1?

7. **Wrap a Rust crate vs build native.** Confirm the **native-over-reqwest-adapters** recommendation
   (§3.3) over adopting `genai`/`rig-core`. If a crate is preferred for speed-to-ship, `genai` is
   the closest fit — but it changes the dep-tree and `!Send`/runtime story.

8. **Telemetry exporter deps.** Hand-rolled OTLP/HTTP-JSON over reqwest (lean, HTTP-only; proposed)
   vs the official `opentelemetry`/`opentelemetry-otlp` crates (heavy, gRPC, spec-complete). And
   should `std/telemetry` ship all three exporters (OTLP/Sentry/PostHog) in v1, or just OTLP +
   leave Sentry/PostHog as follow-ups?

9. **Testing & secrets.** Confirm the record/replay fixture approach: adapters take an injectable
   HTTP-send seam; unit tests replay recorded JSON/SSE fixtures (no network); a `--features net`
   suite hits a local mock server; real-provider tests are env-gated and CI-excluded. Where do
   fixtures live (`tests/fixtures/ai/...`)?

10. **Feature flags.** Proposed: `ai` (Tier A) and `telemetry` as separate default-on-or-off
    features (likely **off-by-default** given they're new + network — unlike the existing
    default-on stack). `ai-bedrock` / `ai-vertex` as opt-in Tier-B sub-features. Confirm
    default-on vs opt-in.

---

## 6. Sources

**Vercel AI SDK (v5):**
- [AI SDK Core: Generating Text](https://ai-sdk.dev/docs/ai-sdk-core/generating-text)
- [AI SDK Core: Generating Structured Data](https://ai-sdk.dev/docs/ai-sdk-core/generating-structured-data)
- [AI SDK: Providers and Models](https://ai-sdk.dev/docs/foundations/providers-and-models)
- [AI SDK Core: Provider Registry](https://ai-sdk.dev/docs/reference/ai-sdk-core/provider-registry)
- [AI SDK Core: Telemetry (experimental_telemetry)](https://ai-sdk.dev/docs/ai-sdk-core/telemetry)
- [AI SDK 5 announcement (Vercel)](https://vercel.com/blog/ai-sdk-5)
- [vercel/ai (GitHub)](https://github.com/vercel/ai)

**Provider API shapes / auth:**
- [Anthropic Messages vs OpenAI Chat Completions (Portkey)](https://portkey.ai/blog/open-ai-responses-api-vs-chat-completions-vs-anthropic-anthropic-messages-api/)
- [Anthropic Claude Messages API on Amazon Bedrock (AWS docs)](https://docs.aws.amazon.com/bedrock/latest/userguide/model-parameters-anthropic-claude-messages.html)
- [LiteLLM Anthropic provider docs](https://docs.litellm.ai/docs/providers/anthropic)

**OpenTelemetry GenAI semantic conventions:**
- [Semantic conventions for generative AI systems](https://opentelemetry.io/docs/specs/semconv/gen-ai/)
- [GenAI client spans](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/)
- [GenAI metrics](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-metrics/)
- [Gen AI attribute registry](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)

**Observability backends:**
- [Langfuse Public/Ingestion API](https://langfuse.com/docs/api-and-data-platform/features/public-api)
- [PostHog Capture & batch API](https://posthog.com/docs/api/capture)
- [Sentry (errors + performance)](https://sentry.io/) — Rust crate `sentry`

**Rust ecosystem:**
- [opentelemetry-rust (GitHub)](https://github.com/open-telemetry/opentelemetry-rust)
- [opentelemetry-otlp (crates.io)](https://crates.io/crates/opentelemetry-otlp)
- [jeremychone/rust-genai (GitHub)](https://github.com/jeremychone/rust-genai)
- [0xPlaygrounds/rig (GitHub)](https://github.com/0xPlaygrounds/rig)

**AScript internals (repo):**
- `src/stdlib/net_http.rs` (reqwest client, SSE reader, Tier-1 pairs, native resource handles)
- `docs/content/stdlib/{net,schema,log,stream}.md`; `docs/content/language/classes-enums.md`
- `Cargo.toml [features]`; `CLAUDE.md` (async model, `validate_into`, resource-handle invariants)
