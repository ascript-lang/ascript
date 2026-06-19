::: eyebrow Standard library

# Email (SMTP)

`std/email` builds and sends RFC 5322 email messages. The **message builder**
(`email.message` / `email.validateAddress`) is pure and side-effect-free — it needs no
capability and runs under `--sandbox`. The **SMTP client** (`email.send` / `email.connect`)
is a separate, `net`-gated layer (a hand-rolled SMTP state machine with STARTTLS) covered
in [SMTP client](#smtp-client) below.

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

## SMTP client

The SMTP client sends a built message over a real connection. It is `net`-gated (denied
under `--sandbox` / `--deny net`) and built on a small RFC 5321 state machine
(`EHLO → STARTTLS → AUTH → MAIL/RCPT/DATA → QUIT`) over the shared TLS plumbing — no
third-party SMTP library.

```ascript
import * as email from "std/email"

let [msg, _] = email.message({
  from: "alice@example.com", to: "bob@example.com",
  subject: "Hi", text: "Sent from AScript.",
})

let [res, err] = email.send(msg, {
  host: "smtp.example.com",
  username: "alice", password: env.get("SMTP_PASS")[0],
})
if err != nil {
  print("send failed:", err.message)
} else {
  print("accepted:", len(res.accepted), "rejected:", len(res.rejected))
}
```

### `email.send(msg, opts) -> [{accepted, rejected}, err]`

Connects, sends one message, and disconnects. The result's `accepted` is the array of
recipient addresses the server accepted; `rejected` is an array of `{address, code, message}`
for any recipient the server refused (a partial rejection populates both). `opts`:

| Field | Type | Notes |
|-------|------|-------|
| `host` | `string` | **required** — the SMTP server hostname. |
| `port` | `int` | optional — defaults by `tls`: **587** (starttls), **465** (implicit), **25** (none). |
| `tls` | `string` | `"starttls"` (default), `"implicit"`, or `"none"`. |
| `username` / `password` | `string` | optional — enables `AUTH`. |
| `authMethod` | `string` | `"plain"` (default) or `"login"`. |
| `allowInsecureAuth` | `bool` | opt-in to send credentials over a plaintext (`tls:"none"`) connection — **not recommended**. |
| `caCert` | `string` | optional extra trusted CA PEM (for a private CA / self-signed server). |
| `serverName` | `string` | optional TLS SNI / cert-verification name override (defaults to `host`). |
| `timeout` | `int` | optional per-command + connect budget in milliseconds (default 30000). |

### `email.connect(opts) -> [client, err]`

Opens a **reusable** connection (same `opts` as `send`). The returned `client` exposes:

- `client.send(msg) -> [{accepted, rejected}, err]` — send another message over the open connection.
- `client.close()` — send a best-effort `QUIT` and close the socket.

### Security model

The client is built around three defenses, all documented as part of its contract:

- **No silent STARTTLS downgrade.** When `tls:"starttls"` is requested, the server **must**
  advertise `STARTTLS` and complete the upgrade. If it does not advertise it, refuses the
  command, or the TLS handshake fails, `send`/`connect` returns a **Tier-1 error** and the
  client **never** sends `MAIL`/`AUTH` over the unencrypted socket. (Use `tls:"none"`
  explicitly if you really want plaintext.)
- **No plaintext credentials.** Passing `username`/`password` with `tls:"none"` is a
  **Tier-2 panic** raised *before* any byte is sent — unless you opt in with
  `allowInsecureAuth:true`.
- **Wire-layer injection re-check.** Even a hand-built message object that bypassed the
  builder's CRLF/NUL guard is **re-validated** (`from` and every recipient) at the wire layer
  before any `MAIL`/`RCPT` is written — a smuggled `\r\n` is a Tier-2 panic. The `DATA` body
  is dot-stuffed (a line starting with `.` is escaped) per RFC 5321.

## Examples

- [`examples/email_builder.as`](https://github.com/ascript-lang/ascript/blob/main/examples/email_builder.as) — the pure, deterministic message builder: plain-text + `multipart/alternative`, `bcc` envelope handling, `validateAddress`, and a header-injection rejection demo.
- [`examples/advanced/smtp_send.as`](https://github.com/ascript-lang/ascript/blob/main/examples/advanced/smtp_send.as) — `email.send` driven against an in-process SMTP sink (no external server), capturing the envelope + `DATA` payload.
