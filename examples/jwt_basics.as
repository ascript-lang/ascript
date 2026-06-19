// JWT basics — sign, verify, decode, and the typed-key algorithm-confusion defense.
//
// PURE / deterministic: every token here is signed WITHOUT a clock-dependent claim
// (no `expiresIn`), so the output is stable across runs and identical on every engine.
// Authentication failures are Tier-1 `[value, err]` pairs — never panics.

import * as jwt from "std/jwt"

fn main() {
  let key = jwt.hmacKey("a-strong-shared-secret")

  // ── sign + verify roundtrip ──────────────────────────────────────────────
  let [token, signErr] = jwt.sign({ sub: "alice", role: "admin" }, key)
  print(`sign error: ${signErr}`)
  print(`token has three segments: ${len(string_split(token, ".")) == 3}`)

  let [claims, verifyErr] = jwt.verify(token, key, { algs: ["HS256"] })
  print(`verify error: ${verifyErr}`)
  print(`subject: ${claims.sub}`)
  print(`role: ${claims.role}`)

  // ── a tampered token fails verification (Tier-1, fails closed) ────────────
  let tampered = token + "x"
  let [_, tamperErr] = jwt.verify(tampered, key, { algs: ["HS256"] })
  print(`tampered rejected: ${tamperErr != nil}`)

  // ── the wrong key fails verification ──────────────────────────────────────
  let otherKey = jwt.hmacKey("a-different-secret")
  let [__, wrongKeyErr] = jwt.verify(token, otherKey, { algs: ["HS256"] })
  print(`wrong key rejected: ${wrongKeyErr != nil}`)

  // ── alg "none" can never be accepted, in any casing ───────────────────────
  // A signature-stripped token is rejected before any key dispatch.
  let [___, noneErr] = jwt.verify(token, key, { algs: ["none"] })
  print(`alg none rejected: ${noneErr != nil}`)

  // ── decode is UNVERIFIED — for routing/debugging only ─────────────────────
  let [decoded, decodeErr] = jwt.decode(token)
  print(`decode error: ${decodeErr}`)
  print(`decoded header alg: ${decoded.header.alg}`)
  print(`decoded claims sub: ${decoded.claims.sub}`)
  print(`decoded verified flag: ${decoded.verified}`)
}

// A tiny local string split (avoids importing std/string for one call).
fn string_split(s, sep) {
  let parts = []
  let cur = ""
  for (ch of s) {
    if (ch == sep) {
      parts = [...parts, cur]
      cur = ""
    } else {
      cur = cur + ch
    }
  }
  return [...parts, cur]
}

main()
print("jwt_basics ok")
