# `std/ai` — a unified, multi-provider LLM client

`std/ai` is a single, AScript-idiomatic interface to every major LLM provider. It
wraps the Rust [`genai`](https://crates.io/crates/genai) crate, so one import covers:

- **OpenAI** and every **OpenAI-compatible** endpoint — Ollama, LM Studio,
  OpenRouter, LiteLLM, xAI / Grok, DeepSeek, groq, together, Azure OpenAI (key auth);
- **native Anthropic** and **native Google Gemini**;
- **AWS Bedrock** (SigV4 via the AWS credential chain) and **GCP Vertex AI** (ADC).

```ascript
import * as ai from "std/ai"
```

> **Build note.** `std/ai` is behind the off-by-default `ai` Cargo feature (network
> + heavy provider deps). Build the `ascript` binary with `cargo build --features ai`
> to use it. The `import "std/ai"` is always accepted by the static checker, even in
> a build without the feature.

## Model selection & credentials

A model is a `"provider:model"` string; credentials resolve from environment
variables by default.

```ascript
let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  prompt: "Write a haiku about Rust ownership.",
})

let [out2, _] = await ai.generate({ model: "anthropic:claude-sonnet-4.5", prompt: "..." })
let [out3, _] = await ai.generate({ model: "google:gemini-2.5-flash", prompt: "..." })

// Bedrock (SigV4 / AWS credential chain) and Vertex (ADC) use ambient creds:
let [b, _] = await ai.generate({ model: "bedrock:anthropic.claude-sonnet-4.5", prompt: "..." })
let [v, _] = await ai.generate({ model: "vertex:gemini-2.5-flash", prompt: "..." })
```

For an OpenAI-compatible endpoint (Ollama, LM Studio, OpenRouter, LiteLLM, …) or
Azure OpenAI, build a **provider handle** with an explicit base URL / key:

```ascript
let local = ai.provider("openai-compatible", {
  baseUrl: "http://localhost:11434/v1",
  apiKey:  "ollama",
})
let [out, _] = await ai.generate({ model: local.model("llama3.1"), prompt: "..." })

let azure = ai.provider("azure", {
  baseUrl: "https://my.openai.azure.com",
  apiKey: env.get("AZURE_OPENAI_KEY"),
  apiVersion: "2024-10-21",
})
let [a, _] = await ai.generate({ model: azure.model("my-gpt4o-deployment"), prompt: "..." })
```

Provider kinds: `"openai"`, `"openai-compatible"`, `"azure"`, `"anthropic"`,
`"google"` (alias `"gemini"`), `"bedrock"`, `"vertex"`, plus the OpenAI-compat
presets `"ollama"`, `"groq"`, `"xai"`, `"deepseek"`, `"together"`, `"fireworks"`,
`"cohere"`, `"openrouter"`, `"nebius"`, `"moonshot"`. An unknown kind is a Tier-2
(programmer-error) panic.

**Credential precedence:** an explicit `apiKey` on a provider handle › the
provider's environment variable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`GEMINI_API_KEY`, the AWS chain for Bedrock, ADC for Vertex, `AZURE_OPENAI_KEY`) ›
a **Tier-1** `[nil, { message: "no credential for provider 'openai'" }]`. A missing
key is an expected operational failure, not a panic.

## Text generation (non-streaming)

```ascript
let [out, err] = await ai.generate({
  model: "anthropic:claude-sonnet-4.5",
  system: "You are a terse assistant.",
  prompt: "Summarize the CAP theorem in one sentence.",
  maxTokens: 256,          // required by Anthropic; missing → Tier-1 err
  temperature: 0.2,
})
if (err != nil) { print(err.message); return }

print(out.text)
print(out.finishReason)            // "stop" | "length" | "tool_calls" | "content_filter" | ...
print(out.usage.inputTokens, out.usage.outputTokens, out.usage.totalTokens)
// out.toolCalls, out.steps, out.raw (the provider-native body) are also present.
```

- `prompt` (a string) and `messages` (an array, below) are **mutually exclusive** —
  setting both is a Tier-2 panic.
- A provider HTTP 4xx/5xx, network/TLS error, or content-filter refusal returns a
  **Tier-1** `[nil, err]`; `err.status` carries the HTTP status when there is one.

## Streaming (generators + `for await`)

```ascript
let [stream, err] = await ai.stream({ model: "openai:gpt-4.1", prompt: "Explain backpressure." })
if (err != nil) { print(err.message); return }

for await (chunk in stream) {
  if (chunk.type == "text") { print(chunk.text, { end: "" }) }
}
let final = stream.result()   // { text, finishReason, usage, toolCalls }
```

Each chunk is `{ type, ... }` where `type` is `"text"` (a `text` delta),
`"toolCall"` (`id`/`name`/`arguments`), or `"finish"` (carrying `finishReason` +
`usage`). `stream.textOnly()` is a convenience adapter that yields bare text
strings:

```ascript
for await (piece in stream.textOnly()) { print(piece, { end: "" }) }
```

A connection / provider error surfaces as a Tier-1 `[nil, err]` from
`await stream.next()` (genai sends the streaming request lazily, so the error
appears on the first poll, not on `ai.stream` itself).

## Structured / typed output

Pass a **class** or a **`std/schema`** as `shape:`. `std/ai` derives a JSON Schema
from it, asks the provider for matching JSON, then decodes + validates the result —
fusing a decode failure and a shape mismatch into ONE Tier-1 error (exactly like
`json.parse(text, Class)`).

```ascript
class Recipe { name: string  ingredients: array<string>  steps: array<string> }

let [recipe, err] = await ai.generate({
  model: "openai:gpt-4.1", prompt: "Give me a lasagna recipe.", shape: Recipe,
})
if (err != nil) { print("bad output: " + err.message); return }
print(recipe.name)            // a Recipe instance, fields type-checked

import * as schema from "std/schema"
let s = schema.object({ sentiment: schema.oneOf(["pos","neg","neu"]), score: schema.number() })
let [obj, e] = await ai.generate({ model: "openai:gpt-4.1", prompt: "...", shape: s })
```

The projector covers required / optional (`T?`) / nullable fields, nested classes,
`array<T>`, `map<K,V>`, defaults, and the `std/schema` kinds (object, array, string
with `minLength`/`maxLength`/`pattern`, number with `min`/`max`, bool, literal,
`oneOf`, `union`, `optional`, map). Streaming structured output is not in v1.

## Tool calling

A tool is `{ description, input: <schema|class>, execute: fn|async fn }`. The loop
runs **inside** `std/ai` (so it keeps the Tier-1 semantics): the model requests a
tool, `std/ai` validates the arguments against the tool's schema, calls `execute`,
feeds the JSON result back, and repeats up to `maxSteps` (default 5) until a final
answer.

```ascript
import * as schema from "std/schema"
let weather = ai.tool({
  description: "Get current weather for a city",
  input: schema.object({ city: schema.string() }),
  execute: async (args) => {
    let [resp, err] = await getWeather(args.city)
    if (err != nil) { return [nil, err] }       // tool errors are fed back to the model
    return [{ tempC: resp.temp, sky: resp.sky }, nil]
  },
})
let [out, err] = await ai.generate({
  model: "anthropic:claude-sonnet-4.5",
  prompt: "What should I wear in Lisbon today?",
  tools: { weather: weather },
  maxSteps: 5,
})
print(out.text)              // final answer
print(len(out.steps))        // per-turn { tool, arguments, result|error }
```

A tool whose `execute` returns `[nil, err]` is **recoverable**: the error is fed
back to the model as the tool result, not raised. A malformed tool *definition*
(missing `input`, non-callable `execute`) is a Tier-2 panic. Tools run
**sequentially** in v1.

## Message / content model

Instead of `prompt`, pass `messages` for multi-turn or multi-modal input:

```ascript
let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  messages: [
    { role: "system", content: "You are a vision assistant." },
    { role: "user", content: [
        { type: "text",  text: "What is in this image?" },
        { type: "image", data: imageBytes, mediaType: "image/png" },
    ]},
  ],
})
```

Roles: `"system"`, `"user"`, `"assistant"`, `"tool"`. Content is a string (text
shorthand) or an array of typed parts (`"text"`, `"image"`, `"file"`). An image/file
part's `data` is a URL string or `Value::Bytes`; `mediaType` is required.

## Embeddings

```ascript
let [emb, err]  = await ai.embed({ model: "openai:text-embedding-3-small", value: "sunny day" })
print(len(emb.embedding))           // emb == { embedding: [..], usage: { inputTokens } }

let [embs, e]   = await ai.embedMany({ model: "openai:text-embedding-3-small", values: ["a","b"] })
print(len(embs.embeddings))         // embs == { embeddings: [[..],[..]], usage }
```

## Observability (telemetry)

When [`std/telemetry`](telemetry) is initialized, `std/ai` automatically emits
OpenTelemetry **GenAI-convention** spans for every `generate` / `embed` (and a child
span per tool call) — so Langfuse or any OTel backend lights up with no AI-specific
configuration. Spans are named `chat {provider:model}` / `embeddings {model}` with
`gen_ai.*` attributes (operation, provider, model, temperature, max_tokens, finish
reason, token usage).

Tracing is **opt-in**: nothing is emitted unless telemetry is initialized. It is
**PII-safe by default** — usage / timing / model are recorded, but prompt and
response *content* are not unless you opt in:

```ascript
await ai.generate({ ..., telemetry: { recordInputs: true, recordOutputs: true } })
await ai.generate({ ..., telemetry: { enabled: false } })   // no span for this call
```

## Error model

| Situation | Result |
|---|---|
| Network/TLS/timeout, HTTP 4xx/5xx, missing credential, content-filter refusal, structured-output validation failure, missing required param (Anthropic `maxTokens`) | **Tier-1** `[nil, err]` (`err.message`, `err.status?`) |
| Wrong argument *types*, `prompt`+`messages` both set, malformed tool definition, unknown provider kind | **Tier-2** panic |
| A tool `execute` returns `[nil, err]` | Fed back to the model as a tool result (recoverable) |
