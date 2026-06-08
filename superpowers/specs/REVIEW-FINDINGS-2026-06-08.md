# Campaign Spec Review — Findings & Required Revisions (2026-06-08)

> **STATUS: ALL REVISIONS APPLIED (2026-06-08).** Every must-fix below has been addressed in its spec by
> the revision wave; all 12 specs are 🔒 lock-ready. Plan-time decisions + new cross-spec reconciliation
> items (unified `Vm.instrument` seam, `implements-violation` ownership, VAL `unsafe`-layer ordering) are
> recorded in `goal.md`. This doc is retained as the audit trail of what was found and fixed.

Independent adversarial review of all 11 campaign specs. Each spec must clear its **must-fix** list
before it is locked (🔒) and planned. Verdicts: NUM/VAL/ADT/IFACE/TYPE/FFI/FUZZ/DX = REVISIONS-NEEDED;
SRV/BIN/JIT = APPROVE-WITH-REVISIONS. No MAJOR-REWORK. Grounding was strong across the board (the
791-site count, ASO=18, single-quote-string, free operator tokens, the gcmodule registry-collector
claim, the LSP-not-on-legacy-AST correction all verified TRUE).

## Cross-cutting findings (affect multiple specs — resolve once, centrally)

1. **[LIVE BUG] The `.aso` reader has an unbounded-allocation / abort(OOM) vector TODAY.** Every
   `reserve(n)`/`with_capacity(n)` in `src/vm/aso.rs` (`read_chunk` ~571-610, `read_proto`/`read_value`/
   `read_type` ~705/724/769/918/1201…) uses an attacker-controlled `u32` length with **no
   `.min(r.remaining())` clamp** — a crafted `.aso` forces a multi-GB alloc and SIGABRT *before*
   `verify` runs. The worker serializer already shows the fix (`serialize.rs:564`
   `with_capacity(len.min(r.remaining()))`). This is a real present bug, gates BIN, and is the FUZZ
   `.aso` target's first catch. **Action:** fix it (clamp all reader allocations) as an explicit
   deliverable — FUZZ owns it, or a standalone pre-req bugfix.
2. **[LIVE GAP] The worker code-shipping closure may walk only functions+consts, not
   classes/enums/interfaces.** Workers Spec A §6 defines the closure as "top-level functions and consts."
   IFACE (a `worker fn` using `instanceof Reader`) and ADT (enum referenced in a `worker fn`) both assume
   descriptors ride the closure. The `fix/actor-class-deps` followup commits ship classes/enums an
   *actor method* references — so it's **partially** handled for actors, unknown for pooled `worker fn`.
   **Action:** verify `src/worker/dispatch.rs` closure walker against classes/enums/interfaces; if it
   doesn't walk them, that's a shared prerequisite for IFACE + ADT (and SRV handlers).
3. **CST nests only the FIRST `match` arm under `MatchExpr`** (subsequent arms are sibling statements;
   `pass.rs:949-951`). Tolerable for narrowing (loses precision) but a **Gate-5 false-positive flood**
   for ADT exhaustiveness (every multi-arm match looks non-exhaustive). **Action:** ADT's exhaustiveness
   must gather arms across the sibling chain (or the CST nesting is fixed); shared concern for any
   match-based analysis.
4. **`check_type` is a free fn `(value, ty)` with NO env** (`interp.rs:5704`); its `Type::Named` arm
   matches class *by name string only* and cannot resolve a name to an interface/reserved-type. Affects
   IFACE (interface contracts) AND NUM (`instanceof int|float|number`, which today panics on a non-class
   RHS, `interp.rs:5102`). **Action:** both specs need a specified resolution path (reserved-type-name
   recognition / env-threading), not "checked like a class today."
5. **Merge order is load-bearing: NUM first.** NUM renames `Value::Number → Float` + adds `Value::Int`;
   SRV's `SharedNode`, ADT's `EnumVariant`, and several specs are written against today's single
   `Number`. Each must rebase onto NUM's split. `.aso` version bumps are sequential (NUM/ADT/IFACE/DBG
   each +1 by merge order — never hardcode 19). One grammar publish per merge wave.
6. **Two `Option`-gated `Vm` hooks** — DX coverage + DBG breakpoints/profiler — land on the same hot
   loop. **Action:** design ONE unified instrumentation seam (coverage | breakpoint | sample) so the
   not-attached path has a single predictably-not-taken check, not two (Gate 12).
7. **`Value` is 32 bytes, not 24** (the fat variants are `ClassMethod`/`GeneratorMethod`, each a 24-byte
   2-field tuple — not `Decimal`/`Str`). VAL's shrink target/analysis must account for them.

## Per-spec must-fix punch lists

### NUM — REVISIONS-NEEDED
- **[CRITICAL] `|` bitwise-or vs or-pattern / union-type collision.** Today `|` is invisible to the
  expression parser (`coalesce()` excludes it) so `case A | B` and `T | U` work. Placing bitwise `|` at
  the additive tier makes the pattern/type value-parser greedily eat `A | B`. **Fix:** bitwise-or gets a
  dedicated tier that the pattern/type entry points deliberately bypass (exactly as `coalesce` does
  today); specify for BOTH parsers + a conformance test (`match x { 1|2 => …}` vs a bitwise `a | b`).
- **[CRITICAL] `instanceof int|float|number` mechanism unspecified** (today `instanceof` panics on
  non-class RHS, `interp.rs:5102`; RHS is an ordinary value expr). **Fix:** specify reserved-type-name
  RHS recognition → subtype check, in interp + the VM `Op::InstanceOf` (which routes through shared
  `apply_binop`, not a per-op handler) + checker narrowing + grammar.
- **[CRITICAL] Truthiness grounding error.** Spec §3.3 says `0`/`0.0` are falsy and calls it "unchanged";
  the oracle makes them **truthy** (only `nil`/`false` falsy; test `value.rs:923`). **Fix:** DECISION
  REQUIRED (keep `0` truthy vs make it falsy as a declared breaking change); correct §3.3 either way.
- `<<` overflow semantics underspecified (checked_shl checks shift-amount only, not lost bits — pick the
  rule + boundary tests `1<<63`, `1<<64`). `**` int path: `checked_pow` takes `u32` exponent — handle
  large/negative exponent; `2 ** -1` → float surprise example.
- Map-key `a==b ⟺ same key` invariant **fails for NaN** (NaN≠NaN) and is undefined vs Decimal — scope it
  to `{int,float}` and carve out NaN.
- Gate 12: commit the checked-int adaptive path to a benchmark (no steady-state regression).
- Gate 7: migration blast radius understated (division goldens flip hardest: `10/3` 3.33→3).

### VAL — REVISIONS-NEEDED  (gcmodule claim verified TRUE)
- Fix the headline number: `Value` is **32 bytes** (ClassMethod/GeneratorMethod fat variants), not 24;
  Stage-1 must also box/shrink those two to reach 16.
- SMI bit budget is self-contradictory (47-bit vs ±2^46 vs 48−tag don't reconcile) — pick one.
- State escape-analysis identity preservation as a **soundness obligation** on the analysis (must-escape
  if `==`/`is`/identity-Map-key reachable), not a free property.
- Add a `--no-specialize` (generic VM) perf axis to the bench (Gate 12 demands no generic-mode regress).
- Cross-spec: VAL's hand-written `unsafe` tag-dispatched Clone/Drop/trace forces ADT/IFACE/SRV variant
  additions to hand-edit unsafe dispatch; SRV's `Send` `Arc` leaf inside a NaN-box needs a coordination
  note. Add `assert_not_impl_any!(Value: Send)`.

### ADT — REVISIONS-NEEDED
- **[HIGH] Exhaustiveness vs the CST-first-arm-only nesting** (cross-cutting #3) — Gate-5 tripwire.
- **[HIGH] Worker unit-variant re-interning is NEW, not existing** (decoder builds a fresh `Rc::new`,
  `serialize.rs:624`, no EnumDef lookup) — own it as new work + reconcile with the equality contract.
- **[HIGH] `Rc→Cc` for EnumVariant** touches every construction site incl. `serialize.rs:624,897`; unit
  variants now allocate a `Cc` (cycle-collector registration) — Gate-12 benchmark or keep unit variants
  Rc-cheap.
- Reconcile with the existing `unknown-enum-variant` rule (`config.rs:43`). Resolve variant-pattern
  grammar: the `Range`-style semantic-recovery path may avoid a new `variant_pattern` node + GLR
  conflict — justify or adopt it. Tighten the bare-unit-variant-vs-Option-C-binding runtime gap
  (`env.get` has no subject-type knowledge). Add `is_truthy`/`type_name` payload arms. Gate 9: the
  non-exhaustive example must be an *exercised* check failure, not a comment.

### IFACE — REVISIONS-NEEDED
- **[G1 load-bearing] `check_type` can't resolve a name to an interface** (cross-cutting #4) — specify
  the env-aware path + signature change.
- **[X1] Worker closure may not ship interface/class descriptors** (cross-cutting #2) — a `worker fn`
  using `instanceof Reader` would ship the fn but not `Reader`.
- **[C4] Eager flatten-at-declaration contradicts late-binding module-globals** — if interfaces
  forward-reference (like classes/fns), flatten must be lazy with a runtime cycle guard.
- **[C5] Worker serializer exhaustive arms** (`unsendable_kind:110`, `encode:381` `unreachable!:500`)
  need the `Value::Interface` arm or it's a trap. Fix the VM-`InstanceOf`-handler miscite (it's the
  shared `apply_binop` arm `interp.rs:5100`). Pin arity-compatibility for defaulted/optional/rest params.
  Cache immortality is per-isolate — state it. Gate 9/12: split examples across runtime-half vs
  TYPE-half; add the `instanceof Class` micro-benchmark.

### TYPE — REVISIONS-NEEDED
- **[HIGH internal contradiction] §4.6 invariance vs the covariant rule-8 it cites** (`ty.rs:400` is
  covariant). User generics built "the same way" would be covariant, contradicting the locked
  invariant decision and the `Box<Dog>↛Box<Animal>` claim. **Fix:** make `ClassApp`/`EnumApp` genuinely
  invariant (a *change* to rule 8, not a reuse) and stop claiming the built-ins already are.
- **[MED] Blocking wiring misses 2 of 4 sites:** `check_call_args` (`pass.rs:886`) and
  `check_field_default` (`:534`) don't route through `check_against` — the `blocking` flag must be a
  severity arg on `emit` at all four sites.
- Factual: ADT + IFACE specs DO exist (design against them, not goal.md stubs). `implements-violation`
  ownership unowned (TYPE provides `conforms`; pin who registers/emits it). Pin empty-array `synth([])`
  element type (must leave generic var gradual, not `Never`). Gate 9: ship a runnable generic example in
  `examples/**`, not just `tests/check.rs` fixtures. Expression-level `Box<int>(…)` disambiguation is a
  NEW parser deliverable (NUM's `>>`-split only covers known-type-position), with its own conformance
  tests.

### FFI — REVISIONS-NEEDED  (security-sensitive)
- **[SECURITY] DNS egress bypasses the cap gate** — `net.lookup`/`lookupOne` route through `call_net`/
  `net_host` (not connect/bind), ungated → data exfil under `--sandbox`/`caps.drop("net")`. Add
  `net_host` to the `net` chokepoint set; enumerate EVERY resource-acquiring entry point, not ~5.
- **[SECURITY] `caps.drop` irreversibility vs pooled-isolate REUSE is unsound** — `isolate_loop` builds
  ONE `interp` and serves MANY requests; a drop in one `worker fn` leaks to the next, and reinstalling a
  fuller set per request *is* a re-grant (contradicts "monotone, never re-grant"). **Fix:** pin
  cap-bearing work to a dedicated (non-pooled) isolate OR define a precise per-request reset; `run_in_worker`
  doesn't exist yet. Reword "life of the isolate."
- **[SECURITY] SP9 replay hole:** out-param `Bytes` buffers (the §3.3 struct/out-param pattern) aren't
  recorded — a C call that writes a buffer and returns a status `int` replays with stale bytes (silent
  wrong replay). Record post-call buffer contents or refuse replay for `ffi.ptr Bytes` args.
- `u64 > i64::MAX` input-narrowing contradiction (can't pass a top-bit-set u64 if negative-to-u64
  panics). `fs::call`/`env::call` are free fns (no `&self`) — cap check must live at the `mod.rs`
  dispatch site. `ForeignSymbol` must store a raw `*mut c_void` + keep the `Library` alive (not a
  borrowed `Symbol<'lib>`). Cancel/timeout can't interrupt a live sync C call — document. Granular
  fs/net allow-within-deny must short-circuit to the bitset when no carve-out configured (Gate 12).

### SRV — APPROVE-WITH-REVISIONS  (Send-safety + GC verified sound)
- **[R1] `setup`/frozen-`args` transport into a `spawn_isolate` accept-loop isolate is unspecified** —
  `spawn_isolate`'s inbound is `Vec<u8>` only; capture the `Send` `Arc<SharedNode>` directly in the
  `Send` `make_loop` closure (cleaner than the invented `WorkerRequest.shared` side-vector).
- **[R2] "Reuse the unmodified accept loop" overstated** — `http_server_serve` takes `&self` + pulls the
  listener from per-isolate `self.resources`; it's a real `accept_loop(listener, …)` refactor with a
  per-isolate handle id. Add `socket2` as a direct dep (R3). Reuse `frozen_kind` for the mutation-panic
  message (R4). Add `assert_not_impl_any!(Value: Send)`. Split the freeze identity table into cycle
  (on-stack) vs diamond (completed) — distinct states. Global `maxRequests` needs a shared
  `Arc<AtomicUsize>` (distribution across isolates is nondeterministic). Rebase `SharedNode` onto NUM
  (Int/Float) + ADT (payload EnumVariant). Frozen-instance method call needs its own diagnostic (not the
  mutation panic).

### BIN — APPROVE-WITH-REVISIONS
- **[R1]** "Verifier always runs" is imprecise — the worker re-parse uses unverified `from_bytes` over
  already-verified bytes; state it precisely. **[R2]** Gate-12: benchmark the per-launch `current_exe`+
  footer-read on the *non-bundle* path (it's not free). **[R3]** macOS arm64 needs ≥ ad-hoc signature to
  *execute* (`codesign -s -` post-build step), not just a Gatekeeper warning. **[R4]** assert stderr too
  in the equivalence test. **[R5]** tighten the FUZZ lock from "green in CI" to FUZZ's "sustained nightly
  clean." Fix `from_bytes_verified` cite (`verify.rs:782`, not aso.rs). Add `--target`-rejection unit
  test.

### FUZZ — REVISIONS-NEEDED
- **[HEADLINE] §2.2 invariant ".aso reader never allocates unboundedly" is FALSE today** — make fixing
  the unclamped `reserve`/`with_capacity` (cross-cutting #1) an explicit FUZZ deliverable that gates BIN.
- Fix `from_bytes_verified` location (`verify.rs:782`) + return type (`FromBytesVerifiedError` wrapper).
  Make the shared `arbitrary` generator dev-only-wired (no `fuzz/`→in-tree dep; use `#[path]` include or
  a dev-dep `gen` crate). Quantify the BIN "sustained clean" bar (N nightly runs / coverage). Sequence
  NUM-dependent properties into the NUM PR; regenerate the `.aso` seed corpus on every
  `ASO_FORMAT_VERSION` bump. Pin where the planted-bug saboteur lives (`#[cfg(test)]`, asserted-off).

### DX — REVISIONS-NEEDED  (LSP-not-on-legacy-AST correction verified TRUE)
- **[A, load-bearing] The locked `BindingId` unification can't deliver frame-precise cross-file identity
  with the EXISTING `BindingId`** — `Local(TextRange)` is per-file (collides across files), `Global(String)`
  is name-only (re-introduces the coarseness it claims to remove). **Fix:** file-qualify it
  (`(FileId, TextRange)` / `(definer-FileId, name)`) — a NEW identity model, not navigation.rs:73's.
- **[B]** Fabricated workers-spec citation ("workers enable parallel test execution" — no such phrase);
  fix attribution. **[C]** "`TestSummary` is Sendable already" is FALSE (it's a Rust struct, not a
  `Value`) — encode it as a `Value::Object` across the airlock. Add the coverage-off benchmark (Gate 12).
  Coordinate the unified instrumentation hook with DBG (cross-cutting #6). Add an `examples/advanced/` DX
  artifact. Own (or explicitly disown) the campaign-wide README/landing repositioning.

### DBG — DRAFTED (new), needs review next wave
- Locked: **breakpoint-patching the bytecode** (overwrite the op byte with `Op::Break`, original in a
  side table) → not-attached loop is byte-identical, zero-cost (Gate 12 spine); `Vm.debugger: Option<…>`
  mirroring `Vm.specialize`; the 3-config benchmark is the primary acceptance gate; multi-isolate staged.
- Grounded gaps to resolve in planning: **no local-name table exists** (FnProto keeps only `params`; the
  resolver drops `let`/loop-var slot→name) — must retain names or vars show as `slot_0`; `Chunk.spans` is
  char-offset, no line table — derive line↔offset. `.aso` debug section bump is sequential (#5).
  Coordinate the unified hook with DX (#6).

## Owner decisions — RESOLVED (2026-06-08)
1. **Truthiness → `0` is now FALSY (declared breaking change).** The falsy set becomes:
   `nil`, `false`, `0` (int), `0.0`/`-0.0` (float), `NaN`, `0m` (zero decimal), and `""` (empty string).
   **Collections (array/map/set), objects, and instances stay TRUTHY even when empty** — emptiness is
   queried explicitly (`len`/`.isEmpty()`), avoiding the "valid-but-empty collection reads as no-result"
   footgun (this is the considered call on "if the correct move"; override to Python-style
   empty-collection-falsy if preferred). This is a campaign-wide truthiness change owned by NUM (it edits
   `value.rs is_truthy`): NUM §3.3 must be rewritten to this rule, §1 must list truthiness as a 4th
   breaking item, and the corpus migration (Gate 7) + a both-configs test must cover it. Rationale per
   the owner: "make the language genuinely better" — `if (count)`/`if (name)` now mean "non-zero /
   non-empty," matching C/Python/JS intuition.
2. **The live `.aso` reader bug → FIX NOW as standalone pre-req P0.** Clamp every reader allocation
   (`reserve`/`with_capacity`) with `.min(r.remaining())` (the `serialize.rs:564` pattern), TDD'd with a
   crafted huge-length `.aso` that must yield a clean `AsoError`, not abort/OOM. Lands before the
   campaign features; de-risks BIN. Tracked as P0 in `goal.md` execution order.
