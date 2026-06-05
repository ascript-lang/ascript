// std/template — minimal {{name}} string templating (core module).
import * as template from "std/template"

// Simple + dotted-path substitution. Non-string values stringify canonically.
let data = { name: "Ada", account: { plan: "pro", seats: 5 } }
let [text, err] = template.render(
  "Hi {{name}} — plan {{account.plan}} ({{account.seats}} seats)",
  data,
)
assert(err == nil, `render err: ${err}`)
assert(text == "Hi Ada — plan pro (5 seats)", `got: ${text}`)
print(text)

// Whitespace inside the braces is trimmed.
let [t2, _e2] = template.render("{{ name }}", data)
assert(t2 == "Ada", "whitespace trimmed")

// Missing keys are a Tier-1 error (strict; the message names the path).
let [bad, merr] = template.render("{{nope}}", data)
assert(bad == nil, "missing key: nil value")
assert(merr != nil, "missing key: err set")
print(`missing key rejected: ${merr.message}`)

print("template_render: all assertions passed")
