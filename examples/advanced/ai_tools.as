// std/ai — tool calling + structured output (SP11).
//
// Needs a binary built with `--features ai` and a provider key for real output;
// with no key it exits cleanly via the Tier-1 error path (never panics).
//
//   OPENAI_API_KEY=sk-... target/release/ascript run examples/advanced/ai_tools.as
//
import * as ai from "std/ai"
import * as schema from "std/schema"

// A tool the model can call. `execute` returns Tier-1 [value, err]; an error is fed
// back to the model as the tool result (recoverable), not raised.
let weather = ai.tool({
  description: "Get the current weather for a city",
  input: schema.object({ city: schema.string() }),
  execute: async (args) => {
    // A real tool would call an API here; we return a canned result.
    return [{ tempC: 18, sky: "clear", city: args.city }, nil]
  },
})

let [out, err] = await ai.generate({
  model: "openai:gpt-4.1",
  prompt: "What should I pack for a day trip to Lisbon?",
  tools: { weather: weather },
  maxSteps: 5,
})
if (err != nil) {
  print("tool run unavailable: " + err.message)
} else {
  print("ANSWER: " + out.text)
  print(`tool steps: ${len(out.steps)}`)
}

// Structured output: the model's reply is decoded + validated into a class. A bad
// shape (or no key) is a single fused Tier-1 error.
class Packing { items: array<string>  note: string }

let [plan, perr] = await ai.generate({
  model: "openai:gpt-4.1",
  prompt: "Give a short packing list for Lisbon in spring.",
  shape: Packing,
})
if (perr != nil) {
  print("structured output unavailable: " + perr.message)
} else {
  print("first item: " + plan.items[0])
  print("note: " + plan.note)
}
