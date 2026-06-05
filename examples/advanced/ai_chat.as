// std/ai — multi-provider LLM client (SP11).
//
// This example is production-shaped and fully error-handled. It needs a built
// binary with the `ai` feature (`cargo build --features ai`) and a provider
// credential to produce real output; with NO key it exits cleanly via the Tier-1
// error path (it never panics), so it is safe to run anywhere.
//
//   OPENAI_API_KEY=sk-... target/release/ascript run examples/advanced/ai_chat.as
//   # or point at a local Ollama:  (no key needed)
//
import * as ai from "std/ai"

// 1) Plain text generation. A missing credential is a Tier-1 [nil, err], not a
//    crash — we handle it and move on.
let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  system: "You are a terse assistant.",
  prompt: "Summarize the CAP theorem in one sentence.",
  maxTokens: 128,
  temperature: 0.2,
})
if (err != nil) {
  print("generate unavailable: " + err.message)
} else {
  print("ANSWER: " + out.text)
  print(`tokens: ${out.usage.totalTokens}`)
}

// 2) Streaming the same kind of request, chunk by chunk. A mid-stream provider
//    error surfaces from `next()`; we drive the stream defensively so a missing
//    provider degrades to a clean message instead of aborting the program.
let [stream, serr] = await ai.stream({ model: "openai:gpt-4.1", prompt: "Count to three." })
if (serr != nil) {
  print("stream unavailable: " + serr.message)
} else {
  let streaming = true
  while (streaming) {
    let [chunk, cerr] = await stream.next()
    if (cerr != nil) { print("stream error: " + cerr.message); streaming = false }
    else if (chunk == nil) { streaming = false }
    else if (chunk.type == "text") { print(chunk.text, { end: "" }) }
  }
  print("")
}

// 3) An OpenAI-compatible local endpoint (e.g. Ollama) via an explicit handle.
//    Swap in your local server; with nothing listening it returns a Tier-1 error.
let local = ai.provider("openai-compatible", {
  baseUrl: "http://localhost:11434/v1",
  apiKey: "ollama",
})
let [lout, lerr] = await ai.generate({ model: local.model("llama3.1"), prompt: "ping" })
if (lerr != nil) {
  print("local model unavailable: " + lerr.message)
} else {
  print("LOCAL: " + lout.text)
}
