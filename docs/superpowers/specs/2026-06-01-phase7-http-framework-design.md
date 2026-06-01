# Phase 7 — HTTP Framework Design (router ergonomics + typed handlers)

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal.
- **Roadmap:** Phase 7 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Reality check (scope revision)

The roadmap framed Phase 7 as "router (path params) + middleware chain + typed handlers."
Inspection of `src/stdlib/http_server.rs` shows the **router and middleware already exist**:
`create()` returns a server with `server.use(middleware)` (chain with `next(req)`),
`server.route(method, path, handler)`, route params (`/users/:id` → `req.params`), query,
headers, body, and structured responses (`string` | `{status,headers,body}` | `[v,err]`).
The advanced example already demonstrates middleware + auth + params.

So Phase 7's genuinely-new value is the **ergonomics + typed-handler** layer on top:
1. **HTTP verb methods** — `server.get/post/put/patch/delete/head/options(path, handler)` as
   sugar over `server.route(METHOD, path, handler)`.
2. **Schema-validated (typed) handlers** — attach a Phase-6 `std/schema` to a route so the
   request body is parsed+validated before the handler runs; on failure auto-respond **400**
   with the structured `{path, message}` error; on success the handler receives the
   **validated** body. This is the "typed handlers using the validation library" deliverable.

This is a focused, mostly-additive phase touching `http_server.rs` (verb methods + a schema
hook in route dispatch) and reusing Phase 6's `parse_value`. No new language syntax.

## Sub-phases
- **7a — Verb methods:** `get/post/put/patch/delete/head/options` on the server object (sugar
  over `route`).
- **7b — Typed routes:** verb methods accept an optional schema; body validated via Phase 6;
  400-with-structured-error on failure; validated body handed to the handler.
- **7c — Integration:** typed-API example + docs + README + merge.

Conventions: native (the server is a native handle with methods); reuse the existing route
storage + dispatch; the schema validation runs inside the per-connection handler task (already
async, borrow-safe); a validation failure produces a normal 400 response (NOT a panic); clippy
clean both configs; RUN both test configs; docs+README+example.

---

## 7a — HTTP verb methods

Add methods on the server handle (alongside `use`/`route`/`serve`):
- `server.get(path, handler)`, `server.post(...)`, `server.put(...)`, `server.patch(...)`,
  `server.delete(...)`, `server.head(...)`, `server.options(...)`.
- Each is exactly `server.route("<VERB>", path, handler)` — store into the same route table.
- (`delete` is fine as a method name — it's not a parse-blocking keyword in method position; if
  it IS blocked like `nil`/`enum` were, expose it as `del` and document — verify by running a
  `.as` script that calls `server.delete(...)`.)

### Tests (7a)
Register routes via `server.get`/`server.post`/etc. and confirm they dispatch identically to
`server.route` (reuse the existing in-process server test harness in http_server.rs — it spins a
real loopback server with `maxRequests:N` and a client_request helper). e.g. `server.get("/x", h)`
then a GET /x returns the handler's response; a POST to a GET-only route → 404/405 per existing
behavior.

---

## 7b — Schema-validated (typed) handlers

Extend the verb methods (and optionally `route`) to accept an **optional schema** before the
handler: `server.post(path, schema, handler)` (3-arg form). Detection: if the arg before the
handler is a tagged-`__kind` schema Object (the Phase-6 representation; reuse
`schema::schema_kind`), it's a body schema.

Behavior when a route has a body schema:
- Parse the request body: if `content-type` is JSON, `json.parse` the body; otherwise treat the
  raw body string as the value (document; JSON is the common case). 
- Run Phase 6 `self.parse_value(schema, value, "")`:
  - **Mismatch** → short-circuit with a **400** response, JSON body
    `{ error: "validation failed", path, message }` (don't call the handler).
  - **Ok(validated)** → set `req.body` to the **validated/coerced** value (and keep the raw
    string available as `req.rawBody`) and call the handler.
  - **InvalidSchema/Control(refine panic)** → a 500 (the handler-panic→500 isolation already
    exists; a malformed schema is a programmer error surfacing as 500, consistent with the
    server's panic isolation). Document.
- No schema on a route → unchanged (body stays the raw string).

This wires the standout validator into the web layer: a typed endpoint validates its input and
returns clean 400s automatically.

### Tests (7b)
A `server.post("/users", schema.object({name: schema.string(), age: schema.number()}), handler)`:
- valid JSON body → handler runs, `req.body` is the validated object, 200.
- invalid JSON body (bad shape) → 400 with a JSON body containing the field path + message, handler NOT called.
- malformed JSON body → 400 (or the existing bad-body handling) — fused.
(Use the in-process loopback test harness with `client_request`.)

---

## 7c — Integration

- `examples/typed_api.as`: a small JSON API using the verb methods + a schema-validated POST
  (create-user with validation → 400 on bad input, 201 on success) + a GET with a path param +
  middleware. Run it via the in-process `maxRequests:N` pattern OR document the two-terminal run
  like the existing http_server example. Must terminate (use `maxRequests`).
- Docs: extend the `std/http/server` doc page with the verb methods + the schema-validated route
  form (request validation, 400 behavior, validated `req.body`); README mention.
- Full gates (both test configs, clippy both, fmt, conformance, idempotence); holistic review;
  merge `--no-ff`.

## Decisions (made; flagged)
1. Router/middleware/params already exist — Phase 7 adds verb-method sugar + typed handlers
   only (no rebuild). **Settled.**
2. Verb methods are thin wrappers over `route`. **Settled.**
3. Typed routes: optional schema arg before the handler (detected via `__kind`); body validated;
   Mismatch→400 with `{path,message}`; validated body → `req.body` (raw kept as `req.rawBody`);
   malformed-schema/refine-panic → 500 (existing isolation). **Settled.**
4. No new language syntax. **Settled.**

## Open implementation choices (decide during impl, document)
- Whether to validate query/params too (not just body) — keep to BODY for v1 (most common);
  query/param schemas are a documented future extension (don't silently skip — just scope to
  body and say so).
- `req.body` replaced-with-validated vs a new `req.valid` field — prefer replacing `req.body`
  with the validated value and exposing `req.rawBody` for the original; document.
- The `delete` method-name reachability (keyword check) — verify; fall back to `del` if blocked.
