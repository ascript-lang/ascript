:::eyebrow Standard library

# Data & serialization

AScript ships a family of data-handling modules for the formats you reach for every day: JSON, CSV, TOML, and YAML serialization; base64/hex/URL/UTF-8 encoding; regular expressions; and UUID generation. All seven modules — `std/json`, `std/csv`, `std/toml`, `std/yaml`, `std/encoding`, `std/regex`, and `std/uuid` — are provided by the `data` Cargo feature, which is enabled by default. If you build AScript with a custom feature set, include `data` to keep these modules available.

> [!TIER1] Fallible functions return a two-element `[value, err]` pair — `err` is `nil` on success. Destructure: `let [v, e] = json.parse(s)`.

## std/json

JSON parsing and serialization. Objects are decoded into insertion-ordered AScript objects, so key order from the source text is preserved.

### json.parse

Parses a JSON string into an AScript value.

- `text` (string) — the JSON source text.
- Returns `[value, err]` — the decoded value, or `nil` plus an error if the text is not valid JSON.

> [!TIER1] On invalid JSON, returns `[nil, err]` rather than panicking.

```ascript
let [data, err] = json.parse("{\"name\": \"Ada\", \"age\": 36, \"tags\": [true, null]}")
// data == { name: "Ada", age: 36, tags: [true, nil] }
// err  == nil

let [bad, e] = json.parse("{not valid")
// bad == nil
// e   == { message: "invalid JSON: ..." }
```

#### Typed parse: `json.parse(text, Class)`

Passing a [class](../language/classes-enums) as a second argument parses **and validates** in one
step. A parse failure and a shape mismatch are **fused into one Tier-1 `[value, err]` pair** —
neither panics. On success the value is a validated instance (with defaults applied and optional
fields defaulted to nil), exactly as if you had called [`Class.from`](../language/classes-enums) on
the decoded object. The class is an ordinary value argument (no generics). With **no** class
argument, `json.parse` returns the raw decoded value unchanged, as above.

```ascript
class User {
  id: number
  name: string
  nickname: string?
  role: string = "guest"
}

let [u, err] = json.parse("{\"id\": 1, \"name\": \"Ada\"}", User)
// u.id == 1, u.role == "guest" (default), u.nickname == nil; err == nil

// A shape mismatch fuses into the err channel (NOT a panic):
let [bad, e] = json.parse("{\"id\": \"x\", \"name\": \"Bug\"}", User)
// bad == nil; e.message describes the bad field

// Malformed JSON surfaces in the same channel:
let [bad2, e2] = json.parse("{not json", User)   // bad2 == nil; e2 != nil
```

Because `?` and `!` compose with this, a validating loader is a single line:

```ascript
fn loadUser(text: string): Result<User> {
  let user = json.parse(text, User)?    // propagates [nil, err] on bad JSON or bad shape
  return Ok(user)
}
```

See `examples/typed_parse.as` for a runnable walkthrough, and the
[HTTP `resp.json(Class)`](net) accessor for the over-the-wire equivalent.

### json.stringify

Serializes an AScript value to a JSON string.

- `value` (any) — the value to serialize. Supports `nil`, booleans, numbers, strings, arrays, objects, and string-keyed maps.
- `pretty` (boolean or number, optional) — when `true` (or a positive number), emits indented, multi-line output. Defaults to compact output.
- Returns `[text, err]` — the JSON text, or `nil` plus an error if the value cannot be serialized (e.g. a non-finite number, a function, a non-string map key, or a cyclic structure).

Integer-valued numbers serialize without a trailing `.0` (e.g. `1`, not `1.0`).

> [!TIER1] A non-serializable value yields `[nil, err]` rather than panicking.

```ascript
let [text, err] = json.stringify({ n: 2 })
// text == "{\"n\":2}"
// err  == nil

let [pretty, _] = json.stringify({ n: 2 }, true)
// pretty == "{\n  \"n\": 2\n}"

let [out, e] = json.stringify({ f: print })
// out == nil
// e   == { message: "cannot serialize a value of type builtin to JSON" }
```

## std/csv

CSV parsing and serialization, backed by the `csv` crate. Parsing is lenient: irregular quoting and ragged rows are coerced rather than rejected. Output rows are terminated with `\n` for predictable cross-platform results.

### csv.parse

Parses CSV text into an array of rows.

- `text` (string) — the CSV source text.
- `options` (object, optional) — see the table below.
- Returns `[rows, err]` — an array of rows, or `nil` plus an error on a genuine reader error (I/O or UTF-8). With `header: true`, each row is an object keyed by the header line; otherwise each row is an array of string fields.

| Option   | Type    | Default | Effect                                                                              |
| -------- | ------- | ------- | ----------------------------------------------------------------------------------- |
| `header` | boolean | `false` | When `true`, treats the first row as column names and yields one object per data row |

> [!TIER1] Reader errors return `[nil, err]`; malformed-but-readable input is coerced, not rejected.

```ascript
let [rows, err] = csv.parse("a,b\n1,2\n3,4")
// rows == [["a", "b"], ["1", "2"], ["3", "4"]]
// err  == nil

let [people, _] = csv.parse("name,age\nAda,36", { header: true })
// people == [{ name: "Ada", age: "36" }]
```

### csv.stringify

Serializes an array of rows to CSV text.

- `rows` (array) — either an array of arrays (each inner array is one row of fields) or an array of objects. The first element determines the shape; for objects, the keys of the first object become the header row (in insertion order).
- Returns `[text, err]` — the CSV text, or `nil` plus an error if rows mix kinds (e.g. an object where arrays are expected) or are not arrays/objects.

> [!TIER1] Mixed or invalid row kinds yield `[nil, err]`.

```ascript
let [text, err] = csv.stringify([["x", "y"], [1, 2]])
// text == "x,y\n1,2\n"
// err  == nil

let [out, _] = csv.stringify([{ name: "Ada", age: 36 }])
// out == "name,age\nAda,36\n"
```

## std/toml

TOML parsing and serialization. Values bridge through the same converter as `std/json`, so decoded tables are insertion-ordered objects. The TOML top level must be a table, so `toml.stringify` requires an object.

### toml.parse

Parses a TOML string into an AScript value.

- `text` (string) — the TOML source text.
- Returns `[value, err]` — the decoded value, or `nil` plus an error if the text is not valid TOML.

> [!TIER1] Invalid TOML returns `[nil, err]`.

```ascript
let [config, err] = toml.parse("name = \"Ada\"\nage = 36")
// config == { name: "Ada", age: 36 }
// err    == nil

let [bad, e] = toml.parse("= bad")
// bad == nil
// e   == { message: "invalid TOML: ..." }
```

### toml.stringify

Serializes an AScript value to TOML text.

- `value` (object) — the value to serialize. The TOML top level must be a table, so a bare scalar or array yields an error.
- Returns `[text, err]` — the TOML text, or `nil` plus an error if the value cannot be represented.

> [!TIER1] A value that cannot sit at the TOML top level (e.g. a bare number) yields `[nil, err]`.

```ascript
let [text, err] = toml.stringify({ k: "v" })
// text == "k = \"v\"\n"
// err  == nil

let [out, e] = toml.stringify(5)
// out == nil
// e   == { message: "cannot serialize to TOML: ..." }
```

## std/yaml

YAML parsing and serialization. Like TOML, YAML bridges through the JSON converter, so mappings decode to insertion-ordered objects.

### yaml.parse

Parses a YAML string into an AScript value.

- `text` (string) — the YAML source text.
- Returns `[value, err]` — the decoded value, or `nil` plus an error if the text is not valid YAML.

> [!TIER1] Invalid YAML returns `[nil, err]`.

```ascript
let [doc, err] = yaml.parse("name: Ada\nage: 36\ntags:\n  - a\n  - b")
// doc == { name: "Ada", age: 36, tags: ["a", "b"] }
// err == nil
```

### yaml.stringify

Serializes an AScript value to YAML text.

- `value` (any) — the value to serialize, following the same serialization rules as JSON.
- Returns `[text, err]` — the YAML text, or `nil` plus an error if the value cannot be serialized.

> [!TIER1] A non-serializable value yields `[nil, err]`.

```ascript
let [text, err] = yaml.stringify({ x: 1 })
// text == "x: 1\n"
// err  == nil
```

## std/encoding

Binary and text encoding helpers: base64, hex, URL percent-encoding, and UTF-8 conversion between strings and byte arrays. The `*Encode` functions return a value directly (no error pair); the `*Decode` functions are fallible and return a `[value, err]` pair. Functions that consume raw bytes accept either a `bytes` value or a string (encoded as UTF-8).

### encoding.base64Encode

Encodes bytes or a string as a standard base64 string.

- `data` (bytes or string) — the raw input.
- Returns a base64 string.

```ascript
let s = encoding.base64Encode("hello")
// s == "aGVsbG8="
```

### encoding.base64Decode

Decodes a standard base64 string into bytes.

- `text` (string) — the base64 input.
- Returns `[bytes, err]` — the decoded bytes, or `nil` plus an error if the input is not valid base64.

> [!TIER1] Invalid base64 returns `[nil, err]`.

```ascript
let [bytes, err] = encoding.base64Decode("aGVsbG8=")
// bytes == <bytes len 5>
// err   == nil

let [bad, e] = encoding.base64Decode("!!!notb64")
// bad == nil
// e   == { message: "invalid base64: ..." }
```

### encoding.hexEncode

Encodes bytes or a string as a lowercase hexadecimal string.

- `data` (bytes or string) — the raw input.
- Returns a hex string.

```ascript
let s = encoding.hexEncode("AB")
// s == "4142"
```

### encoding.hexDecode

Decodes a hexadecimal string into bytes.

- `text` (string) — the hex input.
- Returns `[bytes, err]` — the decoded bytes, or `nil` plus an error if the input is not valid hex.

> [!TIER1] Invalid hex returns `[nil, err]`.

```ascript
let [bytes, err] = encoding.hexDecode("4142")
// bytes == <bytes len 2>
// err   == nil

let [bad, e] = encoding.hexDecode("zz")
// bad == nil
// e   == { message: "invalid hex: ..." }
```

### encoding.urlEncode

Percent-encodes a string for use in a URL. All non-alphanumeric characters are escaped.

- `text` (string) — the input to encode.
- Returns a percent-encoded string.

```ascript
let s = encoding.urlEncode("a b&c")
// s == "a%20b%26c"
```

### encoding.urlDecode

Decodes a percent-encoded string.

- `text` (string) — the percent-encoded input.
- Returns `[text, err]` — the decoded string, or `nil` plus an error if the result is not valid UTF-8.

> [!TIER1] Invalid UTF-8 in the decoded output returns `[nil, err]`.

```ascript
let [text, err] = encoding.urlDecode("a%20b%26c")
// text == "a b&c"
// err  == nil
```

### encoding.utf8Encode

Encodes a string into its UTF-8 bytes.

- `text` (string) — the input string.
- Returns a `bytes` value.

```ascript
let b = encoding.utf8Encode("hi")
// b == <bytes len 2>
```

### encoding.utf8Decode

Decodes a byte array into a string, validating UTF-8.

- `data` (bytes) — the raw bytes.
- Returns `[text, err]` — the decoded string, or `nil` plus an error if the bytes are not valid UTF-8.

> [!TIER1] Invalid UTF-8 returns `[nil, err]`.

```ascript
let [text, err] = encoding.utf8Decode(encoding.utf8Encode("hi"))
// text == "hi"
// err  == nil
```

## std/regex

Regular expressions, backed by the `regex` crate.

The calling convention is value-passing, not method-based. `regex.compile` returns a first-class **Regex value**; that value has **no methods**. To use it, pass the Regex value (or a pattern string) as the **first argument** to `regex.test`, `regex.find`, `regex.findAll`, `regex.replace`, or `regex.split`. Compiling once and reusing the value avoids recompiling the pattern on every call.

There are two ways to supply a pattern, and they fail differently:

- **Compiled value** — `regex.compile(pattern)` validates the pattern up front and returns a `[regex, err]` pair (a Tier-1 error). This is the safe path for untrusted patterns.
- **Inline string** — passing a pattern string directly to `test`/`find`/`findAll`/`replace`/`split` compiles it on the fly. An invalid inline pattern is a **Tier-2 panic**, not a result pair.

> [!TIER2] An invalid inline pattern string passed to `test`/`find`/`findAll`/`replace`/`split` raises a Tier-2 panic. Use `regex.compile` for the Tier-1 (result-pair) path when the pattern is untrusted. Passing a non-regex, non-string value as the pattern is likewise a Tier-2 panic.

### regex.compile

Compiles a pattern string into a reusable Regex value.

- `pattern` (string) — the regular expression source.
- Returns `[regex, err]` — the compiled Regex value, or `nil` plus an error if the pattern is invalid.

> [!TIER1] An invalid pattern returns `[nil, err]` (unlike inline patterns, which panic).

```ascript
let [re, err] = regex.compile("[a-z]+")
// re  == <regex [a-z]+>
// err == nil

let [bad, e] = regex.compile("(")
// bad == nil
// e   == { message: "invalid regex: ..." }
```

### regex.test

Reports whether the pattern matches anywhere in the string.

- `pattern` (regex or string) — a compiled Regex value or an inline pattern string.
- `text` (string) — the string to test.
- Returns a boolean.

> [!TIER2] An invalid inline pattern string panics.

```ascript
let [re, _] = regex.compile("\\d+")
regex.test(re, "ab12")    // true   (reusing a compiled value)
regex.test("\\d+", "abc") // false  (inline pattern)
```

### regex.find

Finds the first match and its capture groups.

- `pattern` (regex or string) — a compiled Regex value or an inline pattern string.
- `text` (string) — the string to search.
- Returns `nil` if there is no match, otherwise an object with `text` (the whole match), `index` (the match's start position as a character offset), and `groups` (an array of capture-group strings, with `nil` for groups that did not participate).

> [!TIER2] An invalid inline pattern string panics.

```ascript
let m = regex.find("(\\d)(\\d)", "x42y")
// m == { text: "42", index: 1, groups: ["4", "2"] }

let none = regex.find("\\d+", "abc")
// none == nil
```

### regex.findAll

Finds every non-overlapping match.

- `pattern` (regex or string) — a compiled Regex value or an inline pattern string.
- `text` (string) — the string to search.
- Returns an array of matched substrings (capture groups are not included).

> [!TIER2] An invalid inline pattern string panics.

```ascript
let all = regex.findAll("\\d", "a1b2")
// all == ["1", "2"]
```

### regex.replace

Replaces every match with a replacement string.

- `pattern` (regex or string) — a compiled Regex value or an inline pattern string.
- `text` (string) — the string to operate on.
- `replacement` (string) — the replacement text. Capture-group references such as `$1` are expanded by the underlying engine.
- Returns the resulting string.

> [!TIER2] An invalid inline pattern string panics.

```ascript
let out = regex.replace("\\d", "a1b2", "#")
// out == "a#b#"
```

### regex.split

Splits a string on every match of the pattern.

- `pattern` (regex or string) — a compiled Regex value or an inline pattern string.
- `text` (string) — the string to split.
- Returns an array of the substrings between matches.

> [!TIER2] An invalid inline pattern string panics.

```ascript
let parts = regex.split(",\\s*", "a, b,c")
// parts == ["a", "b", "c"]
```

## std/uuid

UUID generation. Both functions return a 36-character canonical UUID string and take no arguments.

### uuid.v4

Generates a random (version 4) UUID.

- Takes no arguments.
- Returns a UUID string. Successive calls produce distinct values.

```ascript
let id = uuid.v4()
// id == "3b241101-e2bb-4255-8caf-4136c566a962"  (random, 36 chars)
```

### uuid.v7

Generates a time-ordered (version 7) UUID based on the current timestamp.

- Takes no arguments.
- Returns a UUID string. Values are monotonically ordered by creation time, which makes them well-suited as sortable database keys.

```ascript
let id = uuid.v7()
// id == "018f9b4e-3a7c-7c1d-9f2a-1b2c3d4e5f60"  (time-ordered, 36 chars)
```
