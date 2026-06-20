:::eyebrow Standard library

# Data & serialization

AScript ships a family of data-handling modules for the formats you reach for every day: JSON, CSV, TOML, YAML, and XML serialization; MessagePack and CBOR binary serialization; exact decimal arithmetic; base64/hex/URL/UTF-8 encoding; regular expressions; UUID generation; and URL manipulation. The twelve modules — `std/json`, `std/csv`, `std/toml`, `std/yaml`, `std/xml`, `std/msgpack`, `std/cbor`, `std/decimal`, `std/encoding`, `std/regex`, `std/uuid`, and `std/url` — are provided by the `data` Cargo feature, which is enabled by default (`std/xml` additionally pulls the `xml` feature). If you build AScript with a custom feature set, include `data` to keep these modules available.

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

An optional trailing `strict` bool — `json.parse(text, Class, true)` — rejects any key not
declared on the class (at every nesting level), surfacing it in `err`. Omitted or `false`,
unknown keys are ignored (lenient, the default).

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

JSON preserves the numeric subtype: an `int` serializes with no decimal (`5`), a `float` always
serializes with one (`5.0`). `json.parse` mirrors this — a JSON number containing `.` or `e`/`E`
parses to a `float`, otherwise to an `int` — so `parse ∘ stringify` round-trips the subtype. (An
integral number outside `int`/`i64` range parses as a `float`, pending `bigint`.)

> [!TIER1] A non-serializable value yields `[nil, err]` rather than panicking.

```ascript
let [text, err] = json.stringify({ n: 2 })
// text == "{\"n\":2}"
// err  == nil

let [pretty, _] = json.stringify({ n: 2 }, true)
// pretty == "{\n  \"n\": 2\n}"

let [out, e] = json.stringify({ f: print })
// out == nil
// e   == { message: "cannot serialize a value of type function to JSON" }
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

#### Typed parse: `csv.parse(text, Class|schema, options?)`

Pass a class (or a [std/schema](schema) value) as the 2nd argument to validate **each
row** and get an array of typed instances. CSV cells are always strings, so typed rows
are validated **with coercion** — a `number` field accepts the cell `"36"` → `36`. Use
`{ header: true }` so rows are keyed objects matching the class fields. Validation is
fail-fast across rows: the first bad row's error carries a `row[N]` path prefix.

```ascript
class Row { name: string  age: number }
let [rows, err] = csv.parse("name,age\nAda,36\nGrace,37", Row, { header: true })
// rows[0].age == 36 (a Number); err == nil

let [bad, e] = csv.parse("name,age\nAda,notnum", Row, { header: true })
// bad == nil; e != nil  (row[0].age failed)
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

Like `json.parse`, a 2nd `Class | schema` argument validates the whole decoded
document and fuses a parse failure and a shape mismatch into one `[value, err]`:

```ascript
class Config { host: string  port: number }
let [cfg, err] = toml.parse("host = \"localhost\"\nport = 8080", Config)
// cfg.host == "localhost"; cfg.port == 8080; err == nil
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

A 2nd `Class | schema` argument validates the decoded document, exactly like
`json.parse` / `toml.parse`:

```ascript
class Config { host: string  port: number }
let [cfg, err] = yaml.parse("host: example.com\nport: 443", Config)
// cfg.host == "example.com"; cfg.port == 443; err == nil
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

## std/msgpack

MessagePack binary serialization (compact, schemaless). Provided by the `binary`
Cargo feature (default-on). Two functions:

- `msgpack.encode(value) -> bytes` — serialize any data value to MessagePack bytes.
  This is a total data mapping; it is a Tier-2 panic only on a genuinely
  unrepresentable handle (a function/native handle, etc.), never on data.
- `msgpack.decode(bytes) -> [value, err]` — Tier-1; malformed input → err channel.
- `msgpack.decode(bytes, Class|schema) -> [value, err]` — typed decode, fusing a
  decode failure and a shape mismatch into one pair (like `json.parse(text, Class)`).

The value mapping: numbers (integer-valued in range → integer, else float),
strings, bools, nil, **bytes → binary**, arrays, and maps. A decoded map becomes
an `Object` if every key is a string, otherwise a `Map`.

```ascript
import * as msgpack from "std/msgpack"
let bytes = msgpack.encode({ name: "Ada", nums: [1, 2, 3], ok: true })
let [val, err] = msgpack.decode(bytes)
// val.name == "Ada"; val.nums[1] == 2; err == nil
```

## std/cbor

CBOR (RFC 8949) binary serialization. Same `binary` feature, same API and value
mapping as `std/msgpack`:

- `cbor.encode(value) -> bytes`
- `cbor.decode(bytes) -> [value, err]`
- `cbor.decode(bytes, Class|schema) -> [value, err]`

```ascript
import * as cbor from "std/cbor"
class Point { x: number  y: number }
let [pt, err] = cbor.decode(cbor.encode({ x: 3, y: 4 }), Point)
// pt.x == 3; pt.y == 4; err == nil
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

Like `uuid.v4`, `uuid.v7` is **replay-safe inside a durable `workflow`**: under record/replay both halves are reproducible — the time prefix is drawn from the workflow's virtual clock and the random tail from its seeded PRNG. Outside a workflow it uses the real clock and real entropy (no change).

```ascript
let id = uuid.v7()
// id == "018f9b4e-3a7c-7c1d-9f2a-1b2c3d4e5f60"  (time-ordered, 36 chars)
```

## std/url

RFC-3986 URL parsing, building, and query-string helpers. Backed by the `url` crate (same engine used internally by the HTTP client). All functions are pure and synchronous.

> [!TIER1] `url.parse`, `url.build`, and `url.decode` return `[value, err]` pairs. `url.parseQuery`, `url.buildQuery`, and `url.encode` are infallible and return a value directly.

### url.parse

Parses a URL string into a component object.

- **s** `string` — the URL to parse.
- **Returns** `[obj, err]`. On success, `obj` has the fields below; absent components are `nil`.

| Field | Type | Notes |
| --- | --- | --- |
| `scheme` | `string` | `"https"`, `"http"`, … |
| `host` | `string \| nil` | host name or IP |
| `port` | `number \| nil` | explicit port; `nil` when the port matches the scheme default |
| `path` | `string` | always present; `"/"` for the root |
| `query` | `string \| nil` | raw query string (not decoded) |
| `fragment` | `string \| nil` | fragment identifier (without `#`) |
| `username` | `string \| nil` | |
| `password` | `string \| nil` | |

```ascript
import * as url from "std/url"
let [u, err] = url.parse("https://api.example.com:8080/v1?key=abc#top")
// u.scheme == "https", u.host == "api.example.com", u.port == 8080
// u.path == "/v1", u.query == "key=abc", u.fragment == "top"
```

### url.parseQuery

Parses an `application/x-www-form-urlencoded` query string into an object. Values are percent-decoded. When a key appears more than once, the last value wins.

- **s** `string` — the query string (without the leading `?`).
- **Returns** `object`.

```ascript
import * as url from "std/url"
let q = url.parseQuery("name=Ada+Lovelace&page=2")
// q.name == "Ada Lovelace", q.page == "2"
```

### url.buildQuery

Serializes an object into an `application/x-www-form-urlencoded` query string. Spaces are encoded as `+` (standard for form encoding, not `%20`). Keys are emitted in insertion order.

- **obj** `object` — keys and values (string, number, bool, or nil).
- **Returns** `string`.

```ascript
import * as url from "std/url"
let qs = url.buildQuery({ q: "hello world", page: "1" })
// qs == "q=hello+world&page=1"
```

#### Round-trip example

```ascript
import * as url from "std/url"
let params = { search: "a b", filter: "active" }
let qs = url.buildQuery(params)
let back = url.parseQuery(qs)
// back.search == "a b", back.filter == "active"
```

### url.build

Assembles a URL string from a component object (same shape as `url.parse` output).

- **obj** `object` — must contain at least `scheme`. All other fields are optional.
- **Returns** `[string, err]` — the assembled URL, or an error if the components are invalid.

```ascript
import * as url from "std/url"
let [u, err] = url.build({
  scheme: "https",
  host: "example.com",
  port: 9090,
  path: "/api",
  query: "v=2",
})
// u == "https://example.com:9090/api?v=2"
```

### url.encode

Percent-encodes a single URL component. All non-alphanumeric characters are escaped (same output as `encoding.urlEncode`).

- **s** `string` — the text to encode.
- **Returns** `string`.

```ascript
import * as url from "std/url"
let s = url.encode("a b&c")
// s == "a%20b%26c"
```

### url.decode

Percent-decodes a URL component.

- **s** `string` — the percent-encoded text.
- **Returns** `[string, err]` — the decoded string, or `[nil, err]` if the result is not valid UTF-8.

```ascript
import * as url from "std/url"
let [s, err] = url.decode("a%20b%26c")
// s == "a b&c", err == nil
```

## std/xml

Strict XML parsing and serialization backed by `quick-xml` (the `xml` Cargo feature, on by default). `std/xml` is deliberately **safe by construction**: the classic XML attacks are structurally impossible.

- **No entity expansion beyond the five predefined entities.** Custom DTD-internal entities (`<!ENTITY a "...">`), external `SYSTEM`/`PUBLIC` entities, and billion-laughs / quadratic-blowup payloads are **never expanded**. A reference to any non-predefined entity yields a `[nil, err]` `undefined entity '<name>'` result — the billion-laughs payload stops at its first custom-entity reference, and an external entity (`file:///…`, `http://…`) is never fetched.
- **No network or filesystem access.** `std/xml` requires **no capability** and imports nothing that can open a socket or a file. An XXE external-entity fetch is not merely denied — there is no code path that could perform it.
- **Depth and node budgets.** Nesting deeper than **256** elements or a document exceeding **1,000,000** nodes returns a `[nil, err]` result instead of risking a stack overflow or out-of-memory.

The parse shape is stable: every element is `{ tag, attrs, children }`, where `attrs` is an insertion-ordered object of attribute-name → string value and `children` is an array whose entries are child elements or text strings. CDATA folds into text; comments and processing instructions are dropped. Namespaced names (`ns:tag`, `xmlns:*`) pass through raw.

```ascript
import * as xml from "std/xml"
```

> [!TIER1] `xml.parse`, `xml.stringify`, and `xml.unescape` return a `[value, err]` pair. Malformed XML, an undefined entity, or a budget violation lands in the `err` channel.

### xml.parse

Parses XML text into a stable `{tag, attrs, children}` element tree.

- `text` (string) — the XML source text.
- Returns `[object, err]` — the root element object, or `nil` plus an error on malformed XML, an undefined (custom/external) entity reference, or a depth/node-budget violation.

```ascript
let [node, err] = xml.parse("<root id=\"1\"><b>hi</b>tail</root>")
// node == { tag: "root", attrs: { id: "1" }, children: [
//   { tag: "b", attrs: {}, children: ["hi"] },
//   "tail",
// ] }
// err == nil

// A custom DTD entity is NEVER expanded (XXE / billion-laughs defense):
let [_, e] = xml.parse("<!DOCTYPE x [<!ENTITY a \"BOOM\">]><x>&a;</x>")
// e.message == "undefined entity 'a'"
```

### xml.stringify

Serializes a `{tag, attrs, children}` element tree to XML text.

- `node` (object) — an element object `{ tag, attrs, children }`.
- `options` (object, optional) — `{ indent }`: a positive integer pretty-prints with that many spaces per level; omit for compact output.
- Returns `[string, err]` — the XML text, or `nil` plus an error if the node is not a well-formed element tree.

```ascript
let [text, err] = xml.stringify({ tag: "a", attrs: {}, children: ["hi"] })
// text == "<a>hi</a>", err == nil

let [pretty, _] = xml.stringify(node, { indent: 2 })
```

### xml.escape

Escapes the five predefined XML entities (`&`, `<`, `>`, `"`, `'`) in text.

- `text` (string) — the raw text to escape.
- Returns `string` — the escaped text.

```ascript
xml.escape("a & b < c") // == "a &amp; b &lt; c"
```

### xml.unescape

Unescapes the five predefined XML entities plus numeric character references (`&#NN;` / `&#xNN;`).

- `text` (string) — the escaped text.
- Returns `[string, err]` — the unescaped text, or `nil` plus an error if a named entity is undefined (only the five predefined entities and numeric refs are recognized — there is no entity table).

```ascript
let [s, err] = xml.unescape("a &amp; b &lt; c") // s == "a & b < c", err == nil
let [ab, _] = xml.unescape("&#65;&#x42;")        // ab == "AB"
```

## std/html

Lenient HTML helpers — escape / unescape and a **fail-closed** allowlist sanitizer (the `xml` Cargo feature, on by default; `std/html` is a sibling of `std/xml`). `std/html` is a pure string transform and requires **no capability**.

HTML is not XML, so the sanitizer cannot use a strict parser: a strict parser that *rejected* malformed markup would leave the raw bytes un-sanitized — a fail-OPEN hole and a direct XSS vector. Instead `std/html` is **emit-from-parse**:

- A lenient tokenizer that **never errors** turns the input into text / start-tag / end-tag / comment tokens. Anything it cannot parse (`<<script>`, an unterminated `<a href="x`) becomes literal text.
- A canonical serializer re-emits **only allowlisted tags**, with **only allowlisted attributes**, every text and attribute value re-escaped on emission. The raw input bytes of a tag are **never echoed**.
- A non-allowlisted tag is **escaped as text** (`<script>` → `&lt;script&gt;`) — visible but inert, never silently dropped.
- `href`/`src`/`action`/`cite`/… URL values are scheme-checked **after** entity-decoding, control/whitespace-stripping, and lowercasing, so `javascript:`, ` javascript:`, `java&#x09;script:`, `&#106;avascript:`, and `JaVaScRiPt:` are all neutralized.
- Comments, CDATA, processing instructions, doctypes, and the raw content of `<script>`/`<style>`/`<iframe>`/… are stripped.

**Default allowlist.** Tags: `p`, `br`, `b`, `strong`, `i`, `em`, `u`, `s`, `code`, `pre`, `blockquote`, `h1`–`h6`, `ul`, `ol`, `li`, `a`, `img`, `table`, `thead`, `tbody`, `tr`, `th`, `td`, `hr`, `span`. Attributes: `a` → `href`, `title`; `img` → `src`, `alt`, `title` (every other tag has none). URL schemes: `http`, `https`, `mailto` — plus relative URLs (no scheme).

```ascript
import * as html from "std/html"
```

### html.escape

Escapes the HTML special characters (`&`, `<`, `>`, `"`, `'`) for safe inclusion in element content or a double-quoted attribute value.

- `text` (string) — the raw text to escape.
- Returns `string` — the escaped text.

```ascript
html.escape("<b> & 'x'") // == "&lt;b&gt; &amp; &#39;x&#39;"
```

### html.unescape

Decodes HTML named entities (the HTML5 core set) plus numeric character references (`&#NN;` / `&#xNN;`). An unrecognized `&name;` is left verbatim (matching browser parse-error recovery).

- `text` (string) — the escaped text.
- Returns `string` — the decoded text.

```ascript
html.unescape("a &amp; b &copy; c") // == "a & b © c"
html.unescape("&#65;&#x42;")        // == "AB"
```

### html.sanitize

Fail-closed allowlist sanitizer (see the module overview). A lenient tokenizer feeds a canonical serializer that re-emits only allowlisted tags/attributes with all values re-escaped and URL schemes checked; everything unrecognized is escaped as inert text.

- `text` (string) — the untrusted HTML to sanitize.
- `options` (object) — *(optional)* `{ tags?: array<string>, attrs?: object<string, array<string>>, schemes?: array<string> }`. Each field, when present, **REPLACES** the corresponding default (the default schemes are `["http", "https", "mailto"]`).
- Returns `string` — sanitized HTML containing only allowlisted, canonically-serialized markup.

```ascript
html.sanitize("<script>alert(1)</script><b>hi</b>")
// == "&lt;script&gt;alert(1)&lt;/script&gt;<b>hi</b>"

html.sanitize(`<a href="javascript:alert(1)">x</a>`)
// == "<a>x</a>"   (the unsafe href is dropped)

html.sanitize("<mark>m</mark>", { tags: ["mark"] })
// == "<mark>m</mark>"
```

### Examples

- [`examples/xml_basics.as`](https://github.com/ascript-lang/ascript/blob/main/examples/xml_basics.as) — parse a document, read nodes + attributes, stringify (incl. pretty-print), and reject malformed XML.
- [`examples/advanced/feed_reader.as`](https://github.com/ascript-lang/ascript/blob/main/examples/advanced/feed_reader.as) — the canonical `xml` → `html.sanitize` pipeline: parse an RSS feed, then sanitize each item's untrusted HTML description.

## std/markdown

CommonMark → HTML over `pulldown-cmark`, **sanitized by default** — `markdown.render` pipes its HTML through the same fail-closed allowlist sanitizer as `html.sanitize`, so any embedded raw HTML, `<script>`, or `javascript:` URL comes out inert.

**Honest subset:** CommonMark plus GFM **tables**, **strikethrough**, and **task-lists** (on by default) and **footnotes** (off by default). No front-matter, no syntax highlighting (only a `class="language-x"` hint is emitted on fenced-code `<code>`), no MDX.

### markdown.render

Render CommonMark text to sanitized HTML.

- `text` (string) — the CommonMark source.
- `opts` (object) — *(optional)* `{ sanitize?: bool = true, gfmTables?: bool = true, strikethrough?: bool = true, taskLists?: bool = true, footnotes?: bool = false, allow?: object }`. `allow` is forwarded to the sanitizer (same `{ tags?, attrs?, schemes? }` shape as `html.sanitize`'s options) when sanitizing.
- Returns `string` — the rendered HTML.

> [!WARNING] `{ sanitize: false }` emits the **raw, un-sanitized** HTML — use it ONLY for input you fully trust. With untrusted markdown it is an XSS hole; the default (`sanitize: true`) is what defeats embedded scripts and `javascript:` links.

```ascript
markdown.render("# Hello\n\nA **bold** [link](https://example.test).")
// == "<h1>Hello</h1>\n<p>A <strong>bold</strong> <a href=\"https://example.test\">link</a>.</p>\n"

markdown.render("<script>alert(1)</script>")
// == "&lt;script&gt;alert(1)&lt;/script&gt;"   (script is inert)

markdown.render("```rust\nfn main() {}\n```")
// keeps the language hint: <pre><code class="language-rust">...
```

### markdown.escape

Backslash-escape CommonMark metacharacters so the string renders literally.

- `s` (string) — the text to escape.
- Returns `string` — `s` with every ASCII-punctuation character backslash-escaped.

```ascript
markdown.escape("a*b_c # heading")
// == "a\\*b\\_c \\# heading"   (every ASCII-punctuation char is backslash-escaped)
```

### Examples

- [`examples/markdown_render.as`](https://github.com/ascript-lang/ascript/blob/main/examples/markdown_render.as) — render a small document and demonstrate the sanitize-by-default XSS defense.

## std/decimal

Exact decimal arithmetic backed by a 96-bit scaled integer (`rust_decimal`). Use it wherever floating-point rounding is unacceptable: money, pricing, financial totals, or any domain where `0.1 + 0.2 == 0.3` must hold.

```ascript
import * as decimal from "std/decimal"
```

There is **no decimal literal** — construction is always explicit via `decimal.from` or `decimal.parse`. Once you have a decimal value, the standard arithmetic operators (`+`, `-`, `*`, `/`, `%`) and comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`) work directly. A `Number` on either side of such an operator is **automatically coerced** to decimal (non-finite numbers panic).

> [!NOTE] `decimal.from(number)` converts a floating-point number to the nearest decimal using the number's shortest round-trip string. `decimal.from("0.1")` is exact; `decimal.from(0.1)` is the decimal closest to the IEEE-754 value, which equals `decimal.from("0.1")` for most short decimal fractions.

> [!NOTE] JSON serialization (`std/json`) emits a decimal as a JSON number. The `serde_json` layer re-canonicalizes the value, so **trailing-zero scale is not preserved in JSON** — `decimal.from("1.50")` serializes to `1.5`. Use `decimal.toString(d)` to round-trip the exact scale as text.

### decimal.from

Construct a decimal from a string or number. Panics on invalid input.

- `x: string | number | decimal` — value to convert
  - `string`: parsed exactly (`"0.10"` has scale 2); invalid string → Tier-2 panic
  - `number`: integer or finite float; non-finite → Tier-2 panic
  - `decimal`: identity (returned as-is)
- Returns: `decimal`

> [!TIER2] Panics on invalid string, non-finite number, or wrong-type argument. Use `decimal.parse` for safe string conversion.

```ascript
decimal.from("0.1")     // decimal 0.1  — exact
decimal.from("1.50")    // decimal 1.50 — scale preserved
decimal.from(42)        // decimal 42

// Headline: exact arithmetic
decimal.from("0.1") + decimal.from("0.2") == decimal.from("0.3")   // true
```

### decimal.parse

Safely parse a string into a decimal. Returns a `[decimal, err]` pair — does not panic on invalid input.

- `s: string` — the string to parse
- Returns: `[decimal, nil]` on success, `[nil, err]` on failure

> [!TIER1] Returns `[nil, err]` instead of panicking.

```ascript
let [d, err] = decimal.parse("3.14")
// d == decimal 3.14, err == nil

let [bad, e] = decimal.parse("not-a-number")
// bad == nil, e.message describes the failure
```

### decimal.toString

Convert a decimal to its string representation, **preserving scale** (trailing zeros included).

- `d: decimal` — the decimal to format
- Returns: `string`

```ascript
decimal.toString(decimal.from("1.50"))   // "1.50"  — trailing zero kept
decimal.toString(decimal.from("42"))     // "42"
```

### decimal.toNumber

Convert a decimal to a floating-point `number`. This is a **lossy** conversion — the result is the nearest IEEE-754 double.

- `d: decimal` — the decimal to convert
- Returns: `number`

```ascript
decimal.toNumber(decimal.from("1.5"))   // 1.5
```

### decimal.round

Round a decimal to a given number of decimal places using **half-away-from-zero** (conventional school-math rounding: `1.5 → 2`, `2.5 → 3`, `−1.5 → −2`).

- `d: decimal` — the decimal to round
- `places: number` (optional, default `0`) — number of decimal places
- Returns: `decimal`

```ascript
decimal.round(decimal.from("1.5"))         // decimal 2   (half-away-from-zero)
decimal.round(decimal.from("-1.5"))        // decimal -2
decimal.round(decimal.from("1.456"), 2)    // decimal 1.46
```

### decimal.abs

Return the absolute value.

- `d: decimal` — the source decimal
- Returns: `decimal`

```ascript
decimal.abs(decimal.from("-3.7"))   // decimal 3.7
```

### decimal.floor

Return the largest integer decimal that is ≤ `d`.

- `d: decimal` — the source decimal
- Returns: `decimal`

```ascript
decimal.floor(decimal.from("1.9"))    // decimal 1
decimal.floor(decimal.from("-1.1"))   // decimal -2
```

### decimal.ceil

Return the smallest integer decimal that is ≥ `d`.

- `d: decimal` — the source decimal
- Returns: `decimal`

```ascript
decimal.ceil(decimal.from("1.1"))    // decimal 2
decimal.ceil(decimal.from("-1.9"))   // decimal -1
```

### decimal.trunc

Return the integer part of `d`, truncating toward zero.

- `d: decimal` — the source decimal
- Returns: `decimal`

```ascript
decimal.trunc(decimal.from("1.9"))    // decimal 1
decimal.trunc(decimal.from("-1.9"))   // decimal -1
```

### Operator overloading

Once you have decimal values, use normal operators:

```ascript
import * as decimal from "std/decimal"

let a = decimal.from("10.00")
let b = decimal.from("3.50")

a + b           // decimal 13.50
a - b           // decimal 6.50
a * b           // decimal 35.0000
a / decimal.from("2")   // decimal 5.00

// Number on either side is coerced automatically:
a * 2           // decimal 20.00

// Comparisons return bool:
a > b           // true
a == decimal.from("10.00")   // true

// Decimal / Number cross-type equality:
decimal.from("5") == 5    // true

// Exact: the headline property
decimal.from("0.1") + decimal.from("0.2") == decimal.from("0.3")   // true (not true with number!)
```
