# Backend Batteries (BATT) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the phase merges. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship the backend-batteries stdlib tier — TLS (`server.serve({tls})`,
`tcp.connectTls`), `std/jwt` + `std/oauth` + session helpers, `std/archive`, `std/xml` +
`std/html`, `std/email`, `std/blob` (SigV4 S3 client), deterministic testing
(`ascript test --seed/--frozen-time`) + `std/test` property testing with shrinking, and the T2
toolbelt (`std/cron`, `std/semver`, `std/markdown`, `std/diff` + snapshot wiring) — as pure
stdlib + CLI additions: **no grammar change, no opcode, no new `Value` variant, no `.aso` bump
(`ASO_FORMAT_VERSION` unchanged vs the merge-base — 27 at drafting, 28 once DEFER lands;
asserted by reading the constant, never a literal)**, four-mode byte-identical, every unit behind a
default-on feature flag with a caps verdict, docs page + NAV, intro + advanced examples, and
happy+edge tests in BOTH feature configs.

**Spec:** `superpowers/specs/2026-06-12-backend-batteries-design.md` (BATT). **Read it first
and in full** — §0 (verified code facts), §2 (the module recipe, error tiering, caps,
features, docs, examples policy, security batteries), §3 (dependency verdicts — they are
locked decisions), §4–§14 (per-unit APIs), §15 (phasing), §16 (gates). Section references (§)
below are into it.

**Before writing any code, read these files end to end** (the spec's facts were verified
2026-06-12 — **re-grep every symbol before editing**, names are the anchors, line numbers
drift):
- `src/stdlib/mod.rs` (registration recipe, `STD_MODULES`, `required_cap`, the caps
  completeness test, `want_*` helpers, `MAX_ALLOC_COUNT`)
- `src/stdlib/crypto.rs` (the substrate + the Tier-1/Tier-2 + det-seam house style to copy)
- `src/stdlib/net_tcp.rs` (`TcpStreamState`, ResourceState/NativeKind/`governing_cap` pattern,
  take-out-across-await)
- `src/stdlib/http_server.rs` (`ServeOpts`, the accept loop, multi-isolate serve boot path)
- `src/stdlib/net_http.rs` (`shared_client()` — the pooled reqwest client)
- `src/stdlib/compress.rs` (the tar/zip crates in use; the fns that MUST NOT change)
- `src/stdlib/schema.rs` (tagged-Object posture + call-site method hook — jwt keys, test gens)
- `src/det.rs` + `src/interp.rs` determinism block (~`:1355-1420`) + `run_registered_tests_filtered`
  (~`:2724`) + `src/lib.rs` `run_tests_with_options`/`run_tests_serial`/`run_tests_parallel`
- `src/stdlib/assert_mod.rs` (`snapshot_impl` — the diff wiring point; test registration)
- `src/fuzzgen/mod.rs` (the generator philosophy `test.prop` surfaces)
- `tests/cap_audit.rs` (the audit row style), `tests/vm_differential.rs` (`EXAMPLE_SKIPS`),
  `src/check/std_arity.rs`
- `docs/assets/app.js` `NAV` + one stdlib page (e.g. `docs/content/stdlib/data.md`) for voice

**Architecture (four independently-merged phases, spec §15):**
- **Phase A** (`feat/batt-auth`): TLS server+client → crypto substrate (hmacSha384/512,
  timingSafeEqual) → jwt core → RS256/ES256 → JWKS → oauth → cookies/sessions → docs/examples
  → holistic → merge.
- **Phase B** (`feat/batt-data`, branched AFTER A merges — email needs TLS): archive →
  xml/html → email → blob → docs/examples → holistic → merge.
- **Phase C** (`feat/batt-testing`, independent — may run parallel to B on its own branch):
  det CLI flags + per-test context → `std/test` gens → prop runner + shrinking → reporting →
  docs/examples → holistic → merge.
- **Phase D** (`feat/batt-t2`, after B — markdown needs the sanitizer): cron → semver →
  markdown → diff + snapshot wiring → finish (CLAUDE.md/roadmap/goal-perf) → holistic → merge.

**Tech stack:** Rust; the `!Send` per-isolate runtime (never add `Send` bounds; never hold a
`RefCell`/resource borrow across `.await` — use take-out-across-await); tokio
`current_thread` + `LocalSet`; new crates ONLY per the spec-§3 locked verdicts
(`tokio-rustls`, `rustls-pemfile`, `webpki-roots`, `rsa`, `p256`, `quick-xml`,
`pulldown-cmark` — everything else hand-rolled or already present); tests via `cargo test` in
BOTH feature configs.

**Hard rules carried from the spec:**
- **No `.aso`/opcode/grammar/`Value` change** — `tests/batt_negative_space.rs` pins all four.
- **Error tiering §2.2:** type misuse / malformed programmer input / cap denial = Tier-2;
  world-or-untrusted-data failures = Tier-1 `[value, err]`. `jwt.verify` failure is Tier-1,
  ALWAYS. Every byte parser is alloc-bounded (`want_count` discipline) and panic-free on
  hostile input.
- **Caps §2.3:** new gated modules get `required_cap` arms; per-func modules
  (`jwt`/`archive`/`email`) extend the completeness test via the new `PER_FUNC` list; every
  gated fn gets a `cap_audit.rs` row. `std/compress`'s existing fns are UNTOUCHED.
- **Security batteries §2.7 are merge-blocking:** `zip_slip_battery`,
  `jwt_alg_confusion_battery`, `sigv4_vector_battery`, `sanitizer_xss_battery`,
  `smtp_injection_battery`, `pem_hostile_battery`.
- **Examples are self-contained loopback** (§2.6): network units spin their own in-process
  counterpart; property examples pass an explicit seed. Long-running server examples use a
  documented `EXAMPLE_SKIPS` row + a dedicated integration test.
- **`std/test` + the det test flags are CORE** (no feature gate) — must build and pass under
  `--no-default-features`.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals (spec §17
is the only sanctioned deferral list). TDD per task: failing test → minimal code → green →
commit. Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/stdlib/tls.rs` (shared TLS config builders: PEM loaders, client connector, server
  acceptor — used by net_tcp/http_server/email)
- `src/stdlib/jwt.rs`, `src/stdlib/oauth.rs`, `src/stdlib/archive.rs`, `src/stdlib/xml.rs`,
  `src/stdlib/html.rs`, `src/stdlib/email.rs`, `src/stdlib/blob.rs`, `src/stdlib/test_mod.rs`,
  `src/stdlib/cron.rs`, `src/stdlib/semver.rs`, `src/stdlib/markdown.rs`, `src/stdlib/diff.rs`
- `tests/batt_negative_space.rs`; `tests/tls_server.rs`; security batteries live inside their
  modules' `#[cfg(test)]` + `tests/cap_audit.rs` rows
- Examples (each + corpus registration): `examples/tls_echo.as`, `examples/jwt_basics.as`,
  `examples/archive_roundtrip.as`, `examples/xml_basics.as`, `examples/email_builder.as`,
  `examples/blob_basics.as`, `examples/property_testing.as`, `examples/cron_next.as`,
  `examples/semver_ranges.as`, `examples/markdown_render.as`, `examples/diff_unified.as`;
  `examples/advanced/https_server.as` (SKIP-listed), `examples/advanced/oauth_pkce_flow.as`,
  `examples/advanced/sessions.as`, `examples/advanced/backup_tool.as`,
  `examples/advanced/feed_reader.as`, `examples/advanced/smtp_send.as`,
  `examples/advanced/blob_sync.as`, `examples/advanced/prop_roundtrips.as`,
  `examples/advanced/job_scheduler.as`, `examples/advanced/docs_site_gen.as`
- Docs: `docs/content/stdlib/{auth,archive,markup,email,blob}.md`

**Modified files:**
- `Cargo.toml` (features `tls,auth,archive,xml,email,blob,cron,semver,markdown,diff` + deps +
  `default` list), `src/stdlib/mod.rs` (mods, exports, dispatch, `STD_MODULES`,
  `required_cap`, `KNOWN_UNGATED`, `PER_FUNC`), `src/stdlib/crypto.rs` (hmacSha384/512,
  timingSafeEqual), `src/stdlib/net_tcp.rs` (connectTls + TlsStream), `src/value.rs`
  (`NativeKind` variants + `governing_cap`), `src/interp.rs` (`ResourceState` variants;
  core `set_determinism`; prop runner; per-test det install), `src/stdlib/http_server.rs`
  (ServeOpts.tls + acceptor + session helpers), `src/lib.rs` + `src/main.rs` +
  `src/worker/testrun.rs` (`--seed`/`--frozen-time` threading), `src/stdlib/assert_mod.rs`
  (snapshot diff wiring + failure seed-suffix), `src/check/std_arity.rs`,
  `tests/cap_audit.rs`, `tests/vm_differential.rs` (corpus + goldens + skips),
  `docs/assets/app.js` (NAV), `docs/content/stdlib/{net,time,utilities,assert}.md` +
  `docs/content/cli.md`, `README.md`, `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`

---

# Phase A — TLS + the auth story (`feat/batt-auth`)

## Task A0: branch, negative-space pins, feature scaffolding

**Files:** `tests/batt_negative_space.rs`, `Cargo.toml`.

- [ ] **Step 1:** `git checkout -b feat/batt-auth main`; `cargo build --release` clean.
- [ ] **Step 2:** Write `tests/batt_negative_space.rs` (PASSING now, guards the whole
  campaign):

```rust
//! BATT negative space — the spec promises NO engine surface change (spec §16).
//! These pins fail if any BATT unit accidentally grows the engine.

#[test]
fn aso_format_version_unchanged() {
    // BATT touches no opcode/serialization layout. Merge-base-relative, NOT a campaign
    // literal: at branch time, read the constant from the merge-base
    // (`git show $(git merge-base HEAD main):src/vm/aso.rs | grep ASO_FORMAT_VERSION`)
    // and record it here. 27 at drafting; DEFER legitimately bumps to 28 before BATT
    // executes — the assertion is "unchanged by THIS branch", never "is 27".
    const ASO_AT_MERGE_BASE: u32 = 27; // ← re-record from the actual merge-base at branch time
    assert_eq!(ascript::vm::aso::ASO_FORMAT_VERSION, ASO_AT_MERGE_BASE,
        "BATT must not bump the .aso format — asserts no change vs this branch's \
         merge-base (re-record the const if rebasing over a spec that bumped it)");
}

#[test]
fn value_size_unchanged() {
    // No new Value variant / no size growth (24 bytes at spec time, src/value.rs).
    assert_eq!(std::mem::size_of::<ascript::value::Value>(), 24);
}
```

  (Verify the two symbols' visibility first — `ASO_FORMAT_VERSION` is `pub` in `src/vm/aso.rs`;
  if `vm::aso` isn't re-exported publicly, add the minimal `pub use`/getter rather than
  weakening the pin. If another spec's merge has legitimately moved either constant by the
  time this runs, pin the CURRENT value and note it in the commit.)
- [ ] **Step 3:** `Cargo.toml`: add features `tls = ["net", "dep:tokio-rustls",
  "dep:rustls-pemfile", "dep:webpki-roots"]` and `auth = ["crypto", "data", "net", "dep:rsa",
  "dep:p256"]`; add the optional deps (`tokio-rustls = { version = "0.26", optional = true }`,
  `rustls-pemfile = { version = "2", optional = true }`, `webpki-roots = { version = "0.26",
  optional = true }` — **match the versions already in `Cargo.lock`** via `cargo tree`, do not
  pull a second copy; `rsa = { version = "0.9", optional = true }`, `p256 = { version = "0.13",
  features = ["ecdsa"], optional = true }`). Add both to `default`. Doc-comment each in the
  house style (see the `ffi`/`shared` comments) including the §3 verdict one-liner.
- [ ] **Step 4:** `cargo build` + `cargo build --no-default-features` + `cargo tree -d | head`
  (no duplicate rustls stacks). Commit.
- [ ] Independent review: run the builds, confirm the lock didn't fork versions, probe the
  negative-space pins by (locally, unstaged) bumping a constant and watching them fail.

## Task A1: TLS shared core + `tcp.connectTls` (TDD)

**Files:** `src/stdlib/tls.rs` (new), `src/stdlib/net_tcp.rs`, `src/value.rs`,
`src/interp.rs` (ResourceState), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests first):** in `net_tcp.rs` `#[cfg(all(test, feature = "tls"))]`,
  write tests that (a) spin an in-process tokio-rustls echo server on `127.0.0.1:0` using a
  BAKED self-signed cert/key fixture (generate ONCE with openssl, commit as
  `src/stdlib/testdata/tls_test_cert.pem`/`tls_test_key.pem`, CN/SAN `localhost` long expiry;
  document regeneration in a comment), then assert
  `tcp.connectTls("127.0.0.1", port, {caCert, serverName: "localhost"})` round-trips
  write→readLine; (b) wrong `serverName` → Tier-1 err; (c) plain-TCP server on the other end →
  Tier-1 handshake err; (d) the `pem_hostile_battery`: truncated PEM / garbage / empty
  `caCert` → clean Tier-1/Tier-2 per §2.2, never a Rust panic.
- [ ] **Step 2:** implement `src/stdlib/tls.rs`:

```rust
//! Shared TLS plumbing (feature `tls`): PEM loading + the client connector and
//! server acceptor builders used by net_tcp (connectTls), http_server (serve
//! {tls}), and email (STARTTLS). rustls defaults — TLS 1.2/1.3, no custom
//! ciphers (spec §4.1). PEM STRINGS only, never paths (caps honesty, §4.2).

use std::sync::Arc;

pub(crate) fn load_certs(pem: &str) -> Result<Vec<rustls_pki_types::CertificateDer<'static>>, String> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("certificate PEM contains no certificates".into());
    }
    Ok(certs)
}

pub(crate) fn load_key(pem: &str) -> Result<rustls_pki_types::PrivateKeyDer<'static>, String> {
    rustls_pemfile::private_key(&mut pem.as_bytes())
        .map_err(|e| format!("invalid private key PEM: {e}"))?
        .ok_or_else(|| "private key PEM contains no key".into())
}

/// Client config: webpki-roots + optional extra `caCert` PEM roots (spec §4.3).
pub(crate) fn client_config(ca_cert: Option<&str>, alpn: &[String]) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = ca_cert {
        for cert in load_certs(pem)? {
            roots.add(cert).map_err(|e| format!("invalid caCert: {e}"))?;
        }
    }
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = alpn.iter().map(|p| p.as_bytes().to_vec()).collect();
    Ok(Arc::new(cfg))
}
```

  plus `server_config(cert, key, sni: &[(String, String, String)]) ->
  Result<Arc<rustls::ServerConfig>, String>` (single-cert via `with_single_cert`, SNI via
  `ResolvesServerCertUsingSni`; `alpn_protocols = [b"http/1.1"]`). Register
  `pub mod tls;` under `#[cfg(feature = "tls")]` in `mod.rs` (NOT a script-facing module — no
  `STD_MODULES` entry; it's plumbing).
- [ ] **Step 3:** `net_tcp.rs`: add `connectTls` export (`#[cfg(feature = "tls")]`) +
  dispatch arm; new `NativeKind::TlsStream` (`governing_cap() == Some(Net)`) +
  `ResourceState::TlsStream(TlsStreamState)` where `TlsStreamState` mirrors `TcpStreamState`'s
  buffered reader over `tokio_rustls::client::TlsStream<TcpStream>`; method surface
  `read/readLine/write/close/alpn` via the existing `tcp_stream_method` shape
  (take-out-across-await; never hold the resources borrow over the handshake await). Options
  parsing per spec §4.3 (`serverName` default `host`; bad option types Tier-2; handshake/
  verification failures Tier-1).
- [ ] **Step 4:** green in both configs:
  `cargo test --features tls connect_tls` then full `cargo test` +
  `cargo test --no-default-features`. `cargo clippy --all-targets` +
  `--no-default-features --all-targets`. Commit.
- [ ] Independent review: run the tests; probe — `connectTls` to a closed port (Tier-1, no
  hang — confirm a connect timeout exists or add one), `alpn` negotiation value, huge PEM
  string (alloc-bounded), `caps.drop("net")` then a method on an OPEN TlsStream handle (the
  per-handle `governing_cap` re-check must deny — add the cap_audit row now).

## Task A2: `server.serve({tls})` incl. multi-isolate

**Files:** `src/stdlib/http_server.rs`.

- [ ] **Step 1 (failing tests):** http_server tests — (a) `create` → `bind(127.0.0.1, 0)` →
  `serve({tls: {cert, key}, maxRequests: 1})` with an in-test reqwest client
  (`danger`-free: build the client with the test CA via
  `reqwest::Certificate::from_pem`) asserting an HTTPS 200; (b) malformed PEM → Tier-1 from
  `serve` BEFORE accepting; (c) a plain-HTTP request against the TLS port gets a handshake
  failure and the server KEEPS SERVING (send garbage bytes, then a real TLS request succeeds —
  the log-and-continue rule §4.2); (d) SNI map selects per-host cert (two baked certs, assert
  via the client's expected root per host); (e) `workers: 2` + `tls` serves over both isolates
  (the SO_REUSEPORT path; total-requests budget asserted like `server_multicore.rs`).
- [ ] **Step 2:** implement: `ServeOpts` gains `tls: Option<TlsServeCfg>` (`cert`, `key`,
  `sni: Vec<(host, cert, key)>` — plain `String`s, `Send`); parse in the shared opts parser
  (wrong types Tier-2). In `accept_loop`, build the acceptor ONCE before the loop
  (`tls::server_config` → `TlsAcceptor`); on accept, `acceptor.accept(stream).await` —
  handshake error → `continue` (count nothing); pass the TLS stream into the existing
  `serve_connection` plumbing (hyper is generic over `Read+Write` via `TokioIo`). Multi-isolate
  boot closure carries the PEM strings; each isolate builds its own acceptor.
- [ ] **Step 3:** green both configs; clippy both. Commit.
- [ ] Independent review: run; probe — cert/key mismatch error text, `tls` with
  `requestTimeout`/`maxBodySize` still enforced over TLS, Windows-fallback path compiles
  (`cfg` review only), and confirm `serve` without `tls` is byte-identical (diff a non-TLS
  serve test's output against `main`).

## Task A3: TLS examples + docs + audit rows

**Files:** `examples/tls_echo.as`, `examples/advanced/https_server.as`, `tests/tls_server.rs`,
`tests/vm_differential.rs`, `tests/cap_audit.rs`, `docs/content/stdlib/net.md`.

- [ ] **Step 1:** `examples/tls_echo.as` — fully error-handled: embed the test cert/key PEM as
  string constants, `server.create()`+`bind(127.0.0.1, 0)`+spawned `serve({tls, maxRequests:1})`,
  `http.get` (with the CA option — verify `std/net/http` exposes a per-request `caCert`/client
  opt; if it does NOT, use `tcp.connectTls` + a hand-written HTTP/1 request in the example and
  record the http-client `caCert` option as an in-branch addition — it is small and needed for
  the example to be honest). Deterministic output → corpus member. Run it four-mode by hand:
  `target/release/ascript run examples/tls_echo.as` + `--tree-walker`.
- [ ] **Step 2:** `examples/advanced/https_server.as` (production-shaped, long-running) +
  `EXAMPLE_SKIPS` row (`LongRunningServer`) + `tests/tls_server.rs` integration test driving
  it (the `server_multicore.rs` precedent).
- [ ] **Step 3:** cap_audit rows: `tcp.connectTls` denied under `--deny net` / `--sandbox`;
  granted works (loopback). Docs: TLS sections on `stdlib/net.md` (server + tcp), incl. the
  PEM-strings-not-paths rationale and the §4.1 honest-scope/recorded-futures list.
- [ ] **Step 4:** full suite both configs + `cargo test --test vm_differential` (corpus picks
  up the example). Commit.
- [ ] Independent review: run the examples all four modes; served-docs sanity
  (`cd docs && python3 -m http.server`, check net.md renders + anchors).

## Task A4: crypto substrate — `hmacSha384`/`hmacSha512`/`timingSafeEqual`

**Files:** `src/stdlib/crypto.rs`, `docs/content/stdlib/*` (crypto's home page section),
`src/check/std_arity.rs`.

- [ ] **Step 1 (failing tests):** RFC 4231 HMAC-SHA-384/512 test vectors; `timingSafeEqual`
  truth table incl. length-mismatch → `false` (not panic), bytes|string args, wrong-type
  Tier-2.
- [ ] **Step 2:** implement (mirror the existing `hmacSha256` arm with `Sha384`/`Sha512`;
  `timingSafeEqual` via `subtle`-free constant-time loop: length check then byte-OR fold —
  comment WHY a naive early-exit compare is the bug). Exports + arity rows + docs section.
- [ ] **Step 3:** green both configs; clippy. Commit.
- [ ] Independent review: verify vectors against an independent tool (`openssl dgst -sha384
  -hmac key`), probe empty-key/empty-data edges.

## Task A5: `std/jwt` core — module skeleton, typed keys, HS256/384/512 (TDD)

**Files:** `src/stdlib/jwt.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** in `jwt.rs` tests — (a) the RFC 7515 A.1 HS256 vector
  (token → claims, claims+key → byte-identical token with the fixed header); (b)
  sign↔verify roundtrip HS256/384/512; (c) `exp` in the past → Tier-1 err mentioning
  `expired`; `nbf` future → err; `leeway` rescues both; `iss`/`aud` mismatch → err; (d) the
  `jwt_alg_confusion_battery` (spec §2.7/§5.3): `alg:"none"`, `"None"`, `"NONE"` all rejected;
  header `alg: HS256` verified with an `algs: ["HS384"]` allowlist → err; tampered
  header/payload/signature each err; a token with `jku`/`jwk` headers verifies purely by the
  provided key (headers ignored); (e) malformed compact form (0/1/3 dots, bad base64url, huge
  segments) → Tier-1, alloc-bounded, no Rust panic; (f) `decode` returns
  `{header, claims, signature, verified: false}` without any key.
- [ ] **Step 2:** implement the module:

```rust
//! `std/jwt` — JSON Web Tokens (feature `auth`). Typed keys kill alg-confusion
//! structurally (spec §5.3): a key value is a tagged Object {__jwtkey: kind, ...}
//! (the schema posture — no new Value kind) and verify intersects the header alg
//! with the key kind's algorithm set; `alg: "none"` is rejected before dispatch.
//! Verification failure is Tier-1 [nil, err] ALWAYS (auth failures are control
//! flow, spec §2.2); malformed *programmer* input (a non-key where a key is due)
//! is Tier-2.

const KEY_TAG: &str = "__jwtkey";

fn b64url(data: &[u8]) -> String { /* base64 URL_SAFE_NO_PAD via the existing dep */ }
fn b64url_decode(s: &str, max: usize) -> Result<Vec<u8>, String> {
    if s.len() > max { return Err("jwt segment too large".into()); } // alloc bound
    /* decode, map_err */
}

fn algs_for_key_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "hmac" => &["HS256", "HS384", "HS512"],
        "rsa-public" | "rsa-private" => &["RS256"],
        "ec-public" | "ec-private" => &["ES256"],
        _ => &[],
    }
}
```

  `sign(claims, key, opts)`: header `{alg, typ:"JWT"} + opts.headers`; `expiresIn` sets
  `exp = (clock_now_ms()/1000).floor() + n` (the det seam — `impl Interp` because it reads the
  clock); compact-serialize via `json::stringify` of the claims OBJECT (preserve insertion
  order — document that claim order follows the object). HS sign via hmac; verify via
  `Mac::verify_slice` (constant-time). exp/nbf/iat checked AFTER signature (never leak timing
  on claim contents before authenticity). Register: `pub mod jwt` under
  `#[cfg(feature = "auth")]`; exports + dispatch (`"jwt" => self.call_jwt(...)`) +
  `STD_MODULES` `"std/jwt"`; `required_cap` per-func arm
  (`"jwt" => match func { "jwks" => Some(Cap::Net), _ => None }`); extend the completeness
  test with the `PER_FUNC` list (spec §2.3) asserting jwt has ≥1 gated + ≥1 ungated func.
- [ ] **Step 3:** green both configs (in `--no-default-features` the module is absent but
  `STD_MODULES` still lists it — checker-only, verified by an `is_known_std_module` test);
  clippy both. Commit.
- [ ] Independent review: run the battery; probe — unicode claims, duplicate-dot tokens, a
  10 MB token string (bounded), `exp` as float vs int (NUM: both numeric subtypes accepted),
  verify with `clock` override, and cross-check one HS256 token against `jwt.io`-style
  reference output generated via openssl in the review notes.

## Task A6: RS256 + ES256

**Files:** `src/stdlib/jwt.rs`.

- [ ] **Step 1 (failing tests):** generate fixtures ONCE with openssl (commit under
  `src/stdlib/testdata/`: RSA-2048 PKCS#8 pair, P-256 pair, both PEM): (a) RS256 sign↔verify
  roundtrip; verify against a token signed out-of-band by openssl (`openssl dgst -sha256
  -sign`) — byte-level cross-check; (b) ES256 roundtrip + **the JOSE-encoding pin**: signature
  segment is exactly 64 bytes b64url-decoded (r||s, NOT DER — a DER-encoded signature must
  FAIL verification); (c) `rsaPublicKey`/`ecPublicKey` on hostile PEM → Tier-1; an EC PEM fed
  to `rsaPublicKey` → Tier-1 naming the mismatch; (d) battery extension: RSA public key used
  as the key for an `alg: HS256` token → err (the structural kill, §5.3).
- [ ] **Step 2:** implement key constructors (`rsa::RsaPublicKey::from_public_key_pem` +
  pkcs1 fallback; `p256::ecdsa::{SigningKey, VerifyingKey}` from SEC1/PKCS#8 PEM) storing PEM
  text in the tagged key Object (re-parse per op — keys are not hot-path; simpler than a
  native handle and keeps keys sendable/printable-safe: the tag is shown, material is in the
  object — document); RS256 via `rsa::pkcs1v15::SigningKey<Sha256>`/`VerifyingKey`; ES256 via
  `p256::ecdsa` with `Signature::from_slice` (fixed-width) — never DER.
- [ ] **Step 3:** green both configs; clippy. Commit.
- [ ] Independent review: independent openssl verification of an AScript-signed RS256 token;
  probe — 1024-bit key (works; note), wrong-curve PEM (P-384) → clean Tier-1, empty claims.

## Task A7: JWKS fetch+cache + `std/oauth`

**Files:** `src/stdlib/jwt.rs`, `src/stdlib/oauth.rs` (new), `src/value.rs` + `src/interp.rs`
(JwksCache ResourceState), `src/stdlib/mod.rs`, `tests/cap_audit.rs`.

- [ ] **Step 1 (failing tests):** using an in-process `http_server` stub (run_source-style
  tests like the existing net tests): (a) `jwt.jwks(url)` fetches, `handle.verify` resolves
  `kid` → RS256 verifies; (b) unknown `kid` → exactly ONE refetch (stub counts hits) then
  Tier-1; (c) TTL expiry refetches; within TTL does not; (d) malformed JWKS JSON / non-RSA-EC
  keys skipped with the rest usable; (e) `--deny net` denies `jwt.jwks` (cap_audit row) while
  `jwt.sign` still works under `--deny net` (the per-func split, asserted positively);
  (f) oauth: `exchangeCode` posts grant_type/code/verifier form-encoded (stub asserts body +
  Basic auth), token JSON → tokens object; `clientCredentials`; `refresh`; non-200 → Tier-1
  carrying the error body; `discover` parses issuer metadata; (g) `oauth.pkce()` S256 vector
  (RFC 7636 appendix B).
- [ ] **Step 2:** implement: `NativeKind::JwksCache` (`governing_cap == Some(Net)`) +
  `ResourceState::JwksCache { url, ttl, keys: HashMap<String, JwkKey>, fetched_at, refetching }`;
  fetch via `net_http::shared_client()` (clone the client out, never hold the resources borrow
  across the await); JWK→key conversion (RSA `n`/`e` b64url → `rsa::RsaPublicKey::new`; EC
  P-256 `x`/`y` → encoded point). `oauth.rs` per spec §5.6 over the same pooled client;
  whole-module `Net` `required_cap` arm (feature-gated like the dispatch arm); `STD_MODULES`
  `"std/oauth"`; arity rows for both modules.
- [ ] **Step 3:** green both configs; clippy. Commit.
- [ ] Independent review: run; probe — jwks handle after `caps.drop("net")` (per-handle
  re-check denies refetch but cached verify still works? DECISION per spec: verify with a
  warm cache is pure → allowed; the REFETCH is denied — assert exactly that), stub returning
  302 (reqwest policy — pin behavior), concurrent verify calls during refetch (the in-flight
  flag — no borrow across await).

## Task A8: signed cookies + sessions; phase docs/examples/NAV

**Files:** `src/stdlib/http_server.rs`, `examples/jwt_basics.as`,
`examples/advanced/oauth_pkce_flow.as`, `examples/advanced/sessions.as`,
`docs/content/stdlib/auth.md` (new), `docs/assets/app.js`, `README.md`,
`src/check/std_arity.rs`, `tests/vm_differential.rs`.

- [ ] **Step 1 (failing tests):** cookie sign→verify roundtrip; tampered value/sig → Tier-1;
  CRLF in name/value → Tier-2 (the `smtp_injection_battery`'s sibling — header-injection
  precedent); attribute rendering matrix (`httpOnly`/`secure`/`sameSite` defaults per §5.7);
  `session(req, secret)` on absent cookie → `[{}, nil]`; roundtrip through a real
  `http_server` request cycle.
- [ ] **Step 2:** implement the four helpers (§5.7) using `crypto`'s hmac + the new
  `timingSafeEqual`; payload `base64url(json)` + `.` + `base64url(hmac)`.
- [ ] **Step 3:** examples — `examples/jwt_basics.as` (sign/verify/decode/expiry, pure,
  corpus); `examples/advanced/oauth_pkce_flow.as` (in-script token endpoint + JWKS endpoint;
  full PKCE flow; corpus); `examples/advanced/sessions.as` (loopback server, login sets a
  session, next request reads it; `maxRequests`-bounded → corpus). Four-mode run each.
- [ ] **Step 4:** docs — `docs/content/stdlib/auth.md` (jwt + oauth + sessions; the §5.4
  unverified-decode warning box; the §5.3 typed-key explanation; recorded futures); NAV entry
  `['stdlib/auth', 'Auth (JWT, OAuth2, sessions)']`; README row; arity rows; a
  `vm_differential` golden for jwt sign/verify determinism (fixed clock via det? no — use
  claims without exp so output is stable).
- [ ] **Step 5:** full suite + clippy both configs. Commit.
- [ ] Independent review: serve docs + click the NAV; run all three examples four-mode;
  probe cookie attribute edge (`maxAge: 0`), session > 4 KB (document the cookie-size note).

## Task A9: Phase A holistic review + merge

- [ ] Holistic review subagent: the FULL phase diff (`git diff main...`) — hunt missed
  validation, tiering inconsistencies vs §2.2, caps holes (run the completeness test, scan
  for any new OS-touching path not in `required_cap`), borrow-across-await, docs drift.
- [ ] Gates sweep (spec §16): `cargo test` + `--no-default-features`; clippy both;
  `cargo test --test vm_differential` both configs; `cargo test --test cap_audit`;
  `tests/batt_negative_space.rs`; vm_bench geomean ≥2× re-run (Gate 17) + note in the PR;
  peak-RSS spot-check on the corpus (Gate 18, `/usr/bin/time -l`).
- [ ] Update `goal-perf.md` BATT entry (Phase A merged), `superpowers/roadmap.md`.
- [ ] Merge `--no-ff` to `main`.

---

# Phase B — data & integration (`feat/batt-data`, branched after A merges)

## Task B0: branch + features

- [ ] `git checkout -b feat/batt-data main` (A merged). `Cargo.toml`: features
  `archive = ["compress"]`, `xml = ["data", "dep:quick-xml"]`, `email = ["net", "tls",
  "data"]`, `blob = ["net", "crypto", "data", "xml"]`; deps `quick-xml = { version = "0.39",
  default-features = false, optional = true }` (match the lock's version). All into `default`.
  Build both configs. Commit.

## Task B1: `std/archive` — tar core, writer handle, entries generator (TDD)

**Files:** `src/stdlib/archive.rs` (new), `src/value.rs`/`src/interp.rs`
(`NativeKind::ArchiveWriter` + `ResourceState::ArchiveWriter`), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) `tarWriter` → `add("a.txt", "hello")`,
  `add("dir/", nil, {dir: true})` → `finish` → `tarEntries` roundtrip (names, sizes, modes,
  data); (b) `{gzip: true}` writer → magic-sniffed `tarEntries` reads it; (c)
  `{deterministic: true}`: two writers, same adds → **byte-identical** outputs (mtime/uid/gid
  zeroed); (d) `tarAppend` preserves originals + appends; (e) generator LAZINESS: an archive
  whose 2nd entry header is corrupted yields entry 1 fine, errs on `next()` #2 (generator
  Tier-1 protocol — match how `stream` errors surface, verify first); (f) hostile inputs:
  truncated header, size field `0xFFFFFFFFFFF` (bounded by `MAX_ALLOC_COUNT` →
  clean Tier-1, no alloc-abort), non-tar garbage; (g) writer after `finish` → Tier-2.
- [ ] **Step 2:** implement. The CORE is `pub(crate) mod tarcore` (spec §6.6 — RT's `--oci`
  ships its own writer per owner decision; `tarcore` is the clean internal layering plus the
  recorded optional later unification target):

```rust
//! std/archive (feature `archive`) — streaming tar+zip over the ALREADY-vendored
//! tar/zip/flate2 crates (zero new deps, spec §3). Two planes: in-memory Bytes
//! APIs (ungated) and Fs-gated disk helpers (§6). std/compress's one-shot fns
//! are UNCHANGED (§6.7) — this module is the streaming/hardened superset.

pub(crate) mod tarcore {
    //! Plain-Rust tar building, no Value types (spec §6.6). RT's `--oci` rolls its
    //! own writer (owner decision); unifying it here is a recorded optional refactor.
    pub(crate) struct TarBuild { /* tar::Builder<Vec<u8>> or gz wrapper */ deterministic: bool, /* … */ }
    impl TarBuild {
        pub(crate) fn new(gzip: bool, deterministic: bool) -> Self { /* … */ }
        pub(crate) fn add(&mut self, name: &str, data: &[u8], mode: u32, mtime: u64, dir: bool) -> Result<(), String> {
            let mut h = tar::Header::new_gnu();
            h.set_size(if dir { 0 } else { data.len() as u64 });
            h.set_mode(mode);
            h.set_mtime(if self.deterministic { 0 } else { mtime });
            if dir { h.set_entry_type(tar::EntryType::Directory); }
            h.set_cksum();
            /* append_data; map_err */
        }
        pub(crate) fn finish(self) -> Result<Vec<u8>, String> { /* … */ }
    }
}
```

  Script surface per spec §6.2; writer = `ResourceState::ArchiveWriter` (enum
  Tar(TarBuild)/Zip(zip::ZipWriter<Cursor>)), ungated, `governing_cap() == None`, GC-untraced.
  `tarEntries` = `Value::Generator` over a native cursor (find the existing native-generator
  precedent — `stream.range`/worker generators — and mirror its construction; if no native
  in-memory generator precedent exists, drive via the `GenImpl` machinery in `src/coro.rs`
  with an internal iterator state — verify before coding, this is the one genuinely novel
  plumbing bit of the unit).
- [ ] **Step 3:** registration (recipe §2.1): `PER_FUNC` list gains `"archive"`; `required_cap`
  arm `"archive" => match func { "tarExtractTo" | "zipExtractTo" | "tarCreateFromDir" =>
  Some(Cap::Fs), _ => None }`. Green + clippy both configs. Commit.
- [ ] Independent review: run; probe — 0-byte entries, unicode + 100-char names (tar name
  limits — long-name GNU extension behavior pinned), a 50 MB entry (memory sane), generator
  `close()` mid-iteration, `for…of` over entries in a `.as` snippet four-mode.

## Task B2: archive — zip plane + Fs-gated disk helpers + `zip_slip_battery`

**Files:** `src/stdlib/archive.rs`, `tests/cap_audit.rs`.

- [ ] **Step 1 (failing tests):** zip mirror of B1's roundtrips (+ `compressedSize`,
  store-vs-deflate option); then the **`zip_slip_battery`** (spec §6.5) — construct hostile
  archives IN the tests (entry names `../evil`, `/abs`, `..\\win`, `C:\\x`, `a/../../evil`,
  NUL-embedded; tar entries with symlink type pointing outside, hardlink outside) and assert:
  `tarExtractTo`/`zipExtractTo` into a tempdir (1) returns Tier-1 naming the entry, (2) wrote
  NOTHING outside the dest (walk the tempdir parent), (3) symlinks skipped by default /
  Tier-1 under `{links: "error"}`. Plus happy-path extraction (nested dirs, modes), and
  `tarCreateFromDir` deterministic-mode byte-stability across two runs.
- [ ] **Step 2:** implement per spec §6.5 (canonicalize dest first; component-wise lexical
  normalization of entry paths — REJECT any `..` that escapes, never `fs::canonicalize` a
  not-yet-existing path; reject absolute/drive/UNC/NUL; whole-extract fails on first hostile
  entry; partial files cleaned up best-effort with the result err naming what was written).
  Disk helpers `#[cfg(feature = "sys")]` inside the module (the feature dep is `compress`;
  disk fns additionally need `sys` — if `sys` is off they are absent from exports;
  document).
- [ ] **Step 3:** cap_audit rows: `archive.tarExtractTo` denied under `--deny fs`; in-memory
  `archive.tarWriter` WORKS under `--sandbox` (positive per-func assertion). Green + clippy
  both. Commit.
- [ ] Independent review: run the battery; probe — extraction into a dest containing a
  PRE-EXISTING symlinked subdir (the classic second-order zip-slip: entry `link/evil.txt`
  where `link` was created by a previous entry — must be caught by the skip-links default +
  the lexical check; ADD this exact case if missing), case-insensitive filesystem note
  (macOS), empty archive extract.

## Task B3: `std/xml` (strict) — parse/stringify/escape

**Files:** `src/stdlib/xml.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) parse→shape pins (the §7.2 stable shape: tag/attrs/
  children; text nodes as strings; CDATA folded; comments/PIs dropped); (b)
  parse→stringify→parse fixpoint; (c) `{indent: 2}` pretty form pinned; (d) escaping both
  directions (the five entities + numeric refs); (e) malformed XML (unclosed, bad attr,
  half-entity) → Tier-1 with position info from quick-xml; (f) security: a DOCTYPE with
  entity definitions → entities NOT expanded (billion-laughs input returns Tier-1 or
  literal text, never expansion — pin which); external-entity URL never fetched (no `net` in
  this module at all — structural); depth budget (e.g. 256) + node budget breach → Tier-1;
  (g) namespace passthrough pins (`ns:tag` kept raw; `xmlns:*` in attrs; `localName`/`prefix`
  helpers).
- [ ] **Step 2:** implement over `quick_xml::Reader` (DTD events skipped;
  `check_end_names` on); builder over `quick_xml::Writer`. Register (ungated →
  `KNOWN_UNGATED` + `STD_MODULES` + arity rows).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: probe — BOM handling, `<?xml?>` declaration roundtrip, attribute
  order preservation (IndexMap), unicode tags, a 10 MB flat document (bounded, linear).

## Task B4: `std/html` — escape/unescape + the fail-closed sanitizer + XSS battery

**Files:** `src/stdlib/html.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** the **`sanitizer_xss_battery`** (spec §2.7) as a
  table-driven test: every vector → an output containing NO `<script`, no `on*=` attr, no
  `javascript:`/`data:` URL, and — the fail-closed property — any tag/attr not in the
  allowlist appears only ESCAPED. Vectors: `<script>`, `<ScRiPt>`, `<img src=x onerror=…>`,
  `<a href="javascript:…">`, `<a href="JaVaScRiPt:…">`, `<a href=" javascript:…">` (leading
  whitespace/control chars in scheme), `<svg onload>`, `<math href>`, `<iframe>`, mXSS
  nesting (`<noscript><p title="</noscript><img src=x onerror=…>">`), unterminated tags,
  `<<script>`, entity-encoded `&#106;avascript:`, attribute-without-quotes injection,
  comment/CDATA smuggling, `<style>` exfil. Plus positive cases: allowlisted formatting
  survives; relative + `https:` hrefs survive; custom `opts.tags/attrs/schemes` honored.
  Escape/unescape table (named HTML5 core set + numeric, round-trips).
- [ ] **Step 2:** implement per spec §7.3: a small lenient tokenizer (text / `<tag attrs>` /
  `</tag>` / comment / doctype — never errors, anything unparseable is text) → emit-from-parse
  serializer: allowlisted element → re-emit canonical `<tag attr="escaped">`; URL attrs
  parsed, scheme lowercased + control/whitespace-stripped before checking against
  `opts.schemes`; everything else escaped as text; void/self-closing handled; unclosed
  allowlisted tags auto-closed at end (emit balanced output). Document the default allowlist
  verbatim in the module doc.
- [ ] **Step 3:** register (ungated); green + clippy both. Commit.
- [ ] Independent review (adversarial, REQUIRED): spend real effort trying to smuggle a
  vector PAST the sanitizer (nested quotes, `&colon;`, tab-in-scheme `java\tscript:`,
  uppercase entities, `<a href=https://x onclick=…>` unquoted) — every new bypass found
  becomes a battery row + fix in-branch before acceptance.

## Task B5: `std/email` — message builder + injection battery

**Files:** `src/stdlib/email.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) `email.message` → `msg.raw()` wire-form pins: plain
  text; text+html (`multipart/alternative`, boundary format pinned via a seeded/deterministic
  boundary — derive boundaries from content hash so output is byte-stable for the corpus);
  attachments (`multipart/mixed`, base64 wrapped at 76); non-ASCII subject (RFC 2047
  encoded-word pin); custom headers; (b) `validateAddress` table (valid: simple, +tag,
  quoted-local recorded-unsupported → document; invalid: no-@, spaces, >254 chars, CRLF);
  (c) the **`smtp_injection_battery`** (builder half): `to: "a@b\r\nRCPT TO:<evil>"` →
  Tier-2; subject/header values with `\r`/`\n`/NUL → Tier-2; a 1000-char subject folds
  WITHOUT creating a parseable injected header.
- [ ] **Step 2:** implement builder per spec §8.1 (pure; ungated funcs). Register with the
  `PER_FUNC` split (`"email" => match func { "send" | "connect" => Some(Cap::Net), _ =>
  None }`).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: parse `msg.raw()` output with an independent tool (python
  `email.parser` in the review shell) — structure must round-trip; probe bcc handling (bcc
  in envelope, NOT in headers — pin it), empty body, attachment with unicode filename
  (RFC 2231 or documented ASCII-only — decide + record).

## Task B6: `std/email` — SMTP client (hand-rolled, STARTTLS) + scripted-stub tests

**Files:** `src/stdlib/email.rs`, `src/value.rs`/`src/interp.rs` (SmtpClient resource),
`tests/cap_audit.rs`.

- [ ] **Step 1 (failing tests):** an in-process **scripted SMTP stub** (a tokio task speaking
  canned dialogue, asserting received commands): (a) happy path EHLO→MAIL→RCPT→DATA→QUIT with
  dot-stuffing asserted on the wire (`\r\n.x` → `\r\n..x`); (b) AUTH PLAIN (base64 of
  `\0user\0pass` pinned) and AUTH LOGIN; (c) STARTTLS: stub advertises it, client upgrades
  (reuse the A1 test cert; stub wraps with tokio-rustls server side), EHLO re-sent after
  upgrade; (d) STARTTLS advertised-but-`tls:"starttls"` and upgrade FAILS → Tier-1 (never
  silently continue in plaintext — pin this); (e) partial RCPT rejection → `{accepted,
  rejected}` both populated; (f) multi-line `250-…/250 ` replies parsed; (g) 5xx at MAIL →
  Tier-1 with the server text; (h) `auth without TLS` → Tier-2 unless `allowInsecureAuth`;
  (i) timeout opt honored (stub stalls); (j) wire-layer CRLF defense: a crafted msg object
  bypassing the builder (hand-built object) still cannot inject — `send` re-validates.
- [ ] **Step 2:** implement per spec §8.2: protocol state machine over
  `TcpStream`/`TlsStream` (reuse `tls::client_config`); `ResourceState::SmtpClient`
  (`governing_cap == Net`); take-out-across-await; `Drop` closes; implicit-TLS mode connects
  through the connector first.
- [ ] **Step 3:** cap_audit rows (`email.send` denied under `--deny net`; `email.message`
  works under `--sandbox`). Green + clippy both. Commit.
- [ ] Independent review: run; probe — server closing mid-DATA, oversized reply line
  (bounded read), `port` defaulting (587 starttls / 465 implicit / 25 none — pin),
  greeting-timeout.

## Task B7: `std/blob` — SigV4 core + vector battery

**Files:** `src/stdlib/blob.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** the **`sigv4_vector_battery`**: stage-by-stage unit tests
  with the AWS sigv4 test-suite vectors (vendor 3-4 canonical cases as constants —
  `get-vanilla`, `get-vanilla-query-order-key-case`, `post-x-www-form-urlencoded`, plus an
  S3-style `UNSIGNED-PAYLOAD` presign vector): assert (a) the canonical request string
  byte-exactly, (b) the string-to-sign, (c) the final signature hex, (d) a presigned URL's
  full query string. Plus: URI single-encoding edge (`key with spaces/+`), sorted query
  params with duplicate keys, header trimming/lowercasing.
- [ ] **Step 2:** implement `mod sigv4` inside `blob.rs` (pure functions over
  `(method, uri, query, headers, payload_hash, region, service, datetime, creds)` — fully
  unit-testable without HTTP); time from `clock_now_ms` (det-deterministic presign, spec
  §9.3).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: re-derive one vector independently (python/openssl in the review
  shell); probe session-token (`X-Amz-Security-Token`) inclusion in both header + presign
  variants.

## Task B8: `std/blob` — client ops, list generator, multipart; stub-server tests

**Files:** `src/stdlib/blob.rs`, `src/value.rs`/`src/interp.rs` (BlobClient resource),
`tests/cap_audit.rs`.

- [ ] **Step 1 (failing tests):** an in-process `http_server`-based S3 stub (Rust-side test
  helper or `.as` run_source — prefer Rust-side for assertion power) that VERIFIES the
  `Authorization` sigv4 of every request against the known creds: (a) put→get→head→delete
  roundtrip (etag, contentType, `x-amz-meta-*` metadata); (b) `list` generator paginates
  (stub returns 2 pages via `NextContinuationToken`; assert lazy: page 2 fetched only when
  iteration crosses it); (c) range get; (d) multipart: create→3 parts→complete order +
  abort-on-part-failure (stub 500s part 2 → AbortMultipartUpload observed); (e) path-style vs
  virtual-host URL matrix + R2 `region:"auto"`; (f) S3 XML error body → `err.code/message/
  status`; (g) malformed XML response → clean Tier-1; (h) cap_audit: any blob op denied under
  `--deny net`, incl. `presign` (the whole-module decision, asserted + comment referencing
  spec §2.3).
- [ ] **Step 2:** implement per spec §9.2 over `shared_client()`;
  `ResourceState::BlobClient` (config only); list as a native-cursor generator (reuse B1's
  generator plumbing); multipart enforces the 5 MiB floor on non-final parts (Tier-2 on a
  too-small configured partSize, Tier-1 on runtime stream behavior).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: run; probe — key needing encoding (`a b/c+d.txt`), empty object
  put/get, `bucket` per-call override, generator `close()` mid-pagination, 0-page list.

## Task B9: Phase B examples + docs + NAV

**Files:** examples per File structure; `docs/content/stdlib/{archive,markup,email,blob}.md`,
`docs/assets/app.js`, `docs/content/stdlib/data.md` (cross-link), `README.md`,
`src/check/std_arity.rs`, `tests/vm_differential.rs`.

- [ ] **Step 1:** intro examples (all corpus members, four-mode verified):
  `archive_roundtrip.as` (tar+gz+zip in-memory, deterministic mode), `xml_basics.as`,
  `email_builder.as` (builder + validateAddress + a sanitize demo), `blob_basics.as`
  (in-script `http_server` sigv4-verifying stub + put/get/list — the stub verifying the
  signature in `.as` code is the showcase).
- [ ] **Step 2:** advanced examples: `feed_reader.as` (baked RSS string → xml → html.sanitize;
  corpus), `smtp_send.as` (in-script `net/tcp` SMTP sink task + `email.send` against it;
  corpus), `blob_sync.as` (multipart over the stub; corpus), `backup_tool.as` (fs walk →
  deterministic tar.gz → verify entries; corpus if output deterministic, else `EXAMPLE_SKIPS`
  + reason).
- [ ] **Step 3:** docs pages `stdlib/{archive,markup,email,blob}.md` (API tables in the
  house voice; the §6.5 hardening contract; the §7.3 allowlist verbatim; the §8 injection
  notes; the §9.3 sigv4/compat notes; per-unit recorded-futures from spec §17); NAV entries;
  README rows; arity rows; one `vm_differential` golden per module.
- [ ] **Step 4:** full suite + clippy both configs. Commit.
- [ ] Independent review: serve docs, click everything; run all examples four-mode; check
  `data.md` compress section cross-links archive.

## Task B10: Phase B holistic review + merge

- [ ] Holistic review (full phase diff): tiering audit, caps completeness test green,
  borrow-across-await scan, the four security batteries re-run and read (not just green —
  reviewed for coverage), hostile-input coverage spot-checks.
- [ ] Gates sweep (as A9: both configs, vm_differential, cap_audit, negative space, vm_bench
  geomean, RSS spot-check). Update goal-perf/roadmap. Merge `--no-ff`.

---

# Phase C — deterministic testing + property testing (`feat/batt-testing`)

> Independent of Phase B; may run on its own branch in parallel with B (both branch from
> post-A `main`; whichever merges second rebases trivially — no file overlap except
> `mod.rs`/docs).

## Task C1: core det seam + `ascript test --seed/--frozen-time`

**Files:** `src/interp.rs`, `src/lib.rs`, `src/main.rs`, `src/worker/testrun.rs`,
`src/test_filter.rs` (only if touched), `tests/cli.rs`.

- [ ] **Step 1 (failing tests):** CLI integration tests (`tests/cli.rs` style): (a) a test
  file printing `math.random()` + `time.now()` run twice with `--seed 42` → byte-identical
  outputs; different seeds → different; (b) `--frozen-time 2026-01-02T03:04:05Z` →
  `time.now()` inside a TEST prints exactly that epoch-ms; (c) module TOP-LEVEL `time.now()`
  is NOT frozen (the §10.2 decision, pinned); (d) two tests in one file get INDEPENDENT
  identical streams (test B's draw unaffected by whether test A ran — run with `--filter`
  excluding A, same B output); (e) `--parallel=2` with `--seed` → summary byte-identical to
  serial; (f) `--frozen-time garbage` → clean CLI error; (g) NO flags → output byte-identical
  to a pre-change binary (the INERT pin: a plain test run on this branch vs main, diffed in
  the review).
- [ ] **Step 2:** implement: lift `restore_determinism`/`take_determinism` cfg —
  rename-or-wrap into core `pub(crate) fn set_determinism(&self, Option<DeterminismContext>)
  -> Option<DeterminismContext>` (keep the workflow-named wrappers delegating, zero behavior
  change — re-grep all callers); `DetTestConfig { seed: u64, start_ms: f64 }` threaded:
  `main.rs` clap (`--seed <u64>`, `--frozen-time <string>` parsed via chrono RFC3339 **iff
  `datetime` is on**, else epoch-ms-only with a clear error — document; epoch-ms accepted
  always) → `run_tests_with_options(det: Option<DetTestConfig>)` →
  `run_registered_tests_filtered(det)`: per iteration
  `self.set_determinism(Some(DeterminismContext::record(seed, start_ms)))` before
  `call_value`, `set_determinism(None)` after (also on the error paths — use a scope guard
  pattern WITHOUT holding a borrow); parallel path: add the two scalars to the airlock struct
  in `worker/testrun.rs`; coverage path: same install around its per-file
  `run_registered_tests_filtered` call. Defaults per spec §10.1 (`--frozen-time` alone →
  seed 0; `--seed` alone → `det::deterministic_start_ms(seed)`).
- [ ] **Step 3:** green + clippy both configs (NOTE: the seam must work under
  `--no-default-features` — `DeterminismContext` is core; only the ISO parsing is
  datetime-gated). Commit.
- [ ] Independent review: run; probe — a test that itself starts a workflow (nested det
  context: workflow's save/restore must compose — the wrappers preserve this; pin with a
  test if `workflow` is on), `time.sleep` inside a seeded test (virtual → returns instantly,
  test suite faster — pin), `--seed` with snapshot tests (snapshots stable).

## Task C2: `std/test` — generator combinators (tagged Objects)

**Files:** `src/stdlib/test_mod.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** Rust unit tests for the native draw fn: for each
  combinator, seeded draws produce pinned values + respect bounds; edge bias is REAL
  (over 200 seeded draws of `gen.int(-1000, 1000)`, the boundary set {min, max, 0, ±1}
  appears with the designed boosted frequency — pin the mechanism, not exact counts);
  `gen.filter` exhausting `maxDiscard` → Tier-1 naming the generator; `gen.map` applies the
  user fn (a `call_value` path — async); invalid configs (`min > max`, negative lengths,
  unknown charset) → Tier-2; gens are inert tagged Objects printable/JSON-able.
- [ ] **Step 2:** implement: `gen.*` constructors return `{__gen: kind, ...}` Objects
  (schema posture); the native drawer
  `impl Interp { async fn draw_gen(&self, gen: &Value, rng: &mut det::SeededRng, depth_budget) -> Result<Value, Control> }`
  — recursion-budgeted (nested `arrayOf(objectWith(...))`), sizes drawn through the
  `want_count` discipline; edge bias per spec §10.4 (a 1-in-4 draw from the boundary pool —
  the fuzzgen philosophy, cited in the module doc). Register `std/test` (CORE — no feature
  gate; `KNOWN_UNGATED` + `STD_MODULES` + arity rows).
- [ ] **Step 3:** green + clippy both configs (CORE: must pass under
  `--no-default-features`). Commit.
- [ ] Independent review: probe — `gen.oneOf()` empty (Tier-2), deeply nested gens (budget,
  no stack risk — `stacker` not needed for data recursion bounded at ~32), unicode charset
  draws are valid scalars.

## Task C3: `prop()` runner + shrinking

**Files:** `src/stdlib/test_mod.rs`, `src/interp.rs` (registration), `src/stdlib/assert_mod.rs`
(failure suffix).

- [ ] **Step 1 (failing tests):** run_source-style: (a)
  `prop("commutes", [gen.int(), gen.int()], (a,b) => a+b == b+a)` registers + passes under
  `ascript test` (100 runs default); (b) a FALSE property (`(a,b) => a+b <= 100` over
  `gen.int(0, 1000)`) fails with a report containing `seed:`, the shrunken counterexample,
  and — the convergence pin — the shrunken values satisfy minimality (for `x <= 99` over
  `gen.int(0,1000)`: shrinks to exactly `100`); (c) object-form generators bind by name in
  the report; (d) a panicking body and a `?`-propagating body both count as failures with
  the message; (e) `opts.seed` reproduces the identical counterexample across two runs;
  CLI `--seed` does too; (f) `opts.runs/maxShrinks` budgets honored (instrument via a
  counting fn); (g) `--filter` matches prop names; `--parallel` summary identical; (h) a
  prop inside `--frozen-time` sees the frozen clock.
- [ ] **Step 2:** implement: `prop(name, gens, fn, opts?)` registers a closure into the SAME
  `self.tests` table — the closure is a `Value::Builtin`-style native thunk (find the
  precedent for registering a NATIVE callable as a test; if none, register via a small
  native-fn `Value` the runner dispatches — keep it a `Value` so the table type is
  unchanged); the runner: per-iteration RNG = `SeededRng::new(seed_for(base_seed, i))`
  (document the derivation); failure → shrink per spec §10.5 (greedy loop, per-candidate
  re-run, `maxShrinks` cap) → raise a Tier-2 panic whose message is the formatted report
  (so the existing summary machinery prints it verbatim); seed precedence
  `opts.seed > CLI > random` (random printed in the report). Failure-line suffix
  `(seed: N[, frozen-time: T])` appended in the summary formatting when a det/prop seed was
  in play, plus the replay invocation line (spec §10.3).
- [ ] **Step 3:** green + clippy both configs. Commit.
- [ ] Independent review: run; probe — a property over `gen.arrayOf(gen.int())` asserting
  sortedness of `arr.sort()` (a REAL property catching nothing = sanity), shrink of a
  string property (`!s.contains("ab")` → shrinks to `"ab"`), `maxShrinks: 0` (reports the
  ORIGINAL counterexample), prop name collision with a plain test (both run; names
  reported distinctly).

## Task C4: Phase C docs + examples + holistic + merge

**Files:** `examples/property_testing.as`, `examples/advanced/prop_roundtrips.as`,
`docs/content/stdlib/assert.md`, `docs/content/cli.md`, `docs/assets/app.js` (NAV title),
`README.md`, `src/check/std_arity.rs`, `tests/vm_differential.rs`.

- [ ] **Step 1:** examples (explicit seeds → corpus members, four-mode):
  `property_testing.as` (two passing props + gen tour); `advanced/prop_roundtrips.as`
  (encode/decode laws: `encoding.base64` roundtrip, `json.parse(json.stringify(x))` over a
  generated object shape).
- [ ] **Step 2:** docs: `stdlib/assert.md` gains "Deterministic test runs" (`--seed`,
  `--frozen-time`, what is/isn't frozen — the §10.2 module-load note) + "Property testing"
  (gen table, shrinking strategy §10.5 documented honestly, seed precedence, replay recipe);
  NAV title → "Testing & assertions"; `cli.md` flags; README row for `std/test`.
- [ ] **Step 3:** full suite + clippy both; vm_differential.
- [ ] Holistic review (phase diff): INERT-by-default re-verified (no-flag runs byte-identical
  vs main — actually diff outputs), det+prop+filter+parallel+coverage interplay matrix run.
- [ ] Gates sweep as A9; update goal-perf/roadmap; merge `--no-ff`.

---

# Phase D — the T2 toolbelt (`feat/batt-t2`, after B merges)

## Task D0: branch + features

- [ ] `git checkout -b feat/batt-t2 main` (B + C merged). Features: `cron = ["datetime"]`,
  `semver = []`, `markdown = ["xml", "dep:pulldown-cmark"]`, `diff = []`; dep
  `pulldown-cmark = { version = "0.13", default-features = false, optional = true }` (verify
  current version; enable only the needed extension features). All into `default`. Build both
  configs. Commit.

## Task D1: `std/cron`

**Files:** `src/stdlib/cron.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) parse matrix — every field form (`*`, lists, ranges,
  `*/5`, `10-30/5`, names `jan`/`SUN`, dow 7≡0) + Tier-1 errors (6 fields, out-of-range,
  `*/0`, garbage, `@reboot` Tier-2 per spec §11.2 — it's a programmer-literal misuse);
  (b) `next` known-vector table: `0 0 * * *` from mid-day; minute rollover; month rollover;
  `0 0 31 * *` skips short months; leap-day `0 0 29 2 *` (next from 2026 → 2028);
  **the DOM/DOW OR rule** (`0 0 13 * 5` matches the 13th AND every Friday — both directions
  pinned); `*/15` offsets; `tzOffset: -300` shifts the civil interpretation; (c) `next` with
  no match in 5 years (`0 0 30 2 *`) → Tier-1; (d) `nextN`/`matches`; (e) `schedule` under a
  det context: install a frozen clock, schedule `*/1 * * * *` (every minute), assert the fire
  count advances with virtual time and `stop()` halts (reuse the C1 seam in a Rust test —
  cron tests run with `datetime` on).
- [ ] **Step 2:** implement per spec §11 — parse into bitmask sets
  (`minutes: u64, hours: u32, dom: u32, months: u16, dow: u8` + the
  `dom_restricted`/`dow_restricted` flags for the OR rule); `next` minute-scan with
  month/day skip acceleration, 5-year bound; civil-time math via chrono UTC + offset
  shifting; `schedule` = a spawned task looping `next` → `time.sleep(delta)` (through
  `call_time` so the det seam applies BY CONSTRUCTION — note in the module doc) → `call_value`;
  handle = `ResourceState::CronJob` (abort-on-drop via the task handle, the
  cancel-on-drop discipline; `stop` graceful flag, `close` aborts). Register (ungated;
  `KNOWN_UNGATED`; arity rows).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: cross-check 5 vectors against a reference cron calculator; probe —
  `59 23 31 12 *` year rollover, schedule callback that panics (job survives? pin: log +
  continue, matching the server handler rule), `handle` dropped without close (no zombie —
  abort-on-drop).

## Task D2: `std/semver`

**Files:** `src/stdlib/semver.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) parse matrix (core, prerelease, build, `v` prefix
  REJECTED per SemVer — pin; leading zeros rejected; numeric vs alphanumeric prerelease ids);
  (b) the SemVer 2.0.0 §11 precedence ladder verbatim
  (`1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-alpha.beta < … < 1.0.0`); (c) `satisfies` table per
  spec §12: caret incl. `^0.2.3`/`^0.0.3`; tilde; x-ranges; partials; hyphen ranges incl.
  partial ends (`1.2.3 - 2` → `<3.0.0`); `||` / space-AND; **the prerelease participation
  rule** (`1.2.3-alpha` satisfies `>=1.2.3-0` but NOT `>=1.2.0`); (d) `maxSatisfying`;
  (e) tiering: malformed VERSION to `compare` → Tier-2; malformed RANGE to `satisfies` →
  Tier-1 (spec §12, pinned).
- [ ] **Step 2:** implement (hand-rolled per the §3 verdict; range → DNF of comparator sets;
  document the node-semver-subset contract verbatim in the module doc). Register (ungated).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: cross-check 10 satisfies rows against `npx semver` outputs (run it,
  paste evidence); probe `1.0.0+build` equality (`build` ignored in compare — pin).

## Task D3: `std/markdown`

**Files:** `src/stdlib/markdown.rs` (new), `src/stdlib/mod.rs`.

- [ ] **Step 1 (failing tests):** (a) CommonMark spot vectors (headings, emphasis, links,
  fenced code with info string → `class="language-x"`, lists, blockquotes, html-block
  passthrough INTO the sanitizer); (b) **sanitize-by-default pin**: `<script>alert(1)</script>`
  and `[x](javascript:alert(1))` in markdown → inert output (script escaped, href dropped);
  `{sanitize: false}` emits raw (and the docs warning exists); (c) extension toggles
  (tables/strikethrough/taskLists on by default per spec §13, footnotes off); (d) `allow`
  forwarded to the sanitizer; (e) `markdown.escape` table.
- [ ] **Step 2:** implement over pulldown-cmark `Parser` + `push_html`, then pipe through
  `html::sanitize_with(opts)` (expose the Rust-side entry from `html.rs`). Register
  (ungated; feature `markdown`).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: probe — a markdown link with an entity-obfuscated scheme (battery
  vector through the full pipeline), nested raw-HTML table smuggling an `onclick`, very long
  input (bounded, linear).

## Task D4: `std/diff` + snapshot wiring

**Files:** `src/stdlib/diff.rs` (new), `src/stdlib/mod.rs`, `src/stdlib/assert_mod.rs`.

- [ ] **Step 1 (failing tests):** (a) `diff.lines` hunk pins (identical → one equal hunk;
  pure-insert; pure-delete; interleaved); (b) `diff.unified` byte-pins against `diff -u`
  outputs for 4 fixture pairs (headers `--- a`/`+++ b`, `@@` ranges, context merging at
  `context: 3`, the no-trailing-newline `\ No newline at end of file` marker); (c)
  determinism pin (same inputs twice → identical); (d) budget breach → Tier-1; (e)
  `diff.chars`; (f) snapshot wiring: a failing `assert.snapshot` message now contains a
  `@@` unified hunk of stored→new (update the existing message-pinning tests in the same
  commit — Gate: both feature configs, and under `--no-default-features`+`diff`-off the OLD
  dump format persists — pin both).
- [ ] **Step 2:** implement Myers O(ND) over line slices (hand-rolled per §3; intern lines
  as indices first; the D-path trace bounded by the budget); unified renderer; `chars` over
  the same core with a char-vec adapter. Wire `snapshot_impl` per spec §14.3
  (`#[cfg(feature = "diff")]` replaces the raw-dump section, 200-line truncation). Register
  (ungated).
- [ ] **Step 3:** green + clippy both. Commit.
- [ ] Independent review: byte-compare two unified outputs against GNU `diff -u` (run it);
  probe — 10k-line inputs (performance sane), CRLF inputs (split rule pinned), empty-string
  vs single-newline distinction.

## Task D5: Phase D examples + docs

**Files:** `examples/{cron_next,semver_ranges,markdown_render,diff_unified}.as`,
`examples/advanced/{job_scheduler,docs_site_gen}.as`, `docs/content/stdlib/{time,utilities,
markup}.md`, `docs/assets/app.js` (no new NAV pages — verify), `README.md`,
`src/check/std_arity.rs`, `tests/vm_differential.rs`.

- [ ] **Step 1:** intro examples (corpus, four-mode): `cron_next.as` (pure `next` with fixed
  `after` — deterministic), `semver_ranges.as`, `markdown_render.as` (incl. the sanitize
  demo), `diff_unified.as`.
- [ ] **Step 2:** advanced: `job_scheduler.as` (cron.schedule + stop, bounded run — corpus if
  timing-deterministic via fixed `after`+counts, else documented skip);
  `docs_site_gen.as` (markdown + archive + fs: render a baked page set → deterministic
  tar — corpus).
- [ ] **Step 3:** docs sections: cron → `stdlib/time.md` (incl. the DOM/DOW OR rule, the
  fixed-offset honesty + shared chrono-tz recorded-future, det interplay); semver + diff →
  `stdlib/utilities.md` (the precise range-subset contract; the unified-format note + the
  snapshot tie-in); markdown → `stdlib/markup.md` section (subset honesty + sanitize
  default). README rows; arity rows; one golden each.
- [ ] **Step 4:** full suite + clippy both configs. Commit.
- [ ] Independent review: serve docs; run examples four-mode.

## Task D6: campaign finish — meta-docs, final gates, holistic, merge

**Files:** `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`,
`docs/content/stdlib/overview.md`.

- [ ] **Step 1:** `CLAUDE.md`: feature-flag paragraph gains the 10 new features; a terse
  BATT subsection in "Larger subsystems" (module list, the per-func caps pattern + `PER_FUNC`
  completeness extension, typed-jwt-keys gotcha, zip-slip contract, sanitizer fail-closed
  model, `std/test` tagged-gen posture, det-test seam, snapshot-diff wiring). Stdlib overview
  page + README table complete. `superpowers/roadmap.md` BATT record; `goal-perf.md` BATT →
  ✅ with the per-phase merge SHAs.
- [ ] **Step 2:** FINAL gates sweep (spec §16, evidence pasted into the PR):
  - [ ] `cargo test` AND `cargo test --no-default-features` — full, green.
  - [ ] `cargo clippy --all-targets` AND `--no-default-features --all-targets` — clean.
  - [ ] `cargo test --test vm_differential` both configs — four-mode byte-identical, new
    examples in the corpus, goldens green.
  - [ ] `cargo test --test cap_audit` — all rows incl. every new gated fn; the completeness
    test green with `PER_FUNC`.
  - [ ] `tests/batt_negative_space.rs` — ASO 27 / Value 24 / no new Op / no new tag.
  - [ ] All six security batteries green AND reviewed for coverage (not just passing).
  - [ ] Gate 17: vm_bench spec/tw geomean ≥2×, same-session A/B vs main (paste numbers).
  - [ ] Gate 18: peak RSS on the corpus vs main (`/usr/bin/time -l`), no regression.
  - [ ] Docs: every new module on a NAV-reachable page; served-site click-through; README;
    `cli.md` flags; DOCS claiming table updated IF it exists in-tree by now (check).
  - [ ] fmt idempotence over all new examples (`ascript fmt` twice, no diff).
  - [ ] LSP smoke: completion on `import "std/jwt"` members resolves (the exports path).
- [ ] **Step 3:** Final holistic review subagent over the whole-campaign surface (all four
  phase merges): cross-unit consistency (error wording, option naming `camelCase`, Tier
  choices), spec §17 deferral list matches reality (no silent drops), every spec section
  either implemented or in §17.
- [ ] Merge `--no-ff`. Done.
