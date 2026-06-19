// Email message builder — the pure, deterministic builder (the SMTP client is separate).
//
// PURE / deterministic: the multipart boundary is derived from a content hash (not a
// random value) and no Date header is stamped, so `msg.raw()` is byte-stable across runs
// and identical on every engine. Header injection (CR/LF/NUL in an address or header) is a
// Tier-2 panic — caught here with `recover` to show it fails closed.
import * as email from "std/email"
import * as string from "std/string"

fn main() {
  // ── a plain-text message ──────────────────────────────────────────────────
  let [msg, err] = email.message({from: "alice@example.com", to: "bob@example.com", subject: "Hello from AScript", text: "This is a plain-text body."})
  print(`build error: ${err}`)
  let raw = msg.raw()
  print(`has From header: ${string.contains(raw, "From: alice@example.com")}`)
  print(`has Subject header: ${string.contains(raw, "Subject: Hello from AScript")}`)
  print(`is text/plain: ${string.contains(raw, "Content-Type: text/plain")}`)

  // ── text + html → multipart/alternative with a deterministic boundary ─────
  let [multi, _] = email.message({from: "alice@example.com", to: "bob@example.com", subject: "Rich", text: "plain part", html: "<p>html part</p>"})
  print(`is multipart/alternative: ${string.contains(multi.raw(), "multipart/alternative")}`)

  // ── bcc stays in the envelope, never a header ─────────────────────────────
  let [blind, _e] = email.message({from: "alice@example.com", to: "bob@example.com", bcc: "secret@example.com", subject: "Quiet", text: "body"})
  print(`bcc not in headers: ${!string.contains(blind.raw(), "secret@example.com")}`)

  // ── address validation ────────────────────────────────────────────────────
  print(`valid address: ${email.validateAddress("user+tag@example.com")}`)
  print(`invalid address: ${email.validateAddress("not-an-address")}`)

  // ── header injection fails closed (a recoverable Tier-2 panic) ────────────
  let inj = recover(() => email.message({from: "alice@example.com", to: "bob@example.com\r\nBcc: evil@example.com", subject: "x", text: "y"}))
  print(`injection rejected: ${inj[1] != nil}`)
}

main()
print("email_builder ok")
