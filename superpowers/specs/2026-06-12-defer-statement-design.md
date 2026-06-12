# AScript `defer` Statement — Design (DEFER)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** DEFER (the Language surface track of `goal-perf.md` — **the campaign's ONE grammar
  change**)
- **Depends on:** nothing in the PERF campaign (builds on shipped `main`). **Sequencing
  constraint (goal-perf, restated):** DEFER touches the same frame return/unwind paths
  LANE/CALL/DECODE rework — it lands **before LANE starts or after the engine waves merge**
  (owner call, recorded in the plan's Phase 0), never concurrently.
- **Depended on by:** nothing (LANE/CALL rebase over its unwind hooks if it lands first).
- **Engines:** both — tree-walker (oracle) and VM (specialized + generic + `.aso`), byte-identical
  across all four modes over the new defer corpus and the whole existing corpus.
- **Breaking:** **yes, narrowly** — `defer` becomes a **reserved keyword** (§2.2; grep-verified
  zero collisions in stdlib exports, `examples/**`, `docs/content/**`, `tests/**`). Pre-1.0
  breaking is sanctioned (`goal.md`).
- **`.aso`:** **bumps `ASO_FORMAT_VERSION` 27 → 28** (two new opcodes, §5.4). Verifier, disasm,
  and `bcanalysis` updated in the same PR.
- **Owner amendment (2026-06-12, folded in):** `defer await <call>` is a **first-class form**
  (§3.4); a bare `defer` of a future-returning call is a loud Tier-2 error whose message directs
  the user to `defer await`.

---

## 0. Read this first — what `defer` is and is not

`defer <call>` registers a call to run when the **enclosing function body exits — by any route**:
normal completion, `return`, `?`-propagation, or panic unwind. Deferred calls run **LIFO**,
**arguments and callee are evaluated at the `defer` statement** (Go semantics), and the deferred
call **cannot observe or modify the return value**. It closes the recurring real-world gap the
roadmap names: a `?` early-exit silently skips the manual `close()` below it.

```as
fn copy(srcPath: string, dstPath: string) {
    let [src, err] = fs.open(srcPath)
    if (err != nil) { return [nil, err] }
    defer src.close()                      // runs on EVERY exit below, incl. the `?`s

    let [dst, derr] = fs.create(dstPath)
    if (derr != nil) { return [nil, derr] }   // src.close() runs here
    defer dst.close()                          // LIFO: dst closes before src

    let n = io.copy(dst, src)?                 // a propagate runs BOTH defers
    return [n, nil]
}
```

What it is **not**: a finalizer (no GC interaction), a `try/finally` (no block scoping —
function-scoped only, §3.2), and **not a cancellation hook** — a cancelled task's defers do
**not** run (§4.2, the one loudly-documented semantic edge, with the soundness argument).

## 1. Summary & motivation

- **The gap:** AScript's error model (`?` propagation + recoverable panics) creates early frame
  exits that manual cleanup cannot see. Every production-shaped example today either nests
  `if (err != nil)` ladders around cleanup or leaks the handle on the error path. Native handles
  have deterministic `Drop` (the `ResourceState` rule) as a backstop, but `Drop` timing is
  refcount timing, not scope timing — and script-level cleanup (flush-then-log, unlock, metrics)
  has no backstop at all.
- **The shape:** Go's `defer`, the most battle-tested design in this family: statement, call-only,
  args evaluated at defer time, per-function LIFO stack, runs on every exit including panic
  unwind. We deviate from Go only where AScript's semantics force it (no named result params →
  no return-value mutation, §3.7; explicit `defer await` for async cleanup, §3.4; cancellation,
  §4.2) and each deviation is stated with its reason.
- **The tax:** this is the campaign's one grammar change, so the FULL `CLAUDE.md` "Touching
  syntax" checklist applies: both hand parsers + tree-sitter grammar + regen `--abi 14` +
  `sync-grammar.sh` publish + editor-pin bumps; exhaustive `Stmt` matches in `interp.rs`,
  `fmt.rs`, `ast.rs` (compile-enforced); both engines byte-identical; `.aso` bump + `verify.rs`;
  formatter/LSP/REPL/checker/fuzzer parity.

## 2. Surface syntax

### 2.1 The statement: `defer [await] <call>` — call-only, enforced at parse time

```
defer_stmt := 'defer' 'await'? call_expr
call_expr  := <any expression whose outermost node is a call>   // ExprKind::Call
```

Accepted: `defer f()`, `defer obj.close()`, `defer a?.flush()`, `defer (cond ? f : g)()`,
`defer (() => { … })()` (the inline-block idiom), `defer f(...xs)` (spread args),
`defer await teardown()`. **Rejected with a parse error** (`defer requires a call — only a call
expression can be deferred`): `defer x`, `defer a + b`, `defer f` (no call), `defer f()?`
(`?` wraps the call), `defer f()!`, `defer yield v`. The single sanctioned wrapper is `await`
as part of the statement form (§3.4) — `defer await f()` is the statement-level form, not a
general `Await` expression in defer position.

**Why call-only (Go's restriction, adopted):** an arbitrary deferred expression with no call has
no effect — `defer x + 1` computes a value nobody reads. That is a silent no-op footgun, and a
lint alone is weaker than a parse error: the lint fires after the bug shipped, the parse error
prevents it from existing. Anything expressible as an expression is expressible as a call via the
arrow-IIFE form, so no power is lost. Both hand parsers enforce this **structurally** (parse an
expression, then require `ExprKind::Call` — or `Await(Call)` for the `await` form); the
tree-sitter grammar encodes it directly (`seq('defer', optional('await'),
field('call', $.call_expression))`, §2.4).

**Named-argument calls are a v1 Tier-1 parse error** (`defer does not support named-argument
calls`). `CallArg::Named` (`src/ast.rs:142`) exists solely for ADT variant construction
(`Rect(w: 1, h: 2)`) — constructing a discarded variant under `defer` has no cleanup use, and
supporting it would force a third/fourth opcode pair mirroring `CallNamed`/`CallNamedSpread`'s
lockstep builder for zero corpus demand. Documented, tested, recorded as a v2 follow-up if demand
appears; the arrow-IIFE form covers any genuine need today. (Gate 6: a documented Tier-1 error,
never a silent drop.)

### 2.2 `defer` is a RESERVED keyword — decided against contextual, with reasons

The brief leaned contextual ("the `step` precedent"). **Verified against the code, contextual
does not survive contact with this statement's follower set**, and the repo's own precedent for
this situation is reservation:

1. **The follower set is "any expression", and it collides.** `step`, `worker`, `as`, `extends`
   are contextual because their follower tokens are disjoint from identifier-continuation tokens
   (`step` follows a complete range expression where no identifier can continue,
   `src/parser.rs:1749`; `worker` requires `fn`/`async`/`class` next, `src/parser.rs:106`,
   `src/syntax/parser.rs:322`). A statement-leading `defer` must instead be followed by an
   arbitrary call expression — whose first token can be `(`. So `defer(x)` is **genuinely
   ambiguous**: a call to a user function named `defer`, or a defer statement of the non-call
   `(x)`. Worse, the recommended inline-cleanup idiom `defer (() => { … })()` lives **exactly**
   in that ambiguity. Any contextual rule either breaks statement-leading calls to a function
   named `defer` *silently* (re-parsing them as defer statements that then fail the call-only
   check) or breaks the IIFE idiom. A silent meaning change is the worst outcome on the table.
2. **The in-repo precedent for a broad-follower keyword is reservation.** `interface` was
   RESERVED (`Tok::Interface`, `src/lexer.rs:601`; `InterfaceKw`, `src/syntax/lexer.rs:416`) for
   exactly this reason while `extends`/`implements` stayed contextual. `defer` is the same case.
3. **The blast radius is measured: zero.** `grep -rn '\bdefer\b'` over `examples/**`,
   `docs/content/**`, `tests/**` finds nothing; over `src/**` only prose comments
   (caps.rs/run.rs/types.rs doc text). No stdlib module exports a `defer` binding. Pre-1.0
   breaking is sanctioned and here breaks nobody.
4. **Go, Swift, and Zig all reserve it.** Users coming from any of them expect a keyword.
5. **Tree-sitter agrees for free:** a literal `'defer'` in the grammar plus the `word` token
   triggers tree-sitter's keyword extraction, which reserves it in the generated parser too —
   so all three front-ends reject `let defer = 5` identically (conformance-testable), instead
   of the hand parsers rejecting what tree-sitter accepts.

Mechanically: `Tok::Defer` in the legacy lexer keyword table (`src/lexer.rs:589` block),
`SyntaxKind::DeferKw` in the CST lexer keyword table (`src/syntax/lexer.rs:416` block), a
`'defer'` literal in `grammar.js`. `let defer = 5` / `fn defer() {}` become parse errors in all
three front-ends.

### 2.3 Where `defer` is legal — every statement position; the scope is the FUNCTION

`defer` parses anywhere a statement parses: function/method/`init` bodies, generator bodies,
arrow-block bodies, `if`/`while`/`for` bodies, bare blocks, **and module top level**. The defer
stack it pushes onto is always the **enclosing function activation** (the innermost `fn` /
`async fn` / `fn*` / method / arrow body), NOT the lexical block:

- A defer inside `if (cond) { defer f() }` runs at **function** exit, not block exit (Go
  semantics; a block-scoped variant is rejected in §11).
- A defer inside a loop body pushes **one entry per iteration** — they all run at function exit,
  LIFO. This accumulation is legal (it is sometimes exactly what you want) and gets the
  `defer-in-loop` Warning lint (§6.1), the Go-vet precedent.
- **Top level:** the module/script body is a body like any other. Top-level defers run when the
  **module body completes** — at program end for the entry script (before the process exits with
  the program's result), at import time for an imported module (the module body runs to
  completion during `import`; its defers run before the exports are read). In the REPL, each
  submission is a program: a top-level `defer` in a REPL line runs at the end of **that
  submission**. All three are documented in the user docs.
- A `defer` inside a closure/arrow body belongs to **that closure's** activation: it runs when
  the closure call returns, not when the defining function exits.

### 2.4 Both parsers + the tree-sitter grammar

- **Legacy parser** (`src/parser.rs:96` `statement()`): a `Tok::Defer` arm parses
  `defer ['await'] expr`, validates the call shape (§2.1), and produces the new
  `Stmt::Defer { call: Expr, awaited: bool, span: Span }` (the stored `call` is guaranteed
  `ExprKind::Call` by the parser; `awaited` records the statement-form `await`).
- **CST parser** (`src/syntax/parser.rs:268` `stmt()`): a `DeferKw` arm produces a `DeferStmt`
  node (new `SyntaxKind::DeferStmt`, registered in `kind.rs`; typed-AST node added to
  `src/syntax/ast/ascript.ungram` + `mod.rs`), with the same structural validation and the same
  error messages (the SP1 ±1-column caret tolerance applies as everywhere). The resolver
  (`src/syntax/resolve/mod.rs`) walks `DeferStmt` exactly like `ExprStmt` (resolve the call
  expression; no new binding semantics — closures in defer args follow the normal
  capture-by-value finalization rules).
- **Tree-sitter** (`tree-sitter-ascript/grammar.js`): add to the `_statement` choice
  (`grammar.js:173`):

  ```js
  defer_statement: $ => seq(
    'defer',
    optional('await'),
    field('call', $.call_expression),
    optional(';'),
  ),
  ```

  `call_expression` (`grammar.js:672`) already covers plain calls, member calls, parenthesized
  callees, and explicit type-args. No GLR conflict is expected (the keyword is reserved; the
  follower is a single nonterminal) — if `optional('await')` interacts with `await_expression`,
  resolve with a conflict declaration the way `?`/ternary did, never by changing the hand
  parsers. Then **regenerate `parser.c` with `tree-sitter generate --abi 14`**, and at the merge
  wave run `./scripts/sync-grammar.sh` + bump the pins in `editors/zed/extension.toml` and
  `editors/nvim/lua/ascript/treesitter.lua` (one publish per merge wave, the standing rule).
  `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs` gain `both_accept` /
  both-reject catalog entries for every accepted/rejected form in §2.1 **plus** the reservation
  itself (`let defer = 5` rejected by all three front-ends).

## 3. Semantics — precise and edge-complete

### 3.1 Evaluation timing: callee and arguments bind AT the `defer` statement

Executing a `defer` statement evaluates, **immediately, in source order**, everything needed to
make the call later — only the *call itself* is deferred (Go semantics; the alternative,
re-evaluating at exit, makes `defer f(x)` observe mutations and is rejected in §11):

- **Plain callee** (`defer f()`, `defer (cond ? a : b)()`): the callee expression is evaluated
  to a value now; arguments are evaluated to values now (spread `...xs` is **materialized
  now** — the entry stores a flat argument vector, so later mutation of `xs` is invisible).
  Entry shape: `DeferEntry::Call { callee: Value, args: Vec<Value> }`.
- **Member callee** (`defer obj.m(a)`): the **receiver** and arguments are evaluated now; the
  entry stores `DeferEntry::Method { recv: Value, name: Rc<str>, args: Vec<Value> }` and the
  execution step performs the standard **member-call** with that receiver. This is deliberate
  and load-bearing: AScript has three call-POSITION hooks that fire only for a syntactic member
  callee — `std/schema` fluent methods, `std/shared` frozen-value methods (including the
  distinct frozen-instance diagnostic), and `workflow` `ctx.<method>` routing. Pre-binding the
  callee via a bare `read_member` would silently skip those hooks (a bare `s.minLength` reads
  the stored field, per the schema design). Storing `(recv, name, args)` and re-entering the
  member-call evaluator preserves every hook and panic message byte-identically with a normal
  call site. (It also matches Go: a method value's receiver is evaluated at defer time.)
- **Optional-chain callee** (`defer a?.m(x)`): the receiver is evaluated now. If it is `nil`
  (NUM falsy does NOT apply — `?.` is nil-only, as everywhere), the whole call would evaluate to
  `nil`, so **no entry is pushed** and the argument expressions are **not evaluated** — exactly
  the short-circuit a normal `a?.m(x)` performs. If non-nil, it behaves as the member form.
- Failures during this evaluation (a panicking arg expression, etc.) are ordinary failures of
  the `defer` **statement** — nothing has been registered yet.
- **Capture interplay (torture-tested):** when an argument or callee is a closure created at the
  defer site, the normal capture rules apply — a captured binding that is *mutated* anywhere is
  a shared cell (the closure sees later mutation); an unmutated one is captured by value
  (`finalize_capture_by_value`). So `let x = 1; defer (() => print(x))(); x = 2` prints `2` —
  on both engines, because the by-value split is keyed on the binding's final `mutated` flag.
  This is *argument-evaluation* vs *closure-capture* semantics, stated explicitly in the docs:
  `defer f(x)` snapshots `x`; `defer (() => f(x))()` does not (if `x` is mutated).

### 3.2 The defer stack: per function activation, LIFO, drained exactly once

Each function **activation** (call frame) owns a defer stack, empty by default and
allocation-free when empty (§9). `defer` pushes; frame exit **drains**: entries are taken out of
the frame (the frame's list becomes empty) and executed newest-first. Draining is what makes
every later rule simple and idempotent: a panic raised *while* draining unwinds through frames
whose lists are already empty, so no defer can run twice. A deferred call that itself contains
`defer` pushes onto **its own** activation (it is a normal call); a draining frame's list cannot
be appended to (no statement of that frame can execute during its drain). Defer-call **results
are discarded** (Go semantics) — including a Tier-1 `[value, err]` pair; use the arrow-IIFE form
if the error must be handled. There is no cap on the stack (it grows like any array; the
`defer-in-loop` lint is the guard rail).

### 3.3 Execution points — the frame-exit matrix (both engines, byte-identical)

Defers run **at frame exit, before the frame is popped**, and the in-flight outcome resumes
after they complete (subject to §3.6). The complete matrix:

| Exit route | Defers run? | Notes |
|---|---|---|
| Normal body completion | **yes** | pending outcome = implicit `nil` return |
| `return v` (`Flow::Return`) | **yes** | after `v` is computed (§3.7) |
| `?` propagation (`Control::Propagate`) | **yes** | the `[nil, err]` pair is the pending outcome |
| Panic unwind (`Control::Panic`) | **yes** | every frame between the raise and the `recover` boundary (or program abort) drains, innermost-first — Go's unwind model |
| `exit(code)` (`Control::Exit`) | **NO** | `exit` is process termination, not frame exit — the Go `os.Exit` rule. `recover` already cannot catch it (`src/interp.rs:6198`); defers are skipped for the same reason: "terminate now" must not run arbitrary script. Documented + tested. |
| `break`/`continue` | n/a | not frame exits; loop-crossing control never touches the defer stack |
| Generator `yield` | **no** (suspension, not exit) | the frame persists; defers run at body *completion* (§4.3) |
| Task cancellation (handle drop) | **NO** | §4.2 — the loudly-documented edge |
| Generator `close()` / last-drop | **NO** | §4.3 — same argument, plus `close()` is synchronous |

The ordering invariant across nested frames: defers run **innermost-frame-first** (the natural
consequence of draining at each frame's exit during unwind), and within a frame **LIFO**. Both
engines must produce the same interleaving of defer side effects with panic/propagate delivery —
proven by the four-mode differential over the torture corpus (§8).

**`recover` interplay:** `recover(f)` observes the panic only **after** every frame inside `f`
has drained (the panic crosses `recover`'s call boundary last). The `[nil, err]` pair `recover`
returns carries the final merged message of §3.6. Defers in the frame *containing* the
`recover(...)` call are unaffected (that frame did not exit).

### 3.4 `defer await <call>` — first-class async cleanup (owner decision, 2026-06-12)

- **The bare-call rule:** `defer f()` whose call **returns a `Value::Future`** is a loud Tier-2
  panic **at defer-execution time**, message:
  `deferred call returned a future that would be cancelled on drop — use 'defer await f()' or do async cleanup before exit`
  (anchored at the defer statement's span). Rationale: under M17 the future's task is already
  eagerly scheduled; discarding the handle would *cancel it instantly* (cancel-on-drop,
  `src/task.rs:94`) — a silent-cancel footgun. Silent **auto-await** is rejected too: the design
  goal is *no hidden control flow*, and an invisible suspension point inside an unwind is hidden
  control flow at its worst. The explicit `await` keyword keeps the suspension visible at the
  defer site.
- **The `await` form:** `defer await f()` evaluates callee/receiver + args at the defer
  statement (§3.1) and marks the entry `awaited`. At execution time the call is performed and,
  if the result is a `Value::Future`, it is **driven to completion** before the next (older)
  defer entry runs — defers stay strictly LIFO-sequential, awaited or not. `await` on a
  non-future result is identity (the language-wide rule), so `defer await syncClose()` is legal
  and harmless.
- **Where it is legal — the verified rule:** `await` in AScript is legal in **every** body —
  there is no async-fn-only restriction anywhere in the parser, resolver, or compiler (verified:
  no such diagnostic exists; the tree-walker's evaluator and the VM's `Op::Await` simply drive
  the future inline regardless of the enclosing function's asyncness; top-level `await` works
  today). Therefore **`defer await` is legal wherever `defer` is**, with no extra rule. (If a
  future change ever restricts `await`, `defer await` inherits that restriction by construction.)
- **Failure inside the awaited future:** a panic delivered by the awaited future follows the
  §3.6 merge rules exactly as a panic from a synchronous deferred call.
- **Stdlib reality check (audited):** every stdlib `close()`-family native (fs, tcp/udp, http
  bodies/servers, ws, sqlite/postgres/redis, process, tui, sync channels) returns a plain value
  or `[ok, err]` pair — **none returns `future<T>`** — so bare `defer h.close()` covers all
  native cleanup (the engine drives any internal I/O inline at defer-execution time, the same as
  an un-awaited call site today). `defer await` exists for **script** `async fn` cleanup and
  future-returning APIs (`task.spawn`, a user `async fn teardown`). The docs state this list.
- **Cancellation is unchanged:** if the task is cancelled while a deferred `await` is suspended,
  the body future is dropped at that point — the remaining (older) defers do **not** run. This
  is the same §4.2 rule observed mid-drain; documented with it.
- **Two-lane note (LANE coordination):** in LANE's terminology, a defer stack containing an
  `awaited` entry makes frame exit a **suspension point** — the sync driver must **escalate to
  the async driver** at frame exit when the draining stack can await, exactly like `Op::Await`
  on a pending future. A defer stack with no awaited entries (and no awaiting deferred bodies)
  still requires re-entrant calls at exit, which is already an escalation trigger in LANE's
  model — whichever spec lands second writes the one-paragraph reconciliation in the other's
  "coordination" section.

### 3.5 The pending-outcome stash — one rule for sync and suspended drains

At every frame exit, the engine holds the in-flight outcome — the return value, the propagating
`[nil, err]` pair, or the unwinding `Control::Panic` — in a **local** ("the stash") while the
drain runs, then resumes it. The stash is plain engine-local state (a Rust local in `run_body` /
the `Op::Return` arm / the unwind chokepoint), never observable from script and never stored on
a heap value. **`defer await` extends the same rule across suspension:** the stash lives in the
driving future's state across the `await`, exactly as any local does — there is no new
persistence mechanism, and nothing about the stash can leak into another task (each activation's
drain runs inside its own body future). The merge rules of §3.6 are the only way the stash
changes while draining.

### 3.6 A panic inside a deferred call — the merge rules (decided)

While draining, a deferred call may itself panic (including the §3.4 bare-future error, a
recursion-depth panic, or a panic delivered by an awaited future). The rules, chosen for
root-cause preservation + determinism, with **no silent drop**:

1. **In-flight Normal/Return:** the defer's panic **becomes the frame's outcome** — the return
   value is discarded and the panic unwinds (the caller's defers then run under rule 3).
2. **In-flight Propagate:** the defer's panic **SUPERSEDES** the propagation — the `[nil, err]`
   pair is discarded and the panic unwinds. (A Tier-2 bug outranks a Tier-1 expected error: the
   propagating pair describes an *anticipated* failure; the defer panic is an *unanticipated*
   one and must not be downgraded into the pair's shadow.)
3. **In-flight Panic: the ORIGINAL panic wins.** The deferred panic's message is **appended** to
   the original's as a suppressed note — exact format (locked, both engines share the helper):
   `<original message> (suppressed panic in deferred call: <new message>)` — span and
   span-source of the original are kept. Rationale: the first panic is the root cause the user
   must see (Go's chained-panic output serves the same need); appending is deterministic,
   byte-identical-testable, and not a silent drop. Multiple deferred panics append left-to-right
   in drain (LIFO) order.
4. **Remaining defers still run** (Go semantics): a panic in one deferred call never skips the
   older entries — rule 1/2 turns the stash into a Panic, and subsequent defer panics fall under
   rule 3. Cleanup must not be lost because other cleanup failed.
5. A deferred call **cannot** `Propagate` out (it is a function call — its `?` early-returns
   become its own return value, which is discarded) and cannot deliver `Flow::Break/Continue`
   (not syntactically possible across a call). `Control::Exit` raised inside a deferred call
   terminates as always (it outranks everything, is uncatchable, and skips remaining defers —
   the §3.3 exit rule applied mid-drain).

### 3.7 Return-value interaction: defers CANNOT modify the result — and the exact ordering

AScript has no named result parameters, so Go's one mutation channel does not exist here:
**the return value (or propagating pair) is fully computed before any defer runs, and nothing a
deferred call does can change it** (rule 1/2 of §3.6 can only *replace* it with a panic).
The locked frame-exit ordering, identical on both engines:

1. The body produces the outcome (return value / pair / panic).
2. **The frame's defers drain** (LIFO; merge rules §3.6; the frame still counts against
   `call_depth` — §3.8).
3. The **return-type contract** (`: T` on the function) is checked against the (unchanged)
   return value — only if the stash is still a value/pair after step 2. A contract panic raised
   here unwinds like any panic (the *caller's* defers run; this frame's list is already empty —
   drained, idempotent).
4. The frame pops; the outcome resumes in the caller.

Observable consequence (tested): a deferred `print` runs **before** a failing return-contract
panic is raised; a defer panic preempts the contract check entirely.

### 3.8 Recursion depth and limits

A deferred call is a real call: it re-enters the engine and **increments `call_depth`** while
the exiting frame's own depth unit is **still held** (drain happens before the frame's
decrement, step 2 vs 4 above — on the tree-walker the `DepthGuard` is still alive in `run_body`;
on the VM the drain precedes `return_from_frame`/`leave_frame_depth`). So a function exiting at
exactly `MAX_CALL_DEPTH` cannot run any deferred call — the deferred call panics with the
standard `maximum recursion depth exceeded`, which then follows §3.6. Byte-identical by the same
exactly-once accounting both engines already share (SP3 §B). `EXPR_NEST_LIMIT` applies inside
deferred bodies per call as always.

## 4. Async functions, cancellation, generators, workers

### 4.1 Async fns — defers work across awaits with zero new machinery

The defer stack lives in the activation (VM: the fiber's `CallFrame`; tree-walker: the call
scope, §5.1), and an async fn's activation **persists across `await`s** by construction. A defer
registered before an `await` runs when the body exits at any later point — normal return after
ten awaits, a propagate from an awaited future, a panic mid-body. Nothing special is built; the
frame-exit matrix of §3.3 simply applies inside the task driving the body.

### 4.2 Task cancellation: defers DO NOT RUN — rejected as unsound, documented loudly

M17 structured concurrency cancels by **dropping the body future**: the last `Value::Future`
handle's `Drop` aborts the task (`SharedFuture`, `src/task.rs:94`); `race` losers, `timeout`
expiry, and un-held async calls all cancel this way, at whatever `await` point the body is
parked on. Running defers there would mean **executing script code from inside a Rust `Drop`**,
re-entering an `Interp`/`Vm` whose `RefCell`s may be **live-borrowed by the very code that
triggered the drop** — unsound, full stop (and tokio's abort gives no async hook to do it
"later"). This is rejected, not deferred:

- **The semantics, stated loudly in docs and spec:** *a cancelled task's defers do not run* —
  the exact analogy is a killed Go goroutine (its defers don't run either). `defer` guarantees
  cleanup on every **exit the body itself takes**; cancellation is the body *not being allowed
  to exit*.
- **The structured-concurrency rule that makes this safe in practice:** cleanup that must happen
  even under cancellation belongs on the **resource's deterministic `Drop`** — which every
  native handle already has (the `ResourceState` rule: TCP, files, processes, FFI libs reclaim
  on drop). `defer` is for *script-level* cleanup ordering; the kernel-facing safety net is
  unchanged and unaffected.
- **Lint evaluated, declined for v1:** a `defer-in-async-under-cancellation` heuristic (warn on
  `defer` in an `async fn` whose callers race/timeout it) cannot see call sites from a
  definition and would fire on every async `defer` (near-100% FP) or none. The docs carry the
  rule instead; revisit only with evidence of real-world confusion. (The `workflow-determinism`
  precedent is different — workflow bodies are syntactically identifiable.)

### 4.3 Generators: defers run at body COMPLETION; `close()`/last-drop does NOT run them

A `fn*`/`async fn*` body's activation persists across `yield`s (the lazily-polled
`Pin<Box<dyn Future>>` / the generator fiber). The rules:

- **Completion (return or panic):** the body exits normally or unwinds → defers run per §3.3,
  before the final `resume` reports done / delivers the panic. ✔ natural.
- **`gen.close()` and last-handle drop:** the body is **dropped mid-suspend**
  (`src/coro.rs:469`: `close()` sets `done` and drops the `Body`/`Vm` fiber/`Worker` driver) —
  defers do **not** run. Two independent reasons: (1) the §4.2 argument verbatim — running
  script from a drop path is unsound; (2) **`close()` is a synchronous native method** — driving
  the generator body (which may `defer await`, or simply needs the async engine to execute any
  script call) is an `async` operation that a sync method cannot perform without blocking the
  single-threaded runtime on itself.
- **Python's `GeneratorExit` evaluated honestly (the brief asked):** CPython runs `finally`
  blocks on `close()` by *resuming* the generator with an exception injected at the yield point,
  synchronously, and erroring if the generator yields again. The AScript equivalent — a
  resume-with-close-signal that injects a panic at the parked `yield` so the body unwinds
  through §3.3 — is **implementable on the VM** (resume the fiber such that `Op::Yield`'s
  resumption raises a designated panic) and on the tree-walker (poll the body with a close flag
  set), but it (a) changes `close()` from sync-and-infallible to async-and-fallible (a deferred
  call can panic, await, or take unbounded time — `close()` would need to become awaitable and
  define semantics for a body that yields again), and (b) is new cross-cutting machinery (a new
  generator state, new `for await` interactions, worker-streaming teardown changes). **Recorded
  as the v2 design** (`gen.close()` → `async`, GeneratorExit-style unwind, yield-after-close =
  Tier-2 panic), explicitly NOT v1. v1 documents the pattern: wrap generator *consumption* in
  the function that owns the resource (`defer` in the owner, not the generator), or complete the
  generator before discarding it.

### 4.4 Workers and isolates: nothing special, by construction

A `worker fn` / actor-method / `worker fn*` body runs on a complete, independent `Interp` in its
isolate — its defers are ordinary defers of that engine instance. `defer` introduces no new
value kind, so the serializer airlock is untouched (a `DeferEntry` lives only inside a frame,
never in a `Value`); a closure captured into a defer entry is as non-sendable as ever, but defer
entries never cross the boundary. Worker code-shipping ships `Stmt::Defer` inside function
bodies like any statement (the closure walker sees the call expression's free names). Pooled
`worker fn` isolates reuse an `Interp` across calls — safe, because defer stacks are
per-activation and fully drained at each call's exit (nothing leaks into the next request).

## 5. Engine implementation

### 5.1 Tree-walker (the oracle)

- **AST:** `Stmt::Defer { call: Expr, awaited: bool, span: Span }` (`src/ast.rs:285` enum).
  Exhaustive-match additions (compile-enforced): the `exec` arm (`src/interp.rs`), `fmt.rs`'s
  statement writer (the legacy formatter stays exhaustive even though `ascript fmt` uses the CST
  formatter), `ast.rs` `Display`.
- **The defer scope:** `env.rs`'s `Scope` gains
  `defers: Option<Rc<RefCell<Vec<DeferEntry>>>>` (one `Option` word per scope, `None` everywhere
  except activation roots). `run_body` (`src/interp.rs:5146`) installs a fresh list on
  `call_env` before executing the body; the top-level drivers (`run_file`/`run_source`/
  `run_tests` program exec, REPL submission exec, the module-import exec — enumerated by grepping
  `.exec(`) install one on the program env. `Stmt::Defer` walks the scope chain to the **nearest**
  list — which is always the enclosing activation's, because every activation installs one
  (closures' definition envs are *behind* the callee's own call env in the chain).
- **`Stmt::Defer` eval:** per §3.1 — match the stored call's shape (`ExprKind::Call` with
  `Member`/`OptMember` callee → Method entry, receiver evaluated now; anything else → Call entry,
  callee evaluated now); evaluate args left-to-right (spread materialized, the existing
  `CallArg` evaluation helpers); push `DeferEntry { kind, args, awaited, span }`.
- **Draining:** `run_body` captures the body's outcome (the existing
  `match grow_future(self.exec(body, call_env)).await` site), then — for every outcome except
  `Control::Exit` — drains the list newest-first: Method entries re-enter the **member-call
  evaluator** (hooks intact, §3.1), Call entries `call_value`; `awaited` entries await a
  `Value::Future` result, non-awaited future results raise the §3.4 panic; the §3.6 merge runs
  in a shared helper (`interp::merge_defer_outcome`, used verbatim by the VM). The drain happens
  BEFORE the return-type contract check (§3.7) and while `_depth` is still held (§3.8). The
  top-level drivers drain identically after the program body (before the driver's
  `Propagate => Ok` conversion).
- **Borrow discipline:** entries are drained out of the `RefCell` into a local `Vec` first; no
  borrow is held across the per-entry `.await` (clippy `await_holding_refcell_ref` enforced).

### 5.2 VM

- **Frame state:** `CallFrame` (`src/vm/fiber.rs:20`) gains `pub defers: Vec<DeferEntry>` —
  `Vec::new()` is allocation-free, so the no-defer call path allocates nothing new (CALL-diet
  compatible; the frame struct grows by 24 bytes, measured in §9).
- **Two opcodes** (appended after `Op::Break`, the current tail of the dense discriminant space
  in `src/vm/opcode.rs`):
  - `Op::DeferPush` — operands `flags: u8, argc: u8` (width 2). Flags: bit0 `awaited`, bit1
    `spread` (args were materialized into ONE array by the existing spread-builder sequence —
    stack is `[callee, argsArray]`; without bit1, stack is `[callee, arg0..argN]`). Pops
    `argc + 1` (or 2 under bit1), pushes 0; appends to the **current frame's** `defers` with the
    op's span.
  - `Op::DeferPushMethod` — operands `name: u16 (const idx), flags: u8, argc: u8` (width 4,
    the `CallMethod`-shaped encoding). Stack `[recv, args…]` (or `[recv, argsArray]` under
    bit1). Builds a Method entry. The compiler emits the OptMember form as: evaluate receiver,
    dup + nil-test jump that pops the receiver and skips the arg evaluation + push entirely on
    nil (mirroring the existing optional-chain lowering), preserving §3.1's
    no-entry-no-arg-eval rule.
- **Compilation** (`src/compile/mod.rs:1783` `compile_stmt`, new `DeferStmt` arm): member callee
  → receiver + args + `DeferPushMethod` (defer **never** uses the `CallMethod` IC fusion — the
  entry must carry the name for hook-correct late dispatch; defer sites are cold by nature);
  other callee → callee-as-value + args + `DeferPush`; named args → the §2.1 compile-time error
  (defense-in-depth behind the parser); spread → the existing args-array builder + bit1.
- **Drain sites — every frame-exit path in `run.rs`, enumerated:**
  1. **`Op::Return`** (`run.rs:3345`): if `frame.defers` is non-empty, `mem::take` the list,
     run the drain (async, stash = the popped return value, §3.6 merge via the shared helper),
     then proceed to `return_from_frame` (contract check + pop) with the surviving value — or
     `return Err(panic)` if the stash became a panic. The empty-list check is a `Vec::is_empty`
     on an already-hot frame field — the zero-cost-when-unused path (§9).
  2. **`Op::Propagate`'s err path** (`run.rs:3359`): same drain before its `return_from_frame`,
     stash = the `[nil, err]` pair (§3.6 rule 2 may replace it).
  3. **The unwind chokepoint:** `Vm::run` (`run.rs:1057`) — the single wrapper every
     `run_loop` `Err` already funnels through (the SP4 span-source binder proves the pattern).
     On `Err(Control::Panic | Control::Propagate)` (Propagate-as-`Err` is the rare cross-task
     resurfacing path; included for totality) — but **not** `Control::Exit` — drain **all** live
     frames' defer lists top-of-stack-first (each LIFO), merging per §3.6, then return the final
     control. Frames are *not* popped here (the fiber is abandoned wholesale, as today); their
     lists are emptied, so re-entry/double-drain is impossible. Re-entrant `run` calls
     (`call_value` for HOF callbacks, method invokes, **and the defer drain itself**) each drain
     their own fiber — innermost-first ordering falls out of the call structure.
  4. **Root/script frame:** `Op::Return` of the root frame is case 1 — top-level and
     module-body defers need no extra VM code (module import runs the module chunk through the
     same loop; defers drain before `RunOutcome::Done` releases the exports).
  5. **Generators:** a generator fiber's completion is case 1; its panic is case 3 (the resume's
     `run` call drains before the `Err` reaches `resume_vm`); `close()`/drop never enters `run`
     → no drain (§4.3), and the GC never traces a fiber's defer entries beyond the normal frame
     lifetime (entries hold plain `Value`s inside the fiber, dropped with it — refcount
     reclamation; a defer entry is not a new GC edge class because fibers already own `Value`s).
- **Execution of one entry:** Method → the generic member-call routine (the same
  schema/shared/ctx hook chokepoint a compiled member call falls back to); Call →
  `call_value`. Both re-enter via the existing re-entrant fiber path (depth-guarded, §3.8).
  Result handling per §3.4 (`awaited` → drive `Value::Future`; bare future → the Tier-2 panic).
- **Shared semantics helper:** the §3.6 merge + the §3.4 message + the suppressed-note format
  live in ONE place (`src/interp.rs` beside `Control`), called by both engines — divergence is
  structurally impossible for the rules' text.

### 5.3 What does NOT change

No new `Value` variant (defer entries are frame-internal). No GC rule change (frames already own
`Value`s; native handles stay untraced). No serializer/airlock change. No `Environment`
behavioral change beyond the inert `Option` field. The tree-walker remains the oracle and is
**never relaxed**.

### 5.4 `.aso`, verifier, disasm, bcanalysis

- **`ASO_FORMAT_VERSION` 27 → 28** (`src/vm/aso.rs:167`) — two new opcodes change the executable
  byte space (the standing rule: any opcode change bumps). The reader needs no new section
  (operands ride in `Chunk.code`); the version gate alone rejects old/new mismatches.
- **`verify.rs`:** `stack_effect` (`verify.rs:217`) entries — `DeferPush`: pops `argc + 1`
  (flags bit1 → pops 2), pushes 0; `DeferPushMethod`: pops `argc + 1` (bit1 → 2), pushes 0;
  plus operand validation (name idx in const-pool range and a string constant — the
  `BadInterface`-style structured error precedent; flags' undefined bits must be zero, rejected
  otherwise so future flag bits stay verifiable).
- **`disasm.rs`:** render both ops with decoded flags/argc/name
  (`DEFER_PUSH await spread argc=2`, `DEFER_PUSH_METHOD 'close' argc=0`).
- **`bcanalysis.rs`:** the new ops join the walker's decode table (no jump targets; straight-line).

### 5.5 No kill switch — and why that is correct (the Gate-15 question, answered)

`--no-specialize`-style switches exist to prove that **performance machinery** is observably
invisible. `defer` is **observable semantics** — a mode without it is a second dialect, which is
exactly what the four-mode identity exists to prevent. So: no kill switch, by design. Gate 15's
real obligations are met the applicable way: the defer corpus joins `vm_differential.rs` (both
feature configs) and **the grammar-aware fuzzer generates `defer` the same PR** (§8.3), with a
coverage assertion proving deferred paths actually ran (the anti-false-green rule).

## 6. Static checking & lints

### 6.1 `defer-in-loop` (Warning, default-on)

Fires when a `DeferStmt` is lexically inside a `while`/`for` body **within the same function**
(a nested `fn`/arrow body resets the walk — its defers are per-call of the closure). Message:
`'defer' inside a loop registers one call per iteration; they all run at function exit — wrap
the loop body in a function if you want per-iteration cleanup`. The Go-vet precedent. Registered
in `src/check/rules/` (new `defer_in_loop.rs` + the `ALL` table, `rules/mod.rs:31`), walking the
`ResolvedNode` tree like `range_step.rs`. Zero hits on `examples/**` except the intentionally-
annotated torture example (which carries a config suppression or restructures — Gate 5 stays 0).

### 6.2 `defer-async-call` (Warning, default-on, advisory companion to the §3.4 runtime error)

Fires on a **bare** (non-`await`) `defer` whose callee resolves, within the file, to a
syntactically `async fn` declaration — provably a future-return, so provably the §3.4 runtime
panic. Message: `deferred call to async fn '<name>' will panic at runtime — use 'defer await
<name>(…)'`. Zero-FP by construction (syntactic async-decl resolution only; member callees,
imports, and dynamic callees are out of scope — the runtime error is the backstop). The brief's
"error instead of lint" decision stands — this lint is *additive* early DX on top of the error,
not a replacement.

### 6.3 Checker/infer integration

The infer pass (`src/check/infer/pass.rs`) walks `DeferStmt` like an expression statement
(`synth` the call so arg-level `type-*` diagnostics surface inside deferred calls); no new
`CheckTy`, no exhaustiveness interaction. `unreachable`/`unused`/`undefined` rules see the defer
expression through the same statement walk (their statement dispatch gains the arm — exhaustive
matches make omission a compile error where the dispatch is matched, and a test pins `undefined`
firing inside a defer arg).

## 7. Tooling parity (confirmed-working, not just edited — Gate 11)

- **Formatter:** the CST formatter (`src/syntax/format/mod.rs`) gets a real `DeferStmt` arm —
  canonical `defer <call>` / `defer await <call>` (single spaces, call canonicalized by the
  existing expression renderer); idempotence covered by the standing fmt-idempotence tests over
  the new examples. The legacy `fmt.rs` writer gains its (exhaustiveness-mandated) arm too.
- **LSP:** `defer` joins the keyword completion list (`src/lsp/providers/completion.rs:28`) +
  a snippet (`("defer", "defer ${1:resource}.close()")` beside the `while` snippet,
  `completion.rs:174`). Semantic tokens: `DeferKw` is a *real* keyword token (reserved, §2.2),
  so it styles as a keyword through the normal kind classification — no `contextual_keyword_spans`
  entry needed (`semantic_tokens.rs:86` is for remapped Idents only); a provider test pins it.
  Hover/definition: nothing to do (no new symbol kinds).
- **Tree-sitter queries:** `"defer"` and the statement's `"await"` join the keyword captures in
  `queries/highlights.scm:21`; spot-check `folds`/`indents` need no change (statement-level,
  no block). The vscode TextMate grammar (`editors/vscode`), if it carries a keyword list, gets
  `defer` in the same PR (verified during implementation; the conformance rule is "every editor
  artifact that enumerates keywords").
- **REPL:** `defer` needs no `is_incomplete` change (no new delimiters); a session test pins
  top-level defer running at submission end and `defer` inside a REPL-defined fn behaving
  normally on a later call.
- **DAP/debugger:** breakpoints inside deferred *bodies* (closures) work as ordinary lines;
  stepping over a `return` may execute defers (observable as frames from the deferred calls) —
  no DAP protocol change; noted in the docs. The profiler attributes deferred-call time to the
  deferred function (normal call attribution).

## 8. Correctness — the gates

### 8.1 Four-mode differential (Gate 1)

Every behavior in §3–§4 lands as a `tests/vm_differential.rs` battery (tree-walker ==
specialized == generic, plus the `.aso` mode via the standing example pipeline), in BOTH feature
configs. The torture corpus (each row also a runnable example or focused test):

defer in loops (accumulation + LIFO across iterations); nested fns (inner exits don't fire outer
defers); defer capturing mutated locals (the §3.1 capture-by-value interplay, both the snapshot
and the cell case); defer + rest/destructuring/spread args; defer in class methods and `init`
(incl. a field-contract panic in `init` running defers); defer + `recover` in all §3.6
combinations (return+defer-panic, propagate+defer-panic-supersede, panic+defer-panic-suppressed-
note exact message, multi-defer multi-panic append order, remaining-defers-still-run);
defer + return-type-contract ordering (§3.7); deep recursion at `MAX_CALL_DEPTH` (§3.8);
top-level/module-import/REPL defers; generator completion vs `close()`/drop; `exit()` skipping
defers; schema-method defer (hook preserved); frozen-instance method defer (distinct diagnostic
preserved); `a?.m()` with nil receiver (no entry, args unevaluated — side-effect probe);
**defer await** happy path; **defer await during `?`-propagate unwind**; **defer await during
panic-to-recover unwind**; **LIFO mixing sync and await defers**; **the bare-future error
message** (exact text); cancellation (a raced/cancelled task's defers do NOT run — asserted via
a side-effect channel and a deterministic completion ordering).

### 8.2 Coverage assertion (the anti-false-green rule, Gate 15)

A `fuzzgen`-gated counter pair on the engines (defer entries pushed / drained, plus
drain-on-panic taken) with a corpus assertion that all are nonzero after the differential run —
proof the new paths executed, not merely compiled.

### 8.3 Fuzzer (Gate 15, same PR)

`src/fuzzgen/mod.rs` `stmt()` gains weighted `defer` emission: bare defers of declared sync fns
(printing args, so the differential bites on order), defer of arrow-IIFEs touching mutable
locals, defer inside generated loops and nested fns, `defer await` of generated `async fn`s, and
defers in bodies that `?`-propagate via the existing `rerr` helper — so generated programs
exercise §3.3's matrix. The differential fuzz target inherits it; a smoke campaign
(`cargo +nightly fuzz run differential`) runs before merge.

### 8.4 Negative spaces

`let defer = 5` rejected by all three front-ends (conformance catalog); non-call defer parse
errors (every §2.1 rejected form, both parsers, message-identical); named-arg defer error;
verifier rejection of a hand-built `DeferPushMethod` with a non-string name const and of nonzero
undefined flag bits; `ASO_FORMAT_VERSION == 28` asserted with the version-gate test updated (old
`.aso` rejected cleanly).

## 9. Performance (Gates 12, 16, 17, 18)

- **Defer-free code shows ZERO regression — the empty-stack fast path is the design:** the only
  hot-path additions are (a) a `Vec::is_empty` check on the frame at `Op::Return`/`Op::Propagate`
  (predictably-taken, against a field in the already-loaded frame), (b) +24 bytes per
  `CallFrame` (no allocation — `Vec::new` is heapless), (c) one `Option` word per tree-walker
  scope. The unwind chokepoint work is on the panic path only (cold by definition).
- **Measured, not promised:** same-session A/B (`bench/DEFER_RESULTS.md`) over the standing
  bench corpus + the call-heavy workloads; the Gate-12 floor (spec/tw geomean ≥2×) re-asserted;
  the dispatch loop was touched → **`dbg_zero_cost_gate` re-run** and recorded; peak RSS per
  Gate 18 (expect noise-level — the frame grew by 24B).
- Defer-USING code pays for what it uses: one entry push (a small struct append) per defer, one
  ordinary call per drain entry. No IC work (defer dispatch is deliberately generic, §5.2 —
  cleanup sites are cold).

## 10. Docs & examples

- **Examples:** `examples/defer.as` (intro: file close, LIFO, `?` interplay, defer await,
  evaluation timing) and `examples/advanced/defer_resources.as` (production-shaped: multi-
  resource acquisition with defer-on-each, panic-unwind + `recover` observation of the merged
  message, generator-owner pattern from §4.3, fully error-handled) — four-mode tested,
  fmt-idempotent, joining the standing corpus (never behind `EXAMPLE_SKIPS`).
- **Docs placement (decided):** the primary section lives in
  `docs/content/language/errors.md` (“Cleanup with `defer`”) — defer's identity is its
  interaction with `?`/panics/`recover`, which that page owns; `syntax.md`'s statement list
  gains the one-line form + link; `modules-async.md` gains the async/cancellation/generator
  rules (§3.4/§4.2/§4.3) where async semantics already live. No new page → **no `NAV` change**
  (the orphan-gotcha rule observed).
- **LSPEC cross-reference:** the language-spec effort derives its normative grammar from the
  tree-sitter grammar — DEFER's grammar change flows in automatically, but the LSPEC semantics
  chapters need the §3 matrix; a coordination note is added to the LSPEC spec's inventory the
  same PR.
- **`CLAUDE.md`:** a new bullet under "Language features — gotchas" (reserved keyword, call-only,
  frame-exit matrix incl. the cancellation/close edges, the two opcodes, the merge rules,
  the hook-preserving Method entry); `goal-perf.md` status table + `superpowers/roadmap.md`
  updated at merge.

## 11. Rejected alternatives (recorded so they aren't re-litigated)

- **`using`/`with` blocks** — already rejected in `goal-perf.md` for `defer`: requires a
  closeable protocol (an interface every resource must implement) and composes worse across
  mixed/conditional resource lifetimes; `defer` needs no protocol and handles conditional
  acquisition naturally.
- **Block-scoped defer** (Swift-style `defer { … }` at block exit) — rejected for v1: Go's
  function scoping is simpler to specify across `?`/panic unwind, matches the frame machinery
  both engines already have, and block-scoping can be added later as sugar without breaking
  function-scoped programs. The arrow-IIFE covers ad-hoc block cleanup today.
- **Arbitrary-expression defer + lint** — rejected (§2.1): a no-effect deferred expression is a
  silent bug; the parse error prevents it, the lint would only report it.
- **Contextual `defer` keyword** — rejected (§2.2): the `defer (`-ambiguity breaks either
  statement-leading calls to a `defer` function (silently) or the IIFE idiom; `interface`
  precedent; zero collision cost.
- **Silent auto-await of deferred futures** — rejected (§3.4): hidden control flow (an invisible
  suspension inside unwind); the explicit `defer await` keeps it visible. **Bare future
  silently allowed** — rejected: instant cancel-on-drop is a data-loss footgun.
- **Running defers on task cancellation** — rejected as **unsound** (§4.2): script execution
  from a Rust `Drop` into a possibly-borrowed runtime.
- **Running defers on `gen.close()`/last-drop** — rejected for v1 (§4.3): `close()` is
  synchronous and the unwind-injection design is real new machinery; recorded as the v2 design
  sketch.
- **Running defers on `exit()`** — rejected (§3.3): Go's `os.Exit` rule; `exit` means *now*.
- **Pre-bound member callee (`read_member` at defer time)** — rejected (§3.1): silently skips
  the schema/shared/workflow call-position hooks; the Method entry preserves them.
- **Named-argument defer support in v1** — deferred as a Tier-1 error (§2.1): zero demand,
  meaningful opcode cost; v2 if evidence appears.
- **A defer kill switch** — rejected (§5.5): semantics, not specialization; a switch would be a
  second dialect.
- **Re-evaluating arguments at exit time** — rejected (§3.1): Go's at-statement evaluation is
  the principle of least surprise for cleanup (`defer f(conn)` must close *that* conn) and the
  only design that lets the entry be a plain value vector.

## 12. Grounding (verified against the tree, 2026-06-12)

`src/parser.rs:96` (`statement()` dispatch; `:106` worker lookahead; `:1749` contextual `step`) ·
`src/syntax/parser.rs:268` (`stmt()`; `:322` `at_worker_modifier`) ·
`tree-sitter-ascript/grammar.js:173` (`_statement`), `:672` (`call_expression`) ·
`src/lexer.rs:589/601` + `src/syntax/lexer.rs:416` (keyword tables; `interface` reserved
precedent) · `src/ast.rs:285` (`Stmt`), `:142` (`CallArg::Named`), `:33` (`ExprKind::Call`) ·
`src/interp.rs:26/35` (`Flow`/`Control` — incl. `Control::Exit`, present in code though absent
from the CLAUDE.md two-enum summary), `:5146` (`run_body` — the single call funnel; depth guard;
outcome match), `:6186` (`recover` — catches `Panic`, passes `Exit` through) ·
`src/env.rs:15` (`Scope`/`Environment::child`) · `src/vm/fiber.rs:20` (`CallFrame`),
`:56` (`alloc_cells`) · `src/vm/run.rs:1057` (`Vm::run` — the existing single `Err` chokepoint,
SP4 precedent), `:1088` (`run_loop`), `:3345` (`Op::Return`), `:3359` (`Op::Propagate`),
`:3431` (`Op::Await` — drives futures inline, no async-fn restriction), `:5796`
(`return_from_frame` — contract check, pop, depth decrement) · `src/vm/opcode.rs:29` (dense
`Op` space; `Break` is the tail; `operand_width`) · `src/vm/verify.rs:217` (`stack_effect`),
`:79` (`BadInterface` structured-error precedent) · `src/vm/aso.rs:167`
(`ASO_FORMAT_VERSION = 27`) · `src/coro.rs:469` (`close()` is sync; drops the body/fiber) ·
`src/task.rs:84-99` (`SharedFuture` cancel-on-drop) · `src/compile/mod.rs:1783`
(`compile_stmt`), `:4870-5113` (call/spread/named emission) · `src/check/rules/mod.rs:31`
(`ALL`) · `src/lsp/providers/completion.rs:28/174` (keywords/snippets) ·
`src/lsp/providers/semantic_tokens.rs:86` (contextual-Ident remap — N/A to a reserved keyword) ·
`tree-sitter-ascript/queries/highlights.scm:21` (keyword captures) · `src/fuzzgen/mod.rs:89`
(`gen_program`) · grep audit: zero `defer` identifier uses in stdlib/examples/docs/tests.
Go semantics reference: the Go spec's defer statement (args evaluated at defer time; LIFO; runs
on panic unwind; `os.Exit` skips; killed goroutines don't run defers) — the adopted baseline,
deviations stated inline.
