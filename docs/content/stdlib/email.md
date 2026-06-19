::: eyebrow Standard library

# Email (SMTP)

`std/email` builds RFC 5322 email messages. This page covers the **message builder** —
a pure, side-effect-free function that assembles the wire form of an email. The SMTP
**client** (`email.send` / `email.connect`) is a separate, network-gated layer documented
alongside it once shipped; the builder here needs no capability and runs under `--sandbox`.

The builder's headline feature is its **header-injection defense**. A `\r`, `\n`, or NUL
smuggled into an address or header can split the SMTP `DATA` stream and forge extra headers
or even extra SMTP commands (`\r\nBcc: evil@example.com`, `\r\nRCPT TO:<evil>`). Every
address and every header value is rejected for these control characters at build time — a
**Tier-2 panic**, because injecting a header is a programmer bug, not a recoverable result.

```ascript
import * as email from "std/email"

let [msg, err] = email.message({
  from: "alice@example.com",
  to: "bob@example.com",
  subject: "Hello from AScript",
  text: "Plain-text body.",
  html: "<p>HTML body.</p>",
})

if err != nil {
  print("build failed:", err.message)
} else {
  print(msg.raw())   // the RFC 5322 wire form, for tests / inspection / a custom transport
}
```

## `email.message(opts) -> [msg, err]`

`opts` is an object:

| Field | Type | Notes |
|-------|------|-------|
| `from` | `string` | **required** — the sender address (CRLF/NUL-rejected). |
| `to` | `string \| array<string>` | **required** — one or more recipient addresses. |
| `cc` | `string \| array<string>` | optional carbon-copy recipients (rendered as a `Cc:` header). |
| `bcc` | `string \| array<string>` | optional **blind** copies — kept in the **envelope only**, never rendered as a header (a `Bcc:` header would leak the list to every recipient). |
| `subject` | `string` | the subject (non-ASCII → an RFC 2047 `=?UTF-8?B?…?=` encoded-word). |
| `text` | `string` | the plain-text body. |
| `html` | `string` | the HTML body. |
| `attachments` | `array<{filename, content, contentType?}>` | `content` is a string or `bytes`; base64-encoded, wrapped at 76 columns. |
| `headers` | `object` | extra headers; both NAME and VALUE are CRLF/NUL-rejected, and a name may not contain `:` or whitespace. |

The returned `msg` is a message value; `msg.raw()` returns its RFC 5322 wire form as a string
(computed eagerly at build time, so it is byte-stable across calls).

### Message shapes

- **plain text** (`text` only) — a single `text/plain` part.
- **text + html** — `multipart/alternative` (the client picks the richest part it can render).
- **attachments present** — `multipart/mixed` (the body as the first part, each attachment a
  base64 part).

The multipart **boundary is deterministic** — derived from a SHA-256 over the parts — so
`msg.raw()` is reproducible for tests and golden files (no random boundary).

### Long headers fold safely

A header longer than the 78-column soft limit is **folded** per RFC 5322: a `CRLF` followed by a
single space (a whitespace continuation). The fold never emits a bare `CRLF` that could start a new
header, so even a 1000-character subject cannot be misparsed into an injected header.

## `email.validateAddress(addr) -> bool`

A pragmatic RFC 5321 `addr-spec` subset: exactly one `@`, a non-empty dot-atom local part, a
dotted domain with at least one `.`, no spaces or control characters, and a total length ≤ 254.
Quoted-local forms (`"a b"@host`) are a documented limitation and return `false`.

```ascript
email.validateAddress("user+tag@example.com")   // true
email.validateAddress("nope")                    // false (no @)
email.validateAddress("a@b.com\r\nBcc: x@y")     // false (control characters)
```

It composes with `std/schema` as a refiner — validation reuse runs from email into schema, not the
other way around:

```ascript
import * as schema from "std/schema"

let addr = schema.refine(schema.string(), (v) => email.validateAddress(v), "invalid address")
```
