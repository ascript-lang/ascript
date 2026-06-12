# Backend Batteries (BATT) — Design

- **Status:** Draft for review.
- **Date:** 2026-06-12
- **Code:** BATT (goal-perf.md "Flagship & ecosystem track")
- **Owner:** Mahmoud Kayyali
- **Depends on:** nothing in the PERF engine waves (pure stdlib + CLI). Uses shipped substrates:
  `std/crypto` (hmac/sha2), `compress` deps (tar/zip/flate2/zstd), `net` (reqwest-rustls pooled
  client, hyper server, tokio TCP), `src/det.rs` (clock/RNG seams), the FUZZ generator
  *philosophy* (not its internals), the caps chokepoint (`required_cap`), and `std/lru`'s
  cache pattern.
- **Depended on by:** REPLAY (the `--seed` failure-replay story composes, §10.6). RT rolls its
  own minimal deterministic inner-tar writer for `--oci` (owner decision — RT precedes BATT in
  the track; unifying RT onto §6.6's `tarcore` is a recorded optional later refactor, not an
  edge). TLS is available to future CNTR/RESIL extensions (neither depends on it v1).
- **Engines:** NO engine change. Every unit is native stdlib over `Interp::call_stdlib` (shared
  by tree-walker AND VM) plus CLI wiring. **No grammar change, no opcode, no `.aso` bump
  (`ASO_FORMAT_VERSION` unchanged vs the merge-base — DEFER bumps it to 28 before BATT
  executes, so the negative-space test reads the constant and asserts it unchanged by THIS
  spec, never a literal), no new `Value` variant.**
  Four-mode byte-identity is structural-by-construction and still proven per unit (corpus +
  goldens).
- **Breaking:** no. Strictly additive (new modules, new options on existing fns, new CLI flags).
  `std/compress`'s existing `zipCreate/zipExtract/tarCreate/tarExtract` are **unchanged**
  (§6.7).

---

## 0. Grounding — verified code facts this design is built on

Every claim below was re-verified against the tree on 2026-06-12. Implementers: **re-grep
every symbol before editing** — names are the anchors, line numbers drift.

| Fact | Where | Consequence |
|---|---|---|
| Stdlib registration = `exports()` + `call(...)` + two match arms in `mod.rs` + `STD_MODULES` + the caps **completeness test** (`every_std_module_is_classified_gated_or_explicitly_ungated`) | `src/stdlib/mod.rs` | Every new module follows the recipe in §2.1; an unclassified module trips the completeness test by design. |
| The capability gate is ONE chokepoint at `call_stdlib`, keyed by `required_cap(module, func)`; `os` is the per-func precedent; `Gate-12` short-circuit when `all_granted()` | `src/stdlib/mod.rs:325,475` | New gated modules add `required_cap` arms; per-func splits (jwt/email/archive) follow the `os` pattern; the completeness test needs a `PER_FUNC` allowance (§2.3). |
| `std/crypto` has: sha256/sha512/md5, **hmacSha256 only**, randomBytes (det-seam aware), argon2, bcrypt, crc32, xxhash. **No SHA-384 HMAC, no RSA, no EC.** | `src/stdlib/crypto.rs` | jwt's HS384/HS512 need `hmacSha384/hmacSha512` (sha2 crate already has `Sha384`); RS256 needs the `rsa` crate; ES256 needs `p256` — explicit unit tasks (§5.2). |
| `rustls`, `tokio-rustls`, `rustls-pemfile`, `rustls-pki-types`, `webpki-roots` are **already in `Cargo.lock`** (reqwest `rustls-tls`, tungstenite, genai). `rsa`, `p256`, `quick-xml`, `sha1` are present via `apple-codesign` (macOS target dep). `pulldown-cmark`, `lettre`, `chrono-tz` are NOT present. | `Cargo.lock`, `cargo tree -i` | The §3 dependency table's weight column is measured, not guessed. |
| `tar`/`zip`/`flate2`/`zstd`/`brotli` are already direct optional deps (feature `compress`); `compress.rs` ships in-memory `{name,data}` zip/tar create/extract | `Cargo.toml:94-98`, `src/stdlib/compress.rs` | `std/archive` adds **zero new dependencies**; the in-memory compress fns stay untouched (§6.7). |
| TCP streams/listeners are `ResourceState::TcpStream/TcpListener` behind `Value::Native(NativeKind::…)` with `governing_cap` re-checks; `TcpStreamState` wraps the tokio stream + buffered reader | `src/stdlib/net_tcp.rs` | `TlsStream` is a new sibling `ResourceState`/`NativeKind` with the same method surface and `governing_cap() == Net` (§4.3). |
| `server.bind/serve/listen` accept on a stored `TcpListener`; multi-isolate serve crosses a `std::net::TcpListener` + plain `Send` config into isolates; per-request limits parsed in `ServeOpts` | `src/stdlib/http_server.rs` | `tls` is a new `ServeOpts` member; PEM strings are `Send` so the multi-isolate path carries them unchanged (§4.2). |
| The pooled process-wide reqwest client is `net_http::shared_client()` | `src/stdlib/net_http.rs:381` | JWKS fetch, OAuth2 token calls, and the S3 client all reuse it — no second HTTP stack (the ai/telemetry ungated-egress lesson stays closed because all three route through gated modules). |
| `src/det.rs` (core, un-gated): `DeterminismContext::{record,replay}`, `VirtualClock`, `SeededRng::fill_bytes`; the `Interp.determinism` cell + `is_deterministic`/`clock_now_ms`/`fill_seeded_bytes` are core; **`restore_determinism`/`take_determinism` are `#[cfg(feature = "workflow")]`** | `src/det.rs`, `src/interp.rs:1363-1372` | The test runner's `--seed/--frozen-time` needs a CORE (un-gated) install seam — a real task (§10.2), not an assumption. |
| `ascript test` serial path: ONE tree-walker `Interp`, `load_module` per file, then `run_registered_tests_filtered` loops `self.tests` and `call_value`s each closure. Parallel = per-file isolates, results slotted by input index. Coverage = per-file VM. | `src/lib.rs:414+,525+`, `src/interp.rs:2724` | Per-test fresh `DeterminismContext` slots in at the top of the registration loop (one seam, all three paths reachable — §10.2/§10.3). |
| Snapshot mismatch text is built in `snapshot_impl` (`assert.snapshot '<name>' mismatch:` + an existing JSON **structural** diff + raw stored/new dump) | `src/stdlib/assert_mod.rs:156-216` | `std/diff`'s unified line diff wires in as the **text** diff for the dump section (§14.3). |
| The FUZZ generator (`src/fuzzgen/`) is `Unstructured`-driven, recursion-budgeted, scope-correct-by-construction, deterministic-by-construction, edge-biased, with shrink via proptest's `Vec<u8>` strategy | `src/fuzzgen/mod.rs` header | `test.prop` surfaces the same philosophy as a *user-facing* API (§10.4) — value-level generators + shrinking, NOT a re-export of the internal source fuzzer. |
| `std/date` is **offset-based** (`tzOffsetMinutes`); named zones are a documented deferral ("would need `chrono-tz`") | `src/stdlib/date.rs:4-6` | `std/cron` v1 is UTC + fixed-offset, honestly documented; the chrono-tz upgrade is a SHARED recorded-future with std/date (§11.3). |
| Long-running/network examples are excluded from the run-to-completion corpus via `EXAMPLE_SKIPS` + `SkipReason` (`LongRunningServer`, `Nondeterministic`) | `tests/vm_differential.rs:935-1043` | Examples policy §2.6: prefer self-contained loopback stubs (corpus members); a genuinely long-running server example uses the documented skip + a dedicated integration test (the `server_multicore.rs` precedent). |
| `schema` fluent refiners (`pattern`, `refine`, …) are tagged Objects (`__kind`) with a call-site hook — the "no new Value kind" posture | `src/stdlib/schema.rs` | `test.prop` generators are tagged Objects (`__gen`) in the same posture (§10.4); `std/email` ships a `schema`-compatible address refiner (§7.4). |
| `ASO_FORMAT_VERSION` is **27 at drafting** (DEFER bumps to 28 before BATT executes) | `src/vm/aso.rs:167` | The negative-space test asserts it unchanged vs the merge-base by reading the constant — never a literal pin. |
| Docs sidebar + cmd-K search derive from `NAV` in `docs/assets/app.js`; a page absent from `NAV` is unreachable | `docs/assets/app.js:11` | §2.5 lists the exact NAV additions; the DOCS spec's module→page claiming table + NAV⇄files bijection tripwires will enforce them (coordination note §2.5). |

## 1. Goal & shape

Make AScript a language you can build a real backend in **without leaving the stdlib**: serve
HTTPS, authenticate users (JWT/OAuth2/sessions), move archives and objects (tar/zip/S3), send
email, process markup (XML/HTML/Markdown), schedule jobs, and test it all deterministically with
property-based testing. This is **one multi-unit spec phased like the original batteries
campaign** (the 2026-05-31 phase-1 precedent: per-unit API tables, decisions log, error-tiering
conventions, test plan, docs list) — but executed as ONE spec with four independently-mergeable
phases (§15).

**Tier 1 (the backend story):** T1-1 TLS · T1-2 `std/jwt`+`std/oauth`+sessions · T1-3
`std/archive` · T1-4 `std/xml`+`std/html` · T1-5 `std/email` · T1-6 `std/blob` · T1-7
deterministic testing + `std/test` property testing.
**Tier 2 (the toolbelt):** T2-1 `std/cron` · T2-2 `std/semver` · T2-3 `std/markdown` · T2-4
`std/diff`.

Every unit ships the full battery: API surface (signatures), a default-on feature flag, a caps
mapping through `required_cap`, explicit error tiering, a docs page/section + `NAV`, intro +
advanced examples, happy+edge unit tests, and four-mode differential coverage. Security-sensitive
units additionally ship a named **security test battery** (§2.7).

## 2. Cross-cutting conventions (all units)

### 2.1 The module recipe (the `mod.rs` registration contract)

Adding `std/foo` means ALL of (the completeness test + checker make most omissions
build/test failures):

1. `src/stdlib/foo.rs` with `exports() -> Vec<(&'static str, Value)>` and a
   `call(func, args, span)` (pure) or `impl Interp` `call_foo` (async / stateful / callback-taking
   — the phase-1 precedent's split).
2. `pub mod foo;` gated by its feature in `src/stdlib/mod.rs`; arms in BOTH
   `std_module_exports` and `call_stdlib`'s `match module`.
3. `"std/foo"` in the feature-independent `STD_MODULES` (checker `unresolved-import` works in
   every build).
4. A caps verdict: a `required_cap` arm (whole-module or per-func), OR membership in
   `KNOWN_UNGATED`, OR the new `PER_FUNC` list (§2.3). Gated fns get `tests/cap_audit.rs` rows.
5. Curated `src/check/std_arity.rs` entries (min arity; `max=None` — native fns ignore surplus).
6. Docs section/page per §2.5 (+ `NAV` for new pages) and the README stdlib table row.
7. Examples + tests per §2.6, four-mode corpus membership (or a documented `EXAMPLE_SKIPS` row +
   dedicated integration test).

### 2.2 Error tiering (the house rule, applied uniformly)

- **Tier-2 panic** (recoverable via `recover`): argument-type misuse, malformed *programmer*
  inputs (an invalid cron expression literal, a non-PEM string where a key is required, invalid
  generator config), out-of-range counts (`want_count` discipline — every length/size drawn from
  a script number is validated before it drives an allocation), and capability denials
  (`capability '<cap>' denied`, raised by the gate).
- **Tier-1 `[value, err]` pair**: anything that fails because of the *world or untrusted data* —
  network I/O, TLS handshakes, signature verification, JWT validation, archive/XML/email/S3
  parse-or-protocol errors on untrusted bytes, SMTP rejections. **A failed `jwt.verify` is a
  Tier-1 err, never a panic** (auth failures are normal control flow).
- Untrusted-input hardening (Gate 14): every byte-level parser added here (PEM, JWT compact
  form, JWKS JSON, tar/zip headers, XML, MIME, S3 XML responses, cron text, semver text,
  markdown, diff inputs) must be alloc-bounded (`want_count`/`MAX_ALLOC_COUNT` discipline),
  panic-free on malformed input (fuzz-adjacent unit tests with truncated/hostile inputs), and
  must never silently truncate.

### 2.3 Capability mapping (and the completeness-test extension)

| Module | Verdict | Detail |
|---|---|---|
| TLS (options on `http_server`/`net_tcp`) | already `Net` | No new module; the existing whole-module `Net` arms cover `connectTls` and `serve({tls})` by construction. |
| `std/jwt` | **per-func** | `jwks`* (network fetch) → `Net`; `sign/verify/decode/…` are pure crypto → `None`. |
| `std/oauth` | `Net` (whole module) | Every entry point performs token/discovery HTTP. |
| `std/archive` | **per-func** | `extractTo`/`createFromDir` (disk) → `Fs`; all in-memory `Bytes` APIs → `None`. |
| `std/xml`, `std/html` | ungated | Pure. |
| `std/email` | **per-func** | `send`/`connect` (SMTP) → `Net`; `message`/`validateAddress` (pure builders) → `None`. |
| `std/blob` | `Net` (whole module) | Includes `presign` (pure computation) — **deliberate**: presigning mints a capability-bearing URL from secrets; gating the whole module keeps the secret-handling surface inside `Net` (fail-closed, simpler to audit). Recorded decision. |
| `std/test` | ungated | Pure + det seams. |
| `std/cron` | ungated | Pure computation + the (already ungated) timer machinery; a `cron.schedule` callback that touches the OS hits the normal gates itself. |
| `std/semver`, `std/markdown`, `std/diff` | ungated | Pure. |

**Completeness-test extension:** `every_std_module_is_classified_gated_or_explicitly_ungated`
currently special-cases only `os`. It gains a `PER_FUNC: &[&str] = &["os", "jwt", "archive",
"email"]` list, each entry **also** asserting at least one gated AND one ungated func (so a
per-func module can't silently degenerate to all-`None`). The existing `KNOWN_UNGATED` list
gains `xml/html/test/cron/semver/markdown/diff`; `oauth`/`blob` get `required_cap` whole-module
arms (feature-gated like the dispatch arms so `--no-default-features` builds).

### 2.4 Feature flags (all default-on; `--no-default-features` builds none of them)

| Feature | Modules | Depends on | New crates |
|---|---|---|---|
| `tls` | options on `http_server`/`net_tcp` | `net` | `tokio-rustls`, `rustls-pemfile`, `webpki-roots` (all already transitive — §3) |
| `auth` | `std/jwt`, `std/oauth`, http_server session helpers | `crypto`, `data`, `net` | `rsa`, `p256` |
| `archive` | `std/archive` | `compress`, `sys` (disk helpers `cfg(feature="sys")`-split inside) | none |
| `xml` | `std/xml`, `std/html` | `data` (escape tables shared w/ encoding) | `quick-xml` |
| `email` | `std/email` | `net`, `tls`, `data` | none (hand-rolled SMTP — §3) |
| `blob` | `std/blob` | `net`, `crypto`, `data`, `xml` (S3 list/error responses are XML) | none |
| *(core)* | `std/test`, the `test --seed/--frozen-time` flags | none (det.rs is core) | none |
| `cron` | `std/cron` | `datetime` | none |
| `semver` | `std/semver` | none | none |
| `markdown` | `std/markdown` | `xml` (sanitizer) | `pulldown-cmark` (default-features=false) |
| `diff` | `std/diff` | none | none |

`default` gains: `tls, auth, archive, xml, email, blob, cron, semver, markdown, diff`.
`std/test` is CORE (like `std/assert`) — property testing must work in a bare build.

### 2.5 Docs + NAV (DOCS-tripwire coordination)

New pages (each added to `NAV`'s "Standard library" section AND, when the DOCS spec's
module→page claiming table exists in-tree, claimed there — if DOCS merges first, BATT updates
the table in the same PR; if BATT merges first, the claims are recorded here for DOCS to seed):

- `stdlib/auth.md` — "Auth (JWT, OAuth2, sessions)": `std/jwt`, `std/oauth`, http_server
  cookie/session helpers.
- `stdlib/archive.md` — "Archives (tar & zip)": `std/archive` (cross-link from the compress
  section of `data.md`).
- `stdlib/markup.md` — "Markup (XML, HTML, Markdown)": `std/xml`, `std/html`, `std/markdown`.
- `stdlib/email.md` — "Email (SMTP)": `std/email`.
- `stdlib/blob.md` — "Object storage (S3-compatible)": `std/blob`.

Existing-page sections: TLS → `stdlib/net.md` (server + tcp sections); `std/cron` →
`stdlib/time.md`; `std/semver` + `std/diff` → `stdlib/utilities.md`; deterministic testing +
`std/test` property testing → `stdlib/assert.md` (NAV title becomes "Testing & assertions") +
the `--seed/--frozen-time/--update-snapshots` flags on `cli.md`. README stdlib table gains one
row per module.

### 2.6 Examples & tests policy

- **Self-contained loopback examples.** Network-facing units ship examples that spin their OWN
  in-process counterpart over `127.0.0.1` (the corpus stays run-to-completion + deterministic):
  TLS example = an HTTPS server with a baked self-signed test cert + a `connectTls`/`http.get`
  client against it; email example = an in-script `net/tcp` SMTP **sink** the client sends
  into; blob example = an in-script `http_server` S3 **stub** (sigv4-verified by the stub);
  oauth example = an in-script token endpoint. A genuinely long-running variant (the advanced
  TLS server) uses `EXAMPLE_SKIPS (LongRunningServer)` + a dedicated `tests/*.rs` integration
  test, the `server_multicore.rs` precedent.
- Property-testing examples pass an **explicit seed** so output is byte-stable across all four
  modes.
- Unit tests: happy + edge per fn (empty, nil, wrong-type → Tier-2; malformed/hostile bytes →
  Tier-1 or clean Tier-2 per §2.2; boundary counts at the `want_count` caps), in BOTH feature
  configs. Known-answer vectors wherever a spec provides them (RFC/NIST/AWS — §2.7).
- Four-mode: every example joins the conformance corpus; each unit adds at least one
  `vm_differential.rs` golden exercising its surface.

### 2.7 Security batteries (named, non-negotiable test suites)

| Battery | Unit | What it pins |
|---|---|---|
| `zip_slip_battery` | archive | extraction refuses `../` traversal, absolute paths, drive prefixes, symlink-escape (tar), hardlink-escape, case/`.`-normalization tricks — for BOTH tar and zip, BOTH `extractTo` and entry iteration helpers' `name` sanitization documentation. |
| `jwt_alg_confusion_battery` | jwt | `alg:"none"` (any casing) ALWAYS rejected; an RSA/EC public key can never HMAC-verify (typed keys, §5.3); `algs` allowlist intersection; tampered header/payload/signature each fail; embedded `jwk`/`jku` header fields are ignored (never trusted). |
| `sigv4_vector_battery` | blob | the AWS SigV4 test-suite vectors (canonical request, string-to-sign, signature) + a presigned-URL vector. |
| `sanitizer_xss_battery` | html | an OWASP-derived XSS corpus (script/`on*` handlers, `javascript:`/`data:` URLs, SVG/MathML vectors, mXSS-style nesting, entity-encoding bypasses, unclosed/malformed tags) — every vector must come out inert. |
| `smtp_injection_battery` | email | CRLF in addresses/headers is rejected at the builder (Tier-2) AND the wire layer (defense in depth); dot-stuffing; header-folding never splits an injected header (the http header-injection fix precedent). |
| `pem_hostile_battery` | tls/jwt | truncated/garbage/oversized PEM, wrong key type for cert, zero-cert chains — clean Tier-1/Tier-2, never a Rust panic. |

## 3. Dependency decision table (every candidate crate, recorded verdict)

House posture: pure-Rust, lean, already-in-graph preferred; hand-roll when the protocol is small
and well-specified; never hand-roll cryptographic primitives.

| Candidate | For | Transitive weight (verified) | Alternative | **Verdict** |
|---|---|---|---|---|
| `tokio-rustls` 0.26 + `rustls-pemfile` 2 + `webpki-roots` | T1-1 TLS | **Already in `Cargo.lock`** (reqwest `rustls-tls`, tungstenite, genai) — promoting to direct deps adds ~zero new build weight | `native-tls` (links OpenSSL — rejected); hand-rolled TLS (never) | **ADOPT** (direct, `tls` feature) |
| `rsa` 0.9 (RustCrypto) | RS256 | Pure Rust; already in the macOS graph via `apple-codesign`; new on Linux (~moderate, pulls `num-bigint-dig`) | `ring` (C/asm, heavier posture); skip RS256 (rejected — it's the dominant JWKS/OIDC alg) | **ADOPT** (`auth`). Note: the Marvin timing concern applies to RSA *decryption*; jwt uses only PKCS#1-v1.5 **sign/verify** — recorded. |
| `p256` 0.13 (RustCrypto) | ES256 | Pure Rust; already in the macOS graph | hand-roll (never — crypto) | **ADOPT** (`auth`) |
| `lettre` | T1-5 SMTP | Heavy: own transport/TLS/pool stacks, `Send`-runtime assumptions that fight the `!Send` isolate model | **hand-rolled minimal SMTP** over tokio TCP + tokio-rustls: EHLO/STARTTLS/AUTH PLAIN+LOGIN/MAIL/RCPT/DATA + dot-stuffing is a small, RFC-5321-specified protocol; we already own the TLS + base64 substrates | **REJECT lettre; hand-roll** (with the §2.7 injection battery) |
| `quick-xml` 0.39 | T1-4 XML | Pure Rust, battle-tested; already in the macOS graph via `plist` | hand-rolled XML parser (rejected — XML parsing is a known security tarpit: entities, DTDs, encodings) | **ADOPT** (`xml`), DTD/external-entity processing disabled (no XXE by construction) |
| `ammonia` + `html5ever` | html.sanitize | Large tree (markup5ever, tendril, string_cache …) | **hand-rolled fail-closed allowlist sanitizer**: tokenize leniently, re-EMIT only allowlisted tags/attrs from a canonical serializer (never echo raw input), scheme-check URLs — the ammonia *model* without the html5ever tree | **REJECT ammonia; hand-roll fail-closed** + the XSS battery (§2.7). Honest note: a hand-rolled sanitizer is credible ONLY because the design is emit-from-parse (anything unrecognized is escaped, not passed through). |
| `aws-sdk-s3` / `rusoto` | T1-6 | Enormous (smithy runtime — genai pulls some of it for bedrock, but only under `ai`) | **hand-rolled SigV4** (~200 lines over the existing `sha2`+`hmac`) + the pooled reqwest client | **REJECT SDK; hand-roll SigV4** with the AWS test-suite vectors (§2.7) |
| `cron` crate | T2-1 | Pulls `nom`; its DOM/DOW semantics are its own | hand-rolled 5-field parser + bounded `next()` scan (Vixie OR-rule documented) | **REJECT; hand-roll** |
| `chrono-tz` | cron named TZ | NOT in lock; ~large TZ data tables | offset-based v1 | **DEFER** — cron v1 is UTC + `{tzOffset}` minutes, matching `std/date`'s existing recorded deferral; the chrono-tz upgrade is one shared recorded-future for date+cron (§11.3) |
| `semver` crate (dtolnay) | T2-2 | Tiny, but implements *Cargo* range semantics, not node-semver ranges (`^ ~ || x-ranges` differ) | hand-roll parse/compare + the documented node-range subset | **REJECT; hand-roll** (the crate doesn't provide the required semantics anyway) |
| `pulldown-cmark` 0.13 | T2-3 | NOT in lock; small pure-Rust tree (`unicase`, `memchr`, `bitflags`) with `default-features = false` | subset-by-hand (rejected — CommonMark edge cases are a swamp; the crate is the reference-class implementation) | **ADOPT** (`markdown`, default-features=false) |
| `similar` / `dissimilar` | T2-4 | small, but Myers line-diff is ~150 deterministic lines we fully control (output format matters for snapshot wiring) | hand-rolled Myers O(ND) | **REJECT; hand-roll** |

## 4. T1-1 — TLS for `std/http/server` and `std/net/tcp` (`tls` feature)

### 4.1 Honest scope

TLS 1.2/1.3 with **rustls defaults** (safe cipher suites, no renegotiation) — no custom cipher
configuration in v1. Server: PEM cert chain + PKCS#8/PKCS#1/SEC1 private key, optional SNI
multi-cert map, ALPN fixed to `http/1.1` (the server is HTTP/1). Client: `webpki-roots` trust
anchors + optional extra `caCert` PEM (self-signed/test roots), SNI from the host, optional
ALPN list. **Recorded futures (documented, not silent):** client certificates (mTLS) both
directions; `insecureSkipVerify` (deliberately absent — a test root via `caCert` covers the
legitimate use); raw `tcp.listenTls` (server TLS lives in `http_server` v1); custom cipher
suites.

### 4.2 Server API

```text
server.serve({ tls: { cert: <PEM string>, key: <PEM string>,
                      sni?: { "<host>": {cert, key}, ... } }, ...existing opts })
s.listen(host, port, { tls: {...} })            // bind + serve sugar, unchanged otherwise
```

- **PEM strings only, never file paths** — a path option would read the filesystem from inside
  a `Net`-gated call, bypassing the `Fs` gate. The user reads cert files via `fs.read` (which
  is `Fs`-gated). This keeps the caps model honest; recorded decision.
- Implementation: `ServeOpts` gains `tls: Option<TlsServerCfg>`; a
  `rustls::ServerConfig` is built once per serve (cert chain via `rustls_pemfile::certs`, key
  via `private_key`; `alpn_protocols = ["http/1.1"]`; SNI map via
  `ResolvesServerCertUsingSni`), wrapped in `tokio_rustls::TlsAcceptor`. The accept loop
  wraps each accepted `TcpStream` in `acceptor.accept(stream).await` before
  `serve_connection`; a **handshake failure logs-and-continues** (a port-scanner must not kill
  the server) and counts against nothing.
- **Multi-isolate serve (`workers: N`) carries TLS:** the PEM strings are plain `Send`
  `String`s crossing in the existing boot closure; each isolate builds its own
  `TlsAcceptor`. Asserted by a multi-isolate TLS test.
- Errors: malformed PEM / no cert / key mismatch → Tier-1 `[nil, err]` from
  `serve` (config is data, often user-supplied at runtime); wrong option *types* → Tier-2.

### 4.3 Client API

```text
tcp.connectTls(host, port, opts?) -> [conn, err]
  opts: { serverName?: string,      // SNI/verification name, default `host`
          caCert?: string,          // extra PEM root(s) appended to webpki-roots
          alpn?: array<string> }    // e.g. ["h2","http/1.1"]; negotiated proto on conn.alpn()
conn.read(n) / conn.readLine() / conn.write(data) / conn.close() / conn.alpn()
```

- New `NativeKind::TlsStream` + `ResourceState::TlsStream(TlsStreamState)` mirroring
  `TcpStreamState`'s buffered method surface; `governing_cap() == Some(Net)` (the per-handle
  re-check after `caps.drop`). GC-untraced (native-resource rule); deterministic `Drop` closes.
- `https://` in `std/net/http` already works (reqwest-rustls) — documented, no change.

### 4.4 Tests/examples

Unit: handshake happy-path against an in-process rustls server task; bad cert/expired/wrong
hostname → Tier-1; `pem_hostile_battery` (§2.7); cap audit rows (`connectTls` denied under
`--deny net`; per-handle re-check). Examples: `examples/tls_echo.as` (loopback HTTPS roundtrip
with a baked test cert, corpus member); `examples/advanced/https_server.as`
(production-shaped; `EXAMPLE_SKIPS LongRunningServer` + `tests/tls_server.rs`).

## 5. T1-2 — `std/jwt` + `std/oauth` + sessions (`auth` feature)

### 5.1 `std/jwt` exports

```text
jwt.sign(claims: object, key, opts?) -> [token, err]
  opts: { alg?: string,             // default: derived from key kind (§5.3)
          headers?: object,         // extra protected headers (kid, ...)
          expiresIn?: number }      // seconds; sets exp from the (det-seam) clock
jwt.verify(token, key, opts?) -> [claims, err]
  opts: { algs?: array<string>,     // allowlist; INTERSECTED with the key kind's set
          iss?, aud?, leeway?: number /*s*/, clock?: number /*ms epoch override*/ }
jwt.decode(token) -> [ {header, claims, signature}, err ]   // UNVERIFIED — flagged (§5.4)
jwt.hmacKey(secret: string|bytes) -> key                    // HS256/384/512
jwt.rsaPublicKey(pem) / jwt.rsaPrivateKey(pem) -> [key, err]    // RS256
jwt.ecPublicKey(pem) / jwt.ecPrivateKey(pem) -> [key, err]      // ES256 (P-256, JOSE r||s)
jwt.jwks(url, opts?) -> [jwksHandle, err]     // Net-gated; fetch + cache (§5.5)
jwksHandle.verify(token, opts?) -> [claims, err]   // kid → cached key; refetch-on-miss
jwksHandle.close()
```

Algorithms v1: **HS256/HS384/HS512** (hmac + sha2 — `crypto.rs` gains `hmacSha384`/`hmacSha512`
exports as part of this unit, closing the substrate gap found in §0), **RS256** (`rsa`
PKCS#1-v1.5/SHA-256), **ES256** (`p256` ECDSA, **fixed 64-byte `r||s` JOSE encoding, not
DER** — a documented correctness trap with a wrong-encoding regression test). RS384/512,
ES384/512, PS*, EdDSA: recorded futures.

### 5.2 Crypto substrate tasks (explicit, per the brief)

`std/crypto` additions (same module, `crypto` feature): `hmacSha384`, `hmacSha512` (trivial
over `sha2::Sha384/Sha512`), and `timingSafeEqual(a, b) -> bool` (constant-time compare —
used by jwt HS verification and the cookie helpers; exposed because user code building auth
inevitably needs it).

### 5.3 Alg-confusion is killed structurally (typed keys)

Keys are tagged Objects (`{__jwtkey: "hmac"|"rsa-public"|…}`, schema posture — no new `Value`
kind; the PEM/secret material inside). `verify` computes
`allowed = algs_for_key_kind(key) ∩ (opts.algs or algs_for_key_kind(key))`:

- an HMAC key can only ever HS-verify; an RSA/EC **public key can never HMAC-verify** — the
  classic public-PEM-as-HMAC-secret confusion is unrepresentable;
- `alg: "none"` (any casing) is rejected unconditionally before key dispatch;
- `jwk`/`jku`/`x5u` header fields are never read (keys come only from the caller/JWKS handle).
Pinned by `jwt_alg_confusion_battery` (§2.7). RFC 7515/7519 + RFC 8037 test vectors where
applicable.

### 5.4 `jwt.decode` is loudly unverified

Returns the parsed header+claims **without any signature check** — for routing/debugging only.
Flagged: the docs carry a warning box, and the result object includes `verified: false` so the
value itself testifies.

### 5.5 JWKS fetch + cache

`jwt.jwks(url, {ttl?: seconds (default 300), maxKeys?: 16})` →
`ResourceState::JwksCache` native handle (`NativeKind::JwksCache`, `governing_cap == Net`):
fetches via `net_http::shared_client()`, parses RSA (`n`/`e`) and EC P-256 (`x`/`y`) JWKs,
caches by `kid` with TTL (the `std/lru` *pattern*, native-side). `verify` resolves `kid` from
the token header; an unknown `kid` triggers ONE refetch (thundering-herd-safe: a refetch
in-flight flag) then fails Tier-1. Take-out-across-await discipline for the handle state.

### 5.6 `std/oauth` exports (`Net`-gated, provider-agnostic)

```text
oauth.client({ clientId, clientSecret?, authUrl?, tokenUrl, redirectUri?, scopes? }) -> client
client.authUrl({ state, codeChallenge?, extra? }) -> [url, err]          // authorization-code
oauth.pkce() -> { verifier, challenge, method: "S256" }                  // pure helper
client.exchangeCode({ code, codeVerifier? }) -> [tokens, err]            // code → tokens
client.clientCredentials({ scopes? }) -> [tokens, err]
client.refresh(refreshToken) -> [tokens, err]
oauth.discover(issuerUrl) -> [ {authUrl, tokenUrl, jwksUri, issuer, ...}, err ]  // OIDC
```

`tokens = { accessToken, tokenType, expiresIn?, refreshToken?, idToken?, scope?, raw }`.
Token endpoint calls are `application/x-www-form-urlencoded` POSTs over the pooled client;
`client_secret_basic` and `client_secret_post` both supported (option). The OIDC happy path is
`discover` → `jwt.jwks(meta.jwksUri)` → `jwks.verify(idToken, {iss, aud})` — composition, not a
bundled "login framework" (recorded non-goal: full OIDC RP certification surface).

### 5.7 http_server signed-cookie/session helpers (same `auth` feature)

```text
server.signCookie(name, value, secret, opts?) -> string        // a Set-Cookie header value
  opts: { maxAge?, path? ("/"), httpOnly? (true), secure? (true), sameSite? ("Lax") }
server.verifyCookie(req, name, secret) -> [value, err]         // HMAC-SHA256, timing-safe
server.session(req, secret) -> [object, err]                   // decoded JSON session ({} when absent)
server.sessionCookie(sessionObj, secret, opts?) -> [string, err]  // JSON+HMAC Set-Cookie value
```

Format: `base64url(payload) + "." + base64url(hmacSha256(secret, payload))`. Pure helpers
(ungated — `http_server` module gating stays whole-module `Net`; these ride it, which is
fine: they're only useful inside a server). Cookie *names/values* are CRLF-rejected (Tier-2)
— the header-injection precedent.

### 5.8 Tests/examples

Unit: RFC 7515 HS256 vector, RS256/ES256 sign↔verify roundtrips + cross-checks against
openssl-generated fixtures, exp/nbf/aud/iss/leeway edges, det-mode `expiresIn` determinism,
the alg-confusion battery, JWKS cache TTL/refetch/unknown-kid, oauth token-endpoint happy/edge
against an in-process `http_server` stub, PKCE S256 vector, cookie roundtrip + tamper +
CRLF-rejection. Examples: `examples/jwt_basics.as` (corpus);
`examples/advanced/oauth_pkce_flow.as` (loopback token endpoint + JWKS, corpus);
`examples/advanced/sessions.as`.

## 6. T1-3 — `std/archive` (`archive` feature)

### 6.1 Shape

Two API planes: **pure in-memory** (`Bytes` in/out, ungated) and **disk helpers** (`Fs`-gated,
`cfg(feature="sys")` inside the module). Streaming via the existing generator machinery
(`Value::Generator` driven by a native iterator — the `stream` module precedent).

### 6.2 tar

```text
archive.tarWriter(opts?) -> writer            // in-memory; opts: {gzip?: bool, deterministic?: bool}
writer.add(name, data, opts?)                 // opts: {mode? (0o644), mtime? (epoch s), dir?: bool}
writer.finish() -> [bytes, err]               // one-shot; writer unusable after
archive.tarEntries(bytes|gzBytes) -> generator  // yields {name, size, mode, mtime, type, data}
archive.tarAppend(bytes, entries) -> [bytes, err] // append entries to an existing (non-gz) tar
archive.tarExtractTo(bytes, destDir, opts?) -> [array<string>, err]   // Fs; HARDENED (§6.5)
archive.tarCreateFromDir(dir, opts?) -> [bytes, err]                  // Fs; deterministic option
```

`deterministic: true` zeroes mtimes/uid/gid and sorts directory walks — the same
reproducible-output discipline RT's `--oci` applies in its own writer (§6.6). gzip handled inline (`{gzip:true}` writer / auto-sniffed `1f 8b`
magic on read) — the "gzip-tar convenience".

### 6.3 zip

```text
archive.zipWriter(opts?) -> writer            // deflate via the existing `zip` crate
writer.add(name, data, opts?)                 // opts: {compress? (true), mtime?}
writer.finish() -> [bytes, err]
archive.zipEntries(bytes) -> generator        // {name, size, compressedSize, mtime, data}
archive.zipExtractTo(bytes, destDir, opts?) -> [array<string>, err]   // Fs; HARDENED (§6.5)
```

Writers are native handles (`NativeKind::ArchiveWriter`, one kind, tar/zip discriminated in
`ResourceState::ArchiveWriter`) — ungated (in-memory), GC-untraced, `Drop`-cleaned.

### 6.4 Entry generators

`tarEntries`/`zipEntries` return a real `Value::Generator` (consumer-driven, one entry per
`next()`/`for…of` step) over a native cursor — entries are decoded lazily so a 1-GB archive
doesn't materialize at once; per-entry `data` reads are bounded by the declared size, which is
itself `want_count`-validated against `MAX_ALLOC_COUNT` (a hostile size field cannot drive an
alloc-abort).

### 6.5 Extraction hardening (`zip_slip_battery`)

`*ExtractTo` canonicalizes `destDir` first, then for EVERY entry: reject absolute paths,
reject any path whose normalized form escapes `destDir` (component-wise `..` resolution — no
symlink-following normalization of not-yet-existing paths), reject Windows drive/UNC prefixes
and NUL bytes, **skip symlink/hardlink tar entries by default** (`{links: "skip"}` default;
`"error"` opt) — a link target is never created pointing outside `destDir` under any option.
Every rejection is a Tier-1 err naming the offending entry (fail the whole extract — partial
extraction of a hostile archive is not a success).

### 6.6 RT `--oci` coordination (owner decision: RT rolls its own)

The tar-writing CORE is still a plain-Rust internal API (`pub(crate) mod tarcore` inside
`archive.rs`: `TarBuild::{add_entry, finish}` taking `&[u8]`, with the `deterministic`
knob), and the script-facing writer is a thin shell over it. RT's `--oci` does **NOT** call
it: RT precedes BATT in the track and deliberately ships its own bounded (~100-line,
fixed-timestamp) deterministic inner-tar writer (RT spec §8.1). Unifying RT onto `tarcore`
once both exist is recorded as an optional later refactor — not a dependency in either
direction.

### 6.7 `std/compress` is unchanged

The existing in-memory `zipCreate/zipExtract/tarCreate/tarExtract` keep their exact behavior
(four-mode byte-identity; programs depending on them never move). Docs cross-link both
directions ("for streaming/disk/hardened extraction, see std/archive").

### 6.8 Tests/examples

Unit: roundtrips (tar, tar.gz, zip; empty archive; empty file; unicode names; 0-byte +
boundary-size entries), append, deterministic-mode byte-stability (two runs → identical
bytes), truncated/hostile header fuzz-adjacent tests, the zip-slip battery, generator laziness
(a poisoned later entry only fails when reached). Examples: `examples/archive_roundtrip.as`
(corpus); `examples/advanced/backup_tool.as` (walk dir → deterministic tar.gz → verify —
`sys`-using, corpus if deterministic, else documented skip).

## 7. T1-4 — `std/xml` + `std/html` (`xml` feature)

### 7.1 Module placement decision (recorded)

Two modules, one feature. **`std/xml`** = strict XML 1.0 (quick-xml). **`std/html`** = the
lenient HTML helpers (`sanitize`, `escape`, `unescape`). Justification: HTML is not XML — the
sanitizer needs a lenient tokenizer that never rejects input, while the XML parser must be
strict and Tier-1-fail on malformed input; conflating them invites using the strict parser on
HTML (breaks) or the lenient one on XML (hides errors). `std/markdown` (T2-3) depends on
`std/html`'s sanitizer, and `import "std/html"` reads correctly there.

### 7.2 `std/xml`

```text
xml.parse(text) -> [doc, err]
xml.stringify(doc, opts?) -> string            // opts: {indent?: number, declaration?: bool}
xml.escape(s) -> string                        // & < > " '
```

**Document value shape (stable, documented):** every element is
`{ tag: string, attrs: object<string,string>, children: array<node> }` where a node is an
element object or a plain `string` (text; CDATA folded to text, comments/PIs dropped —
documented). `doc` = the root element object. **Namespaces, minimally-honestly:** prefixes are
NOT resolved — `tag` keeps the raw `ns:name`, `attrs` keeps `xmlns`/`xmlns:*` entries verbatim;
a `xml.localName(tag)`/`xml.prefix(tag)` helper pair covers the common need. Full
namespace-URI resolution is a recorded future, not a half-feature. Security: DTD content is
skipped, external entities never fetched, entity expansion limited to the five built-ins +
numeric refs (no billion-laughs by construction); depth + total-node budgets (Tier-1 err on
breach).

### 7.3 `std/html`

```text
html.escape(s) / html.unescape(s) -> string    // named + numeric entities (HTML5 core set)
html.sanitize(s, opts?) -> string              // allowlist-based, fail-closed (§3 verdict)
  opts: { tags?: array<string>, attrs?: object<string, array<string>>,
          schemes?: array<string> /* default ["http","https","mailto"] */ }
```

Default allowlist: the usual formatting/structure set (`p, br, b, strong, i, em, u, s, code,
pre, blockquote, h1–h6, ul, ol, li, a[href,title], img[src,alt,title], table/thead/tbody/tr/
th/td, hr, span`). Engine: lenient tokenizer → only allowlisted tags re-emitted by the
canonical serializer with only allowlisted attributes; ALL text and attribute values
re-escaped on emission; `href`/`src` parsed and scheme-checked (relative allowed,
`javascript:`/`data:`/unknown schemes dropped); everything unrecognized is escaped as text
(never echoed). Comments, CDATA, PIs, doctypes stripped. Pinned by `sanitizer_xss_battery`.

### 7.4 Schema/email tie-in

`std/email` (§8) exposes `email.validateAddress`; `std/schema` is NOT modified (the refiner
reuse runs the other way: an email example shows `schema.refine(s, (v) =>
email.validateAddress(v), "invalid address")` — composition over new surface).

### 7.5 Tests/examples

Unit: parse/stringify roundtrips (attrs, nesting, CDATA, entities, unicode), malformed XML →
Tier-1, entity-bomb + depth-budget breach → clean Tier-1, namespace passthrough pins, escape
tables, the XSS battery, allowlist-options behavior. Examples: `examples/xml_basics.as`
(corpus); `examples/advanced/feed_reader.as` (parse a baked RSS/Atom string → typed objects →
sanitize summaries; corpus).

## 8. T1-5 — `std/email` (`email` feature; hand-rolled SMTP per §3)

### 8.1 Message builder (pure, ungated)

```text
email.message({ from, to, cc?, bcc?, replyTo?, subject, text?, html?,
                attachments?: array<{filename, content: bytes|string, contentType?}>,
                headers?: object }) -> [msg, err]
msg.raw() -> string            // the RFC 5322 wire form (for tests/inspection)
email.validateAddress(s) -> bool   // pragmatic RFC 5321 addr-spec subset (documented)
```

Builder produces `multipart/alternative` for text+html, `multipart/mixed` with base64 parts
for attachments (encoding substrate reuse), RFC 2047 encoded-words for non-ASCII headers,
dot-stuffing applied at send time. **Every address and every header value is CRLF/NUL-rejected
(Tier-2) at build time** — the injection battery's first line of defense; the wire writer
re-checks (defense in depth).

### 8.2 SMTP client (`Net`-gated)

```text
email.send(msg, { host, port?, tls?: "starttls"|"implicit"|"none" (default "starttls"),
                  username?, password?, caCert?, timeout?: ms }) -> [ {accepted, rejected}, err ]
email.connect({ ... same ... }) -> [client, err]      // reusable connection
client.send(msg) -> [ {accepted, rejected}, err ]
client.close()
```

Protocol: EHLO → (STARTTLS via the T1-1 client connector, then EHLO again) → AUTH PLAIN or
LOGIN (only over TLS unless `tls:"none"` AND the user passes `allowInsecureAuth: true` —
loud, documented) → MAIL FROM/RCPT TO (per-recipient accept/reject collected) → DATA with
dot-stuffing → QUIT. Implicit TLS = connect through the TLS connector first (port 465
convention). `ResourceState::SmtpClient` native handle, `governing_cap == Net`,
take-out-across-await, `Drop` closes. All server interactions Tier-1.

### 8.3 Tests/examples

Unit: builder wire-form pins (alternative/mixed/encoded-words/base64 wrapping at 76 cols),
address validation table, the SMTP injection battery, protocol happy/edge against an
in-process scripted SMTP stub (multi-line replies, AUTH failure, RCPT partial rejection,
STARTTLS refusal), timeout behavior. Examples: `examples/email_builder.as` (pure builder,
corpus); `examples/advanced/smtp_send.as` (in-script `net/tcp` SMTP sink + send through it,
corpus).

## 9. T1-6 — `std/blob` (S3-compatible; `blob` feature; `Net`-gated)

### 9.1 Client + config

```text
blob.client({ endpoint,            // e.g. "https://s3.us-east-1.amazonaws.com" or MinIO/R2 URL
              region,              // sigv4 scope; "auto" accepted (R2)
              accessKey, secretKey, sessionToken?,
              bucket?,             // default bucket for the key-only call forms
              pathStyle?: bool })  // default: true for non-AWS endpoints (MinIO), virtual-host for AWS
        -> client
```

`ResourceState::BlobClient` (config + nothing else; HTTP via the pooled `shared_client()`),
`governing_cap == Net`.

### 9.2 Operations (all Tier-1)

```text
client.put(key, data: bytes|string, opts?: {contentType?, metadata?, bucket?}) -> [etag, err]
client.get(key, opts?) -> [bytes, err]               // opts: {bucket?, range?: [start, end]}
client.head(key, opts?) -> [ {size, etag, contentType, lastModified, metadata}, err ]
client.delete(key, opts?) -> [nil, err]
client.list(opts?) -> generator                      // {prefix?, delimiter?, bucket?, pageSize?}
                                                     // yields {key, size, etag, lastModified};
                                                     // continuation-token pagination driven lazily
client.presign(method, key, opts?) -> [url, err]     // {expires?: s (default 900), bucket?, contentType?}
client.putMultipart(key, source, opts?) -> [etag, err]
   // source: a generator|array of bytes chunks; create→UploadPart(stream)→complete,
   // abort-on-error (no orphaned uploads); partSize floor 5 MiB enforced
```

### 9.3 SigV4 (the hard part, implemented precisely)

Hand-rolled per the AWS spec over `sha2`+`hmac`: canonical request (method, canonical URI with
**single**-encoding for the path, canonical query with sorted+encoded params, canonical
headers incl. `host`/`x-amz-date`/`x-amz-content-sha256`, signed-headers list, payload hash) →
string-to-sign (`AWS4-HMAC-SHA256`, scope `date/region/s3/aws4_request`) → derived signing key
(four HMAC chain) → `Authorization` header. Presigned URLs use the query-param variant with
`UNSIGNED-PAYLOAD`. **Pinned by `sigv4_vector_battery`** (AWS sig-v4 test-suite vectors as
unit tests of the canonicalization + signature stages independently). Time comes from the det
seam (`clock_now_ms`) — presign/sign are replay-deterministic under `--seed/--frozen-time` by
construction. S3 XML error bodies parsed via `std/xml`'s Rust core into
`err = {message, code, status}`.

### 9.4 Tests/examples

Unit: the vector battery; URL construction matrix (path-style × virtual-host × R2 "auto");
list pagination over a stub; multipart abort-on-error; range get; metadata roundtrip;
hostile/malformed XML responses. Examples: `examples/blob_basics.as` (in-script `http_server`
S3 stub that VERIFIES the sigv4 signature server-side — the stub is itself the best test;
corpus); `examples/advanced/blob_sync.as` (dir → multipart upload pipeline over the stub).

## 10. T1-7 — Deterministic testing + property testing (CORE)

### 10.1 CLI surface

```text
ascript test --seed <u64> --frozen-time <iso8601|epoch-ms> [existing flags]
```

Both optional, independently usable; `--frozen-time` alone implies a fixed clock with a
default seed of 0; `--seed` alone freezes time at the deterministic epoch
(`det::deterministic_start_ms(seed)` — the existing convention). When neither is given,
nothing changes (byte-identical default — the INERT discipline).

### 10.2 Wiring (the verified seam)

- `Interp::set_determinism(Option<DeterminismContext>)` becomes CORE (the `workflow` cfg on
  `restore_determinism`/`take_determinism` is lifted to a core seam with the workflow-specific
  wrappers kept where they are) — verified-needed in §0.
- `run_tests_with_options` gains `det: Option<DetTestConfig{seed, start_ms}>`;
  `run_registered_tests_filtered` installs a **fresh** `DeterminismContext::record(seed,
  start_ms)` at the top of EACH test iteration and clears it after. Fresh-per-test (same seed)
  makes every test's RNG/clock stream independent of execution order and of `--filter` — which
  preserves the §7 parallel-determinism contract: the parallel path ships `(seed, start_ms)`
  across the airlock (two plain `Send` scalars) and each isolate applies the same per-test
  rule; the coverage path passes it to the per-file VM run. Module top-level load runs WITHOUT
  the context (only test bodies are deterministic — documented; freezing load-time would change
  `let now = time.now()` module constants in surprising ways).
- Under the context, the ALREADY-SHIPPED seams do the work: `time.now/monotonic/sleep`
  (virtual clock, no real sleeping), `date.now`, `math.random*`, `uuid.v4`,
  `crypto.randomBytes`/salts, FFI replay refusals — all inherited, nothing re-implemented.

### 10.3 Failure reporting + replay

Every failure line in the summary gains the suffix `(seed: N, frozen-time: T)` when a det
context was active, and `test.prop` failures print the replay invocation verbatim
(`ascript test file.as --seed N --filter "<name>"`). REPLAY-synergy note: `--record` trace
capture is REPLAY's scope; the seed line is designed to be the same vocabulary. Coordination:
REPLAY's `test --record` installs per-test contexts in the SAME `run_registered_tests_filtered`
loop §10.2 wires — whichever spec lands second rebases onto the first's per-test install seam
(REPLAY carries the mirror note).

### 10.4 `std/test` — user-facing property testing (CORE module)

```text
import { prop, gen } from "std/test"

prop(name, generators: array<gen>|object<string,gen>, fn, opts?)
  // registers a test (the same self.tests table — filter/--parallel/coverage all work).
  // opts: { runs?: 100, seed?: u64, maxShrinks?: 500 }
gen.int(min?, max?)          gen.float(min?, max?)        gen.bool()
gen.string(opts?)            // {minLen?, maxLen?, charset?: "ascii"|"unicode"|string}
gen.arrayOf(g, opts?)        // {minLen? 0, maxLen? 32}
gen.objectWith({k: g, ...})  // fixed shape, per-field generators
gen.oneOf(...gens|values)    gen.constant(v)              gen.nilOr(g)
gen.map(g, fn)               gen.filter(g, fn, opts?)     // {maxDiscard? 100}
```

Generators are **tagged Objects** (`{__gen: "int", min, max}` — the schema posture, §0; no new
`Value` kind; `gen.map/filter` store the user fn like `schema.refine` does). The runner is
native (`impl Interp`): per iteration, draw values from a `det::SeededRng` stream derived from
`(seed, iteration)`, call `fn(...values)` — **falsy return or Tier-2 panic = failure**
(`Propagate` = failure with the err) — matching `assert` semantics so `assert.eq` works
inside the body unchanged.

Seed precedence: `opts.seed` > CLI `--seed` > a fresh random seed (printed on failure either
way). This is the FUZZ generator philosophy surfaced — budgeted, deterministic, edge-biased
(int/float generators bias toward `0, ±1, min, max, ±2^53, i64 bounds`; string toward empty/
unicode-boundary; arrays toward empty/single) — NOT the internal source-program fuzzer, which
stays an internal asset.

### 10.5 Shrinking (documented strategy: halve + trim, greedy, bounded)

On failure, greedily try simpler candidates, re-running `fn` (re-seeded deterministically per
candidate), keep any that still fails, repeat to a fixpoint or `maxShrinks`:

- int/float: toward 0 by halving the distance (then sign-flip toward positive);
- string/array: drop half the tail, then halves of interior runs, then shrink elements
  pointwise;
- objectWith: shrink field values pointwise (shape is fixed by construction);
- oneOf: earlier alternatives; nilOr: try `nil`… then shrink the inner;
- map/filter: shrink the SOURCE, re-apply the mapping (filter discards respect `maxDiscard`).

The report prints the **shrunken counterexample** (the `print`-form of each argument), the
original failing iteration, the shrink count, and the seed line (§10.3).

### 10.6 Tests/examples

Unit: per-generator distribution/boundary pins (seeded → exact-value assertions), shrink
convergence pins (a known property like `x < 100` must shrink to exactly `100`), falsy/panic/
propagate failure classes, runs/maxShrinks/maxDiscard budgets, filter interplay, parallel
determinism (same summary `--parallel` on/off), frozen-time + seeded `time.now`/`math.random`
in plain tests, default-mode byte-identity (no flags → output identical to pre-BATT). CLI
tests for the new flags (incl. rejection of malformed `--frozen-time`). Examples:
`examples/property_testing.as` (explicit seed, corpus);
`examples/advanced/prop_roundtrips.as` (encode/decode roundtrip laws over json/encoding,
explicit seed, corpus).

## 11. T2-1 — `std/cron` (`cron` feature)

### 11.1 API

```text
cron.parse(expr) -> [schedule, err]      // schedule is a tagged Object (reusable, inspectable)
cron.next(expr|schedule, opts?) -> [epochMs, err]     // opts: {after?: epochMs (default: now via det seam),
                                                      //        tzOffset?: minutes (default 0 = UTC)}
cron.nextN(expr|schedule, n, opts?) -> [array<epochMs>, err]
cron.matches(expr|schedule, epochMs, opts?) -> [bool, err]
cron.schedule(expr, fn, opts?) -> [handle, err]       // {tzOffset?, immediate?: false}
handle.start() / handle.stop() / handle.running() / handle.close()
```

### 11.2 Semantics (documented precisely)

5-field Vixie cron (`min hour dom month dow`), `*`, lists, ranges, `*/step` + `a-b/step`,
names (`jan-dec`, `sun-sat`, case-insensitive), `dow` 0 and 7 = Sunday; `@yearly @annually
@monthly @weekly @daily @hourly` shortcuts (`@reboot` rejected — meaningless here, Tier-2).
**DOM/DOW rule:** when BOTH are restricted, a time matches if EITHER matches (the Vixie OR
rule) — documented with examples. `next` scans minute-by-minute with month/day skip
acceleration, bounded to 5 years (no match → Tier-1 err — catches impossible dates like
`0 0 30 2 *`). Malformed expression → Tier-1 from `parse` (it's frequently user/config data);
wrong arg types → Tier-2.

### 11.3 TZ honesty + det interplay

UTC by default; `tzOffset` minutes shifts the civil-time interpretation — **fixed offset, no
DST transitions**, the exact posture `std/date` already documents; the chrono-tz named-zone
upgrade is one shared recorded-future for `std/date` + `std/cron` (§3). **Det interplay
(documented):** `schedule` computes delay from `clock_now_ms` and sleeps via the `time.sleep`
seam — under `--seed/--frozen-time`/workflow the virtual clock advances instantly and fire
times are replay-deterministic *by construction* (no new seam). The handle's loop is a
spawned task (cancel-on-drop discipline; `stop` is graceful, `close` aborts).

### 11.4 Tests/examples

Unit: parse matrix (every field form + errors), next() known-vector table (incl. month/year
rollover, leap day, the DOM/DOW OR rule, `*/step` offsets, tzOffset), 5-year-bound err,
schedule start/stop/fire-count under virtual time. Examples: `examples/cron_next.as`
(pure computation, corpus); `examples/advanced/job_scheduler.as` (schedule + virtual-time
test pattern).

## 12. T2-2 — `std/semver` (`semver` feature)

```text
semver.parse(v) -> [ {major, minor, patch, prerelease: array, build: array}, err ]
semver.valid(v) -> bool
semver.compare(a, b) -> int        // -1|0|1; SemVer 2.0.0 precedence incl. prerelease rules
semver.satisfies(v, range) -> [bool, err]
semver.maxSatisfying(versions, range) -> [string|nil, err]
```

**Range subset (documented precisely, node-semver-compatible):** exact; comparators
`= > >= < <=`; `^` (caret, incl. the `^0.x`/`^0.0.x` special cases); `~` (tilde); x-ranges
(`1.2.x`, `1.x`, `*`, partial `1.2`); hyphen ranges (`1.2.3 - 2.0.0` with partial-end
semantics); space = AND within a comparator set; `||` = OR of sets. **Prerelease rule:** a
prerelease version satisfies a range only if some comparator in the matching set has the same
`[major,minor,patch]` tuple AND a prerelease (node's default, `includePrerelease:false`; the
option is a recorded future). Not included (documented): `workspace:`/`npm:` protocols, loose
mode. `compare` Tier-2s on a malformed version (programmer data); `satisfies` returns Tier-1
on a malformed RANGE (often external data). Tests: the SemVer 2.0.0 precedence ladder
verbatim + a node-semver-derived satisfies table. Example: `examples/semver_ranges.as`
(corpus).

## 13. T2-3 — `std/markdown` (`markdown` feature)

```text
markdown.render(text, opts?) -> string
  opts: { sanitize?: true,         // output passes html.sanitize (DEFAULT ON)
          gfmTables?: true, strikethrough?: true, taskLists?: true, footnotes?: false,
          allow?: {...} }          // forwarded to html.sanitize when sanitizing
markdown.escape(s) -> string       // escape markdown metacharacters
```

pulldown-cmark (CommonMark) with the listed extensions toggled; **sanitized by default** —
`render` pipes through `html.sanitize` so embedded raw HTML/script comes out inert;
`{sanitize:false}` is the documented escape hatch for trusted input (the docs carry the XSS
warning). Honest subset note in docs: CommonMark + tables/strikethrough/task-lists; no
front-matter, no syntax highlighting (a `code` class is emitted for fencing info), no MDX.
Pure; ungated. Tests: CommonMark spot vectors, extension toggles, the sanitize-by-default
pin (`<script>` in input → escaped in output), option matrix. Example:
`examples/markdown_render.as` (corpus); `examples/advanced/docs_site_gen.as` (markdown +
archive + fs mini static-site generator).

## 14. T2-4 — `std/diff` (`diff` feature) + snapshot wiring

### 14.1 API

```text
diff.lines(a, b) -> array<{tag: "equal"|"delete"|"insert", aStart, aEnd, bStart, bEnd, lines}>
diff.unified(a, b, opts?) -> string    // opts: {context? 3, fromFile? "a", toFile? "b"}
diff.chars(a, b) -> array<...>         // same hunk shape over chars (small-input intra-line)
```

Hand-rolled Myers O(ND) over lines (split preserving a trailing-newline flag so the
`\ No newline at end of file` marker is correct); deterministic output; input size budgeted
(`want_count` discipline on total line counts; beyond budget → Tier-1 "inputs too large").

### 14.2 Pure module conventions

Tier-2 on non-string args; everything else total. Tests: known unified-format vectors
(empty↔empty, empty↔content, identical, interleaved hunks with context merging, no-trailing-
newline marker), determinism pin, budget breach. Example: `examples/diff_unified.as` (corpus).

### 14.3 Snapshot-failure DX wiring (the verified location)

`assert_mod::snapshot_impl` (§0) currently prints the JSON structural diff + a full raw
stored/new dump. Under `feature = "diff"` the raw dump section is replaced by
`diff.unified(stored, new, {fromFile:"stored", toFile:"new"})` (truncated past 200 lines with
a "… N more lines" tail); the structural JSON diff stays first (it's better for shape
changes); without the feature the old dump remains (no silent behavioral dependency of a core
path on an optional feature). This changes test-failure *message text* only — the in-tree
tests pinning the old message are updated in the same task (Gate: behavior identical, message
improved, both feature configs green).

## 15. Phasing & merge strategy (each phase merges independently green)

| Phase | Branch | Units | Rationale |
|---|---|---|---|
| **A — auth story** | `feat/batt-auth` | T1-1 TLS, T1-2 jwt/oauth/sessions (+ crypto substrate additions) | TLS first (email later depends on it); jwt/oauth complete the "serve HTTPS + authenticate" headline. |
| **B — data & integration** | `feat/batt-data` | T1-3 archive, T1-4 xml/html, T1-5 email, T1-6 blob | Internal order: archive → xml/html → email (needs A's TLS, B branches AFTER A merges) → blob (needs xml for S3 errors). |
| **C — testing batteries** | `feat/batt-testing` | T1-7 det testing + `std/test` | Independent of A/B (could run in parallel with B). |
| **D — toolbelt** | `feat/batt-t2` | T2-1..T2-4 | markdown needs B's sanitizer; diff's snapshot wiring is standalone. |

Per the campaign cadence: every unit is implemented by a fresh subagent, independently
reviewed (commands run, edges probed), each phase gets a holistic review before merge
`--no-ff`; CLAUDE.md/roadmap/goal-perf status updates ride each phase's final task.

## 16. Gates & acceptance

- **goal.md Gates 1–14 verbatim** per phase: four-mode byte-identity (corpus + goldens, both
  feature configs); clippy clean both configs; tests green both configs; no borrow across
  await (the SMTP/TLS/blob handles use take-out-across-await); Gate 5 zero `type-*` corpus
  false positives; no placeholders (every deferral in §17 is recorded); corpus migrated never
  deleted; fuzz/CI green; examples happy+edge; unit tests happy+edge; tooling parity (no
  grammar change → fmt/tree-sitter untouched; LSP completion picks up new modules via the
  existing exports path — verified, and the SIG spec's signature table NOTE: new modules add
  rows when SIG's drift-tested table exists); zero perf regression — re-run
  `tests/vm_bench.rs` geomean (the cap gate's `all_granted()` short-circuit means the new
  `required_cap` arms cost nothing on the default path; det-off test runs are byte-identical);
  docs + NAV per §2.5; production-grade mandate (any bug found in neighbors is fixed
  in-branch, failing-test-first).
- **goal-perf 15–18 where applicable:** 15 — no new engine configuration (n/a, recorded);
  16 — no headline perf claims; the one measured number is the Gate-12 re-run; 17 — the
  geomean ≥2× floor re-verified at each phase merge; 18 — peak RSS on the corpus re-checked at
  each phase merge (stdlib additions must not regress baseline memory).
- **Security batteries (§2.7) are merge-blocking** for their phases.
- **Negative space:** `tests/batt_negative_space.rs` asserts `ASO_FORMAT_VERSION` unchanged vs
  the merge-base (read the constant — it is 27 at drafting, 28 once DEFER lands; never assert a
  literal), no new
  `Op`, no new serializer tag, no new `Value` variant (compile-time size/match pins).
- The caps **completeness test** passes with every new module classified; `cap_audit.rs` rows
  for every gated fn (`connectTls`, `serve({tls})` implied by module, `jwt.jwks`, `oauth.*`,
  `archive.extractTo/createFromDir`, `email.send/connect`, `blob.*`).

## 17. Non-goals & recorded futures (no silent drops)

mTLS/client certs (both directions); `insecureSkipVerify`; raw `tcp.listenTls`; custom cipher
suites; JWT RS384/512+ES384/512+PS*+EdDSA; encrypted JWE; full OIDC RP certification surface;
XML namespace-URI resolution; full HTML5 tree-construction parsing (the sanitizer is a
tokenizer-based allowlist, stated); 7z/rar; chrono-tz named timezones (shared with std/date);
node-semver `includePrerelease` option + loose mode; markdown front-matter/highlighting/MDX;
word-level diff; `test.prop` async-property bodies beyond what `call_value` already awaits
(generator-of-generators/recursive gens). Each is documented on its unit's docs page.

## 18. Cross-subsystem checklist (per phase)

`STD_MODULES` + `std_module_exports` + `call_stdlib` arms; `required_cap`/`KNOWN_UNGATED`/
`PER_FUNC` + completeness test; `cap_audit.rs`; `std_arity.rs`; docs pages + sections + `NAV`
+ README table (+ DOCS claiming table if present); examples + `EXAMPLE_SKIPS` rows where
documented; `vm_differential` goldens; `tests/batt_negative_space.rs`; `Cargo.toml` features
(+ the `default` list) with doc-comments in the house style; CLAUDE.md (stdlib feature list +
the new modules' gotcha notes), `superpowers/roadmap.md`, `goal-perf.md` status flip — each
phase's final task.
