// std/schema collect-all-errors mode (parseAll).
//
// `schema.parse` is fail-fast: it returns the FIRST validation error.
// `schema.parseAll` keeps going and returns EVERY error as an array of
// {path, message} objects, in deterministic document order.
import * as schema from "std/schema"
let form = schema.object({
  name: schema.minLength(schema.string(), 1),
  age: schema.min(schema.number(), 0),
  email: schema.string(),
})
// All three fields are wrong: name empty, age negative, email is a number.
let bad = { name: "", age: -3, email: 42 }
let [val, errs] = schema.parseAll(form, bad)
assert(val == nil, "parseAll failure: value is nil")
assert(errs != nil, "parseAll failure: errors set")
assert(len(errs) == 3, `expected 3 errors, got ${len(errs)}`)
for (e in errs) {
  print(`${e.path}: ${e.message}`)
}
// Fluent method form works too.
let [_v2, errs2] = form.parseAll(bad)
assert(len(errs2) == 3, "fluent parseAll: 3 errors")
// A fully-valid value: errors is nil, value is the validated object.
let good = { name: "Ada", age: 36, email: "ada@example.com" }
let [okVal, okErrs] = schema.parseAll(form, good)
assert(okErrs == nil, "parseAll success: errors nil")
assert(okVal.name == "Ada", "parseAll success: value validated")
// `parse` (fail-fast) still returns a single error object, not an array.
let [_fbVal, firstErr] = schema.parse(form, bad)
assert(firstErr.path == "name", "parse fail-fast: first error is name")
print("schema_collect: all assertions passed")
