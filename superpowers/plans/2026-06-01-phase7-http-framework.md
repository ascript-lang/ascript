# Phase 7 — HTTP Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Verb methods + schema-validated handlers on the existing `std/http/server` (router/middleware/params already exist). Full spec: `docs/superpowers/specs/2026-06-01-phase7-http-framework-design.md`.

**Architecture:** The server is a native handle (`create()`) with methods `use`/`route`/`serve` dispatched in `src/stdlib/http_server.rs`. Add verb methods (sugar over `route`) and an optional body-schema on routes (validated via Phase-6 `self.parse_value`, reuse `crate::stdlib::schema::{schema_kind, ParseFail}` — both `pub(crate)`). Validation runs inside the existing async per-connection handler task (borrow-safe; a validation Mismatch → 400 response, not a panic). No new syntax. `net` feature gated (server is net).

**Conventions:** register methods in the server's method dispatch; reuse the in-process loopback test harness already in http_server.rs (spins a real server with `maxRequests:N` + a `client_request` helper); clippy clean both configs; RUN both test configs; docs+README+example.

Sub-phases: 7a verb methods → 7b typed routes → 7c integration.

---

## Sub-phase 7a: HTTP verb methods

**Files:** `src/stdlib/http_server.rs` (server method dispatch), tests.

- [ ] **Step 1 — failing tests** (reuse the existing http_server in-process test harness — find the test that does `create()` + `s.route(...)` + `client_request`): register routes via `s.get("/x", h)`, `s.post("/y", h)`, `s.put`, `s.patch`, `s.delete`, `s.head`, `s.options`; assert each dispatches like `route(VERB, ...)`. FIRST run a quick `.as` check that `s.delete(...)` parses (delete may be a keyword-ish — if `s.delete` is a parse error, expose as `del` and note; verify).
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** in the server's method dispatch (where `use`/`route`/`serve` are handled), add `get/post/put/patch/delete/head/options` arms, each calling the same route-registration path as `route` with the fixed method string. (If `delete` is parse-blocked, also/instead expose `del`.)
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy.
- [ ] **Step 5 — commit:** `feat(http): server verb methods (get/post/put/patch/delete/head/options)`

---

## Sub-phase 7b: Schema-validated (typed) routes

**Files:** `src/stdlib/http_server.rs` (route storage carries an optional schema; handler dispatch validates body), tests.

- [ ] **Step 1 — failing tests** (in-process harness): `s.post("/users", schema.object({name: schema.string(), age: schema.number()}), handler)` (import schema in the test source):
  - valid JSON body `{"name":"a","age":30}` → handler runs, returns 200, and (assert) the handler saw the validated object as `req.body`.
  - bad-shape JSON body `{"name":"a","age":"x"}` → 400, response JSON body contains the path (`age`) + a message, handler NOT called (assert via a side effect the handler would have caused).
  - malformed JSON body → 400.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - Route storage: allow a route entry to carry an optional `schema: Value`. The verb methods (and `route`) detect a 3-arg form `(path, schema, handler)` where the middle arg is a tagged schema (`schema::schema_kind(&v).is_some()`); store it.
  - In the per-connection handler dispatch, BEFORE calling the matched route handler: if the route has a body schema, parse the body (if content-type is JSON → `json` decode the body string; else use the raw string), run `self.parse_value(&schema, &value, "").await`:
    - `Ok(validated)` → set `req.body` = validated (keep raw at `req.rawBody`), proceed to handler.
    - `Err(ParseFail::Mismatch(errObj))` → build a 400 response: status 400, JSON body `{error:"validation failed", path: errObj.path, message: errObj.message}` (extract path/message from the errObj Object), DO NOT call the handler.
    - `Err(ParseFail::InvalidSchema(_) | ParseFail::Control(_))` → let it become a 500 via the existing handler-panic isolation (or map to 500 explicitly). Document.
  - Borrow-safe: clone the schema + body out before the `.await` (the handler task already follows this discipline).
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(http): schema-validated routes (body validation -> 400 on failure)`

---

## Sub-phase 7c: Integration

- [ ] `examples/typed_api.as`: `create()` + a logging middleware + `server.get("/users/:id", ...)` (path param) + `server.post("/users", userSchema, ...)` (typed create: 400 on bad body, 201 on success) + `server.serve({maxRequests:N})` so it TERMINATES (drive it with a few in-process or documented external requests; OR structure like the existing http_server.as but ensure the example process terminates — use maxRequests). Run it; confirm it terminates + prints expected output.
- [ ] Docs: extend the `std/http/server` doc page (`docs/content/stdlib/*`) with the verb methods + the typed-route form (schema arg, 400 behavior with {path,message}, validated `req.body`/`req.rawBody`); README mention.
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: verb methods dispatch correctly; schema-route detection doesn't break plain 2-arg routes or middleware; validation 400 is correct + handler-not-called on failure; no borrow-across-await in the validation hook; reuse of Phase-6 parse_value correct; no regression to existing server behavior incl middleware/params/maxRequests/limits; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 7b route-arg detection (3-arg typed form vs 2-arg plain form vs the existing `route(method,path,handler)`) must be unambiguous (schema via `__kind`), and the validation hook must run at the right point (after route match + params, before handler, inside the async task, borrow-safe).
- Reuse `schema::parse_value` + `schema_kind` (pub(crate)); confirm the body-as-JSON decode reuses `json` decoding.
- No new syntax → conformance unchanged. Verify `delete` method name is reachable from script.
