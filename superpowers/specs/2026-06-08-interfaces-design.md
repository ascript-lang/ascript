# AScript Structural Interfaces / Traits — Design (IFACE)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** IFACE (capability + correctness track of the Serious Language campaign — see `goal.md`)
- **Depends on:** nothing for the **runtime conformance half** (lands first); **TYPE** for the full
  *static* conformance/assignability checking (layers on after). **NUM merge-order note:** NUM splits
  today's single `Value::Number` into `Value::Int(i64)` + `Value::Float(f64)` (renamed) and merges *before*
  IFACE (cross-cutting #5; `.aso` bumps are sequential by merge order). This spec's example signatures use
  `int` (e.g. `fn read(b: bytes) -> int`) — those names **rebase onto NUM's `int`/`float`** once it lands;
  any "a number" wording in prose means the post-NUM `int`/`float`. The runtime predicate is **type-erased**
  (arity-only, §5.1), so the NUM split touches no IFACE runtime code — purely a signature-vocabulary rebase
  in examples/docs. No hard code dependency.
- **Depended on by:** TYPE (interfaces are a type the generics layer must generalize over) and any
  abstraction-over-implementations stdlib (a `Reader`/`Writer`/`Iterator` vocabulary).
- **Engines:** both (tree-walker oracle == VM, byte-identical).
- **Breaking:** **no.** `interface` is a new top-level declaration and a new reserved-ish contextual
  keyword (§3); no existing program changes meaning. The existing nominal `instanceof` over classes is
  preserved bit-for-bit; interfaces extend the RHS it accepts.

---

## 1. Summary & motivation

AScript today has exactly one abstraction mechanism for "a family of values with shared behavior":
**class inheritance**. `instanceof` is strictly nominal — `is_instance_of` (`src/value.rs:557`) walks
the `superclass` chain by `Rc::as_ptr` identity and returns `false` for everything that is not a
`Value::Instance` of a subclass. There is no way to say "any value that *has* a `read` method," and the
checker (`CheckTy::Class`, `src/check/infer/ty.rs:64`) only knows nominal subtyping
(`is_subclass`, `table.rs:152`).

This is a real gap for a serious general-purpose language. You cannot write the single most basic
abstraction in systems code — *"a function that takes anything you can read bytes from"* — without
forcing every such type to inherit from one base class. Inheritance-only abstraction has three concrete
costs:

- **No retroactive conformance.** A type from another module (or the stdlib) cannot be made to satisfy
  your abstraction unless you can edit its `extends` clause.
- **Single-inheritance lock-in.** A type can be a `Reader` *and* a `Writer` *and* a `Closer` only if
  those collapse into one inheritance chain — which they do not, in general.
- **Coupling the abstraction to the hierarchy.** `copy(src, dst)` should care that `src` has `read` and
  `dst` has `write`, not where they sit in a class tree.

This spec adds **structural interfaces** (the Go model, with TypeScript's structural assignability and a
nod to Swift protocols for the optional explicit-conformance ergonomics): an `interface` names a
**method set**; a value **conforms** if it structurally has those methods with compatible signatures;
`value instanceof Reader` becomes a **structural conformance check**; functions take `Reader` params and
the checker proves conformance statically. No inheritance required, retroactive by construction, and a
value may conform to arbitrarily many interfaces at once.

The design is deliberately split so the **runtime conformance half ships independently of TYPE**: an
interface name resolves to a lightweight **conformance descriptor** consumed by `instanceof` (runtime,
this spec) and by the checker (`CheckTy::Interface` + `assignable`, layered on once TYPE lands). The
runtime half is a strict, additive extension of the existing `instanceof` RHS; the static half adds
*new true-positive* diagnostics only, holding the gradual gate (Gate 5).

### Two conflicts this spec resolves up front

1. **`instanceof` RHS is currently "must be a class."** Both engines hard-error on a non-class RHS through
   ONE shared free fn — `apply_binop`'s `InstanceOf` arm (`src/interp.rs:5100`, error at `:5101`); the VM
   reaches it via `eval_binop_adaptive` delegation (`src/vm/run.rs:664,3761`), not a per-op handler. IFACE
   widens the RHS to *class **or** interface*:
   the value an interface name evaluates to is a new, cheap value the `instanceof` evaluator
   recognizes. The class path is **unchanged** (still `is_instance_of`); the interface path runs a
   structural check. A genuinely-invalid RHS (a number, a string) still Tier-2-panics, byte-identically
   on both engines.
2. **No new heavyweight `Value` kind.** AScript guards its ~16-kind `Value` union jealously (`CLAUDE.md`
   §Values). An interface does **not** become a 17th first-class runtime kind with methods, vtables, and
   GC obligations. It is a **conformance descriptor** — an immutable, acyclic, `Rc`-backed handle naming
   a method set — small, `Trace`-trivial (no-op like `Native`/`Regex`), and identity-equal. Justified in
   §4.

## 2. The model: structural by default, explicit `implements` optional

There is **one user concept** — an interface is a named set of required methods — realized two ways:

| You write | Conformance is | Checked | Runtime cost |
|---|---|---|---|
| `interface Reader { fn read(b: bytes) -> int }` + any class with a matching `read` | **structural** (implicit) — the class needs no declaration | `instanceof` at runtime (this spec); TYPE statically (later) | cached per `(class, interface)` |
| `class File implements Reader { fn read(b) { … } }` | **structural, but asserted** — same predicate, plus a *checker guarantee* the class actually conforms | a **blocking diagnostic** if `implements` is claimed but not satisfied (`implements-violation`, default **Error**) | identical to the implicit case at runtime |

Both forms use the **same conformance predicate**. `implements` is **documentation + a compile-time
guarantee**, never a *requirement* for conformance and never a *runtime* tag:

- A class with a matching method set conforms **whether or not** it says `implements` (Go's model — this
  is the whole point of structural typing; retroactive conformance is preserved).
- `implements Reader` asserts intent: the checker proves the class satisfies `Reader` *at the
  declaration site* and emits a blocking diagnostic if not (Swift/Java ergonomics — you find out you
  broke conformance where you declared it, not at a far-away call site). It changes **no runtime
  behavior**: `instanceof` still runs the structural check; the result is identical.

**Method-set only for v1.** An interface declares method *signatures*. Default-method bodies are
**deferred** (§10) — recommended out of v1 unless cheap; recorded as a decision, not a silent drop.
Required *fields* on an interface are also out of v1 (an interface is behavioral, not structural-data;
field requirements are a TYPE-era extension if ever justified).

**Composition by extension.** An interface may extend others:
`interface ReadWriter extends Reader, Writer` requires the **union** of all transitively-extended method
sets. `extends` here is *interface composition* (Go embedding / Swift protocol inheritance), distinct
from class `extends` (single nominal superclass) — same keyword, unambiguous by declaration context.

## 3. Surface syntax & semantics

`interface` is a **new reserved keyword** (unlike `worker`/`step`/`as`, which are contextual). It only
ever introduces a top-level declaration; reserving it avoids a `let interface = 5` ambiguity in the
method-set body and keeps both parsers simple. (The campaign accepts keyword additions freely — pre-1.0,
backward compat is not a constraint, `goal.md`.) `implements` and the interface-composition `extends`
are **contextual** (soft keywords), exactly like the class `extends` already is
(`src/parser.rs:351` — `extends` lexes as `Tok::Ident`).

```
// declaration: a named method set
interface Reader {
  fn read(b: bytes) -> int
}

interface Writer {
  fn write(b: bytes) -> int
}

// composition: the union of Reader + Writer
interface ReadWriter extends Reader, Writer {}

// implicit (structural) conformance — File never names Reader:
class File {
  fn read(b) -> int { /* … */ return n }
}

// explicit conformance — asserted + checker-guaranteed:
class Socket implements ReadWriter {
  fn read(b) -> int { /* … */ }
  fn write(b) -> int { /* … */ }
}

fn slurp(r: Reader) -> bytes {      // any conforming value is accepted
  // …
}

let f = File()
print(f instanceof Reader)          // true  (structural runtime check)
print(5 instanceof Reader)          // false (a number has no read method)
slurp(f)                            // ok — File structurally conforms
```

Semantics:

- **An `interface` declaration introduces a binding** in the same namespace as classes/enums (a
  top-level user-global on the VM, a module-`Environment` entry on the tree-walker). Its value is a
  **conformance descriptor** (§4), printed as `<interface Reader>` (mirroring `<class Foo>`,
  `src/value.rs:886`).
- **A method requirement** is `fn name(params) -> ret` — no body. Parameters may be typed; the return
  may be annotated. `async`/`fn*`/`static`/`worker` modifiers on a requirement are **rejected in v1**
  (a parse-time error — an interface requires a plain instance method; async/generator/static
  conformance shape is a TYPE-era question). Multiple requirements are separated by newlines or `;`
  (the class-body separator rule, `skip_semicolons`).
- **`extends I1, I2, …`** on an interface composes their method sets (transitive union, deduplicated by
  name). **Interfaces forward-reference like classes/fns** (they are late-bound module-globals), so an
  `extends` name may be declared *after* the extending interface; composition is therefore resolved
  **lazily** (first use), not at declaration time (§4). A cyclic `extends` (`interface A extends B`,
  `interface B extends A`) is caught by a **runtime visited-set** at flatten time → a recoverable Tier-2
  panic `cyclic interface extends: …` (§4), and **statically** by the checker as a blocking diagnostic
  (`interface-cycle`, §6), mirroring the class-table cycle guard (`table.rs:152` terminates on a cyclic
  class `extends`).
- **`class C implements I1, I2`** asserts conformance to each. The runtime ignores it (no tag stored —
  see §4/§5); the checker proves it (§6, `implements-violation`).
- **`v instanceof I`** is a structural conformance check (§5): `true` iff `v`'s class exposes every
  method `I` requires (v1: by name + arity; TYPE tightens to signature compatibility).
- **`r: Reader` parameter / return / field / `let` annotations** are accepted everywhere a type is
  (`Type::Named` already covers an arbitrary name; the checker resolves it to an interface, §6). At
  **runtime**, an interface annotation is a **contract**, but it is **NOT** "checked exactly like a class
  today" — the class path needs a fix. The existing free `check_type(value, ty)` (`src/interp.rs:5704`)
  has **no environment**; its `Type::Named` arm (`:5744`) matches a class **by name string only** by
  walking the value's own class chain (`&c.name == name`), so it can never reach an `InterfaceDef`
  (interfaces are not on the value's class chain). The load-bearing fix is to route interface-annotated
  contract checks through an **env-aware** path, not the free fn — see §8 for the exact signature change
  and the two call paths involved. The verdict: a non-conforming value → the same Tier-2 contract panic a
  class annotation produces (`contract_panic`, `src/interp.rs:5775`). This keeps gradual contracts honest
  without TYPE.

## 4. The conformance descriptor (why no new heavyweight Value kind)

An interface name resolves to an **`InterfaceDef`** — an immutable, acyclic descriptor:

```rust
// src/value.rs (sketch)
pub struct InterfaceDef {
    pub name: String,
    /// This interface's OWN requirements (the body's `fn` signatures), keyed by name.
    pub own_methods: IndexMap<String, MethodReq>,
    /// The names of the interfaces this one `extends` (composition). Stored as NAMES,
    /// resolved LAZILY (see below) — NOT pre-flattened at declaration time.
    pub extends: Vec<String>,
    /// MEMOIZED flattened method set (own + every transitively-extended interface's),
    /// deduplicated by name. `None` until the first conformance/use; filled on first
    /// `conforms`/contract check via `flatten()` (the lazy builder), then reused.
    pub flat: RefCell<Option<Rc<IndexMap<String, MethodReq>>>>,
}
pub struct MethodReq { pub arity: usize, pub has_rest: bool, /* TYPE adds param/ret CheckTy */ }
```

**Flattening is LAZY, not eager-at-declaration (C4 — late-binding correctness).** Classes and `fn`s are
**late-bound module-globals** (forward-referenceable; a `fn` may call a later-declared `fn`). **Interfaces
are the same** (decision, recommended: interfaces forward-reference exactly like classes/fns — uniform with
the rest of the module-global namespace, and the natural consequence of binding the descriptor as a
module-global). Therefore `interface A extends B` may name a **not-yet-declared `B` at A's declaration
time**, so the transitive method set **cannot** be flattened "once at declaration time" — `B`'s descriptor
may not exist yet. Instead:
- At **declaration**, `exec`/`Op::DefineInterface` builds an `InterfaceDef` holding only `own_methods` +
  the `extends` **names** (no resolution, no flatten).
- On the **first** `conforms`/contract check against the interface, a `flatten(iface, env)` resolves each
  `extends` name via the same module-global lookup (`env.get(name)` → `Value::Interface`), recursively
  unions their (also-lazily-flattened) method sets with `own_methods` (own-wins on a name collision),
  caches the result in `iface.flat`, and returns it. Subsequent checks reuse the memo.
- **Runtime cycle guard.** Because resolution is lazy and names can mutually reference, `flatten` carries a
  **visited-`Rc::as_ptr` set**; re-entering an interface already on the stack is a **recoverable Tier-2
  panic** `cyclic interface extends: A -> B -> A` (the *runtime* analog of the checker's blocking
  `interface-cycle`, §6). This mirrors the class-table cycle guard (`table.rs:152`) but at runtime, since
  the tree-walker has no separate static table. An `extends` name that resolves to a non-interface (a class,
  a number) or to nothing is its own recoverable Tier-2 panic at flatten time.
- The `flat` memo is invalidated **never within a run** — interfaces are load-time-immortal module-globals
  (§5.3), so once flattened the set is stable. (A REPL redefinition rebinds a *fresh* `InterfaceDef` with an
  empty `flat`; the verdict cache keys on the new pointer, so no stale flatten survives — §5.3.)

and is carried as **one new `Value` arm backed by `Rc`**:

```rust
Value::Interface(Rc<InterfaceDef>),   // identity-equal, like Value::Class(Rc<Class>)
```

**Why this is *not* a "heavyweight" kind** (the design choice the prompt asks to justify):

- **No behavior, no vtable, no dispatch.** An interface value is never called, never indexed, has no
  methods of its own, never appears as a receiver. It exists only to be the **RHS of `instanceof`**, the
  resolved target of a **type annotation**, and a checker symbol. It is strictly a *descriptor*, the
  same weight class as `Value::Class` (which is also just an `Rc` you can't do arithmetic on) — not the
  weight class of `Instance`/`Future`/`Generator` (which carry mutable state, GC edges, or drive
  execution).
- **GC-trivial.** `InterfaceDef` holds only owned `String`s and an `IndexMap` of plain data — **no
  `Value` edges, no `Cc`, no cycles**. Its `Trace` is a **no-op**, exactly like `Regex`/`Native`
  (`CLAUDE.md` §Values: "acyclic/immutable handles STAY on `Rc` with no-op `Trace`"). The GC never
  traces into it; there is nothing to trace.
- **One arm, mechanically added.** It needs arms in `PartialEq` (identity via `Rc::ptr_eq`, like
  `Value::Class`, `src/value.rs:722`), `Debug`/`Display`, `type_name` (→ `"interface"`), `is_truthy`
  (→ `true`, a descriptor is truthy), and `trace` (no-op). That is the *entire* blast radius in
  `value.rs`. We reject embedding the method set in a tagged `Object` (the `std/schema`/`workflow`
  trick) because an interface must be **identity-equal** and **immutable**, and routing it through
  Object equality/mutation would be wrong; a thin dedicated arm is cleaner and safer than overloading
  Object.

The alternative — *no* new `Value` arm, resolving an interface name to a tagged `Object` descriptor à la
`std/schema` — was considered and **rejected** (§10): it muddies `instanceof`'s RHS discrimination (we'd
have to sniff a magic `__kind` on every Object RHS), loses identity equality, and is a worse fit than one
trivially-`Trace`-able `Rc` arm. The descriptor approach keeps the runtime cost of an interface at
"one `Rc` and a hash lookup."

## 5. Runtime conformance & `instanceof` (ships first, no TYPE)

This is the half that **lands independently**. It is a strict extension of the existing single source of
truth, `is_instance_of` (`src/value.rs:557`), shared by both engines.

### 5.1 The conformance predicate

```text
conforms(value, iface, env):
  let class = match value {
      Value::Instance(i) => i.class,
      _                  => return false,   // only class instances can conform in v1
  }
  let methods = flatten(iface, env)?        // §4: lazy transitive method set, memoized;
                                            // runtime cycle guard → recoverable Tier-2 panic
  for (name, req) in methods:
      let Some((method, _)) = find_method(&class, name) else { return false }
      if !arity_compatible(method, req) { return false }       // v1: min-required/declared + rest
  return true
```

`conforms` takes the **module `env`** (needed only to resolve `extends` names during the lazy
`flatten`, §4). The first call against a given interface flattens-and-memoizes; thereafter `flatten`
returns the cached set with no env work. The verdict cache (§5.3) sits *above* this, so the common hot path
touches neither.

- **v1 compatibility = name + arity** (defined precisely below). Full signature/type compatibility
  (parameter and return types) is **TYPE's job** (§6) — the *static* check tightens it; the *runtime*
  check stays cheap and gradual (a value with a `read` of compatible arity conforms at runtime, exactly
  as duck typing would; if its types are wrong, TYPE catches it statically on annotated code). This is
  the deliberate gradual seam: runtime is permissive-but-structural, static is strict-on-annotations.

- **Arity compatibility (`arity_compatible`), pinned for defaulted / optional / rest params (#6).** A
  method satisfies a requirement when the method can be **called with the requirement's argument count**.
  Concretely, for a method whose `Param` list (`src/ast.rs:165`, fields `rest: bool`, `default:
  Option<Expr>`) yields:
  - `min_required` = count of params that are neither defaulted (`default.is_some()`) nor the rest param;
  - `declared_max` = total params **minus** the rest param, or **unbounded** if a rest param is present
    (`rest` absorbs surplus args, `has_rest` fast path in `run_body`);

  and a requirement `req.arity` (the count of params the interface signature declares) with
  `req.has_rest`, the method conforms iff `min_required <= req.arity <= declared_max` (with
  `declared_max = ∞` when the method has a rest param). **Worked verdicts:** `fn read(b, opts=nil)`
  (min 1, max 2) **satisfies** `read(b)` (req.arity 1 — `1 <= 1 <= 2`) and `read(b, o)` (req.arity 2).
  `fn read(b)` (min 1, max 1) **does not** satisfy a `read(b, opts)` requirement (req.arity 2 — `2 > 1`).
  `fn read(...xs)` (min 0, max ∞) satisfies any `read(...)` arity. A requirement that itself declares a
  rest param (`req.has_rest`) requires the method to *also* be variadic (a non-rest method cannot absorb
  an unbounded tail) — the only place `req.has_rest` is consulted. This is **runtime-permissive by
  design**: it checks call-shape compatibility, not parameter *names* or *types* (TYPE adds the type
  tightening on annotated signatures).

- **Permissive-runtime vs strict-static skew is a NAMED trade-off** (like the CLAUDE.md SP1 trades). The
  runtime `conforms` deliberately checks **arity only** (a `read` of compatible call-shape conforms even
  if its `bytes`/`int` types are wrong), while TYPE's static `assignable` (§6) tightens to full signature
  compatibility on *annotated* code. So a value can pass `instanceof Reader` / a `Reader` contract at
  runtime yet draw a static `type-mismatch` — the same gradual seam the language already has (runtime
  contracts are structural-and-permissive, the checker is strict-on-annotations). Recorded so it is not
  mistaken for a bug.
- **Only `Value::Instance` can conform in v1.** Objects (bare `{}`), enums, natives, and builtins return
  `false`. Rationale: interface methods resolve through `find_method` over a class's method table; a
  bare `Object` has data keys, not methods, and "an object whose `read` *field* is a closure conforms"
  is a different (and thornier) feature we explicitly defer (§10). `instanceof` over a non-instance is
  already `false` today; we preserve that.
- **`implements` is irrelevant to the predicate.** A class conforms iff its method set matches —
  whether or not it declared `implements`. (The `implements` clause is *not* stored on the runtime
  `Class`; see §8. This keeps structural-by-default honest: a class that *forgot* to say `implements`
  still conforms, and a class that *claims* `implements` but the checker waved through still gets a real
  structural answer.)

### 5.2 `instanceof` dispatch (both engines)

`apply_binop`'s `InstanceOf` arm (`src/interp.rs:5100`) is extended to branch on the RHS kind. This is the
**single** edit site for both engines: the VM does **not** have its own `Op::InstanceOf` handler — on
`Op::InstanceOf` the VM calls `eval_binop_adaptive` (`src/vm/run.rs:664`), which for the non-arithmetic
`InstanceOf` op falls straight through to this same `apply_binop` (`src/vm/run.rs:3761`). So:

```text
match rhs {
    Value::Class(c)     => is_instance_of(&lhs, c),          // UNCHANGED nominal walk
    Value::Interface(i) => conforms(&lhs, i, env, cache),    // NEW structural check (lazy flatten + memo)
    _                   => Tier-2 panic
                           "instanceof requires a class or interface on the right-hand side",
}
```

The error message is the only change to the non-class/non-interface path (one word: "or interface"),
and it stays byte-identical across both engines (the message is the single source of truth in
`apply_binop`'s `InstanceOf` arm). The new predicate is a **`&self` method on `Interp`** rather than a
free `value.rs` fn (it needs the module `env` for the lazy `flatten`, §4, and the per-engine verdict
cache, §5.3): `fn conforms(&self, v: &Value, iface: &Rc<InterfaceDef>) -> Result<bool, Control>` (the
`Result` carries the lazy-flatten cycle / bad-`extends` Tier-2 panic). It lives beside the engine state it
needs, but is the SINGLE source of truth both engines call (the VM through the same `apply_binop`
delegation, §5.2 header) — preserving the single-source-of-truth discipline §1's box describes. The
nominal `is_instance_of` (`src/value.rs:557`) stays a free fn, env-free, unchanged.

### 5.3 Caching the verdict (cost + shape/IC interaction + GC-safety)

A naive `conforms` is `O(methods × inheritance-depth)` per `instanceof` (a `find_method` walk per
required method). For hot `instanceof Reader` in a loop that is wasteful and, worse, **non-uniform**
across engines if only one caches. We cache the **per-`(class, interface)` verdict**:

- **Key:** `(Rc::as_ptr(class) as usize, Rc::as_ptr(iface) as usize)` — both are stable identities the
  runtime already uses for `instanceof`/`find_method` (`Rc::as_ptr`, `src/value.rs:561`). The verdict is
  a pure function of those two identities (method sets are immutable post-declaration), so the cache
  never goes stale within a run.
- **Where it lives:** a `RefCell<HashMap<(usize, usize), bool>>` on the **`Interp`** (tree-walker) and
  on the **`Vm`** (VM) — *not* on the `Value`, *not* on the class shape. This mirrors how
  `user_globals`/resource tables hang off the engine root (`CLAUDE.md` §VM module-scope; the `Vm` is the
  GC root, plain owned data stays live). Both engines populate it identically, so the *observable*
  result is byte-identical regardless of cache state (the cache only changes *speed*, never the answer —
  the §9 differential guards this).
- **GC-safety (the load-bearing constraint):** the cache stores **`usize` pointer keys and a `bool`** —
  **no `Value`, no `Rc`, no `Cc`**. It therefore **holds nothing alive** and the GC never traces it.
  Because a `Class`/`InterfaceDef` is only ever freed when no live `Value` references it, a stale key can
  in principle collide only if a *new* `Rc` is allocated at the *exact freed address* AND has a
  different method set — which cannot produce a wrong answer here because **`InterfaceDef`s and `Class`es
  are created once at program load and live for the whole run** (they are module-globals; nothing frees
  them mid-run). We record this explicitly: *the cache is sound because interface/class descriptors are
  load-time-immortal*; if a future feature ever made classes/interfaces collectible mid-run, the cache
  would need a generation guard (the same `struct_gen` pattern the global cache uses) — flagged as a
  forward-looking invariant, not a v1 obligation.
- **Immortality is PER-ISOLATE, and so is the cache (#6).** The cache hangs off the `Interp`/`Vm` engine
  root, and "load-time-immortal" holds **within one isolate's lifetime**. This matters for two cases the
  campaign actually has: (a) **workers** — a `worker fn` runs on a *separate isolate* with its OWN
  `Interp`/`Vm`; the interface descriptor it uses is rebuilt there from the code-shipped closure (§8/X1),
  getting a **fresh `Rc` pointer** and a fresh (cold) cache. Pointer keys are never compared across
  isolates — each isolate's cache is private and keyed on its own descriptors — so immortality and cache
  soundness hold per-isolate, by construction. (b) **REPL** — redefining `interface R { … }` on a new line
  rebinds a *fresh* `InterfaceDef` (new pointer, empty `flat` memo); old cache entries key on the dead
  pointer and are simply never hit again (and the dead descriptor is freed once unreferenced). No
  cross-isolate or cross-redefinition staleness is possible because **a verdict is only ever read with the
  same `(class, iface)` pointers that wrote it, inside the one isolate that owns the cache.**
- **Interaction with the shape/IC machinery:** the verdict cache is **independent of** object shapes
  and inline caches. Shapes key on *field layout* (`shape_id`, `src/value.rs:327`); conformance keys on
  *method set*, which is a property of the **class**, not the instance shape — two instances of the same
  class with different field shapes have identical conformance. So we deliberately key on the **class
  pointer, not the shape id** (a shape-keyed cache would be both wrong-grained and needlessly cold).
  No IC invalidation hook is touched. We do **not** add an interface bit to the shape registry.
- **`--no-specialize` parity:** the cache is a pure memo (same answer hot or cold), so it is **active in
  both specialized and generic VM modes** — it is not a "fast path" that the kill switch disables, it is
  a correctness-neutral memo. (Contrast the IC/arith fast paths, which `--no-specialize` skips.) This
  keeps `tree-walker == specialized == generic` trivially: all three compute the same `conforms`.

## 6. Type-system integration (CheckTy::Interface — layers on with TYPE)

The runtime half (§5) ships first. This section is the **static** half, wired when TYPE lands; the
representation is designed now so TYPE only *adds rules*, never reshapes.

- **`CheckTy::Interface(InterfaceId)`** — a new lattice arm beside `CheckTy::Class(ClassId)`
  (`src/check/infer/ty.rs:64`). An `InterfaceId` indexes a new `InterfaceInfo` vector in the `Table`
  (`src/check/infer/table.rs:36`), parallel to `ClassInfo`, carrying the **flattened required method
  set** (own + `extends`-transitive) lowered to `CheckTy` signatures. **(Static flatten is eager; runtime
  flatten is lazy — intentional asymmetry.)** The checker sees every interface declaration up front, so the
  `Table` builder flattens eagerly with a visited-set cycle guard (→ `interface-cycle`, the static analog of
  the runtime guard in §4). The runtime cannot (interfaces forward-reference as late-bound module-globals,
  §4), so it flattens lazily. Both compute the **same** transitive method set; only *when* differs. `from_type_node` resolves an
  unknown `NamedType` to `Interface(id)` when `table.interface_id(name)` hits (the same lookup ladder
  that today tries `class_id` then `enum_id`, `ty.rs:110`); still `Any` if unknown (the
  zero-false-positive gradual default).
- **`assignable` — a class/instance is assignable to an interface iff it conforms.** New arms in
  `assignable_depth` (`ty.rs:346`), mirroring the existing nominal `Class → Class` rule:
  - `Class(c) → Interface(i)`: `Yes` if `c` (walking its superclass chain for methods) **provably
    conforms** — every required method exists with an **assignable** signature; `No` if a required
    method is **provably absent or provably signature-incompatible**; `Unknown` otherwise. Because the
    method-return/param types may be `Any` (unannotated methods synth `Any`, `table.rs:109`), a method
    present-but-untyped yields `Unknown`-for-that-method → overall `Unknown` → **silent** (the gradual
    gate: we only emit `No` when a method is genuinely missing or a typed signature genuinely clashes).
  - `Interface(i) → Interface(j)`: `Yes` if `i`'s method set ⊇ `j`'s and signatures are assignable
    (interface subtyping = superset of requirements); `No`/`Unknown` per the same three-valued rule.
  - `Interface(i) → Object`: `Yes` (an instance is an object at runtime, like `Class → Object`,
    `ty.rs:355`). `Interface(i) → Class(c)`: `Unknown` (a conforming value need not be that class —
    never *provably* wrong, so silent).
  - `Object → Interface`: `Unknown` (an object *might* conform — not provable; gradual-silent), exactly
    as `Object → Class` is `Unknown` today (`ty.rs:364`).
- **The `type-mismatch` code is reused** (no new diagnostic kind): passing a **provably non-conforming**
  annotated value to a `Reader` parameter is the existing "value provably wrong for an ANNOTATED slot"
  story (`CLAUDE.md` §SP10). **`implements-violation`** is a *separate*, declaration-site, **blocking
  (Error)** diagnostic: when `class C implements I` and the checker proves `C` does **not** conform (a
  required method missing or provably-incompatible), it fires *at the `implements` clause* with the
  specific missing/mismatched method — the Swift/Java ergonomic. (It is the only **Error**-level code
  IFACE introduces; the assignability diagnostics stay default-Warning like the rest of SP10.)
- **Narrowing.** `v instanceof Reader` narrows `v` to (its existing type ⊓ `Interface(Reader)`) in the
  guarded branch — the same `instanceof`/nil-guard narrowing already in `pass.rs` (`CLAUDE.md` §SP10).
  Concretely: in `if (v instanceof Reader) { … }`, inside the block `v` is treated as conforming to
  `Reader`, so `v.read(b)` type-checks against `Reader.read`'s signature. A `match` on `instanceof`
  guards narrows identically.
- **Gradual gate (Gate 5) holds.** `examples/**` emits **zero** `type-*`/`implements-*` diagnostics in
  both feature configs: structural conformance defaults to `Unknown` (silent) unless a method is
  *provably* missing or a *typed* signature *provably* clashes, and the example corpus is written to
  conform. A new corpus diagnostic is a bug in the conformance rule (default to `Unknown`, never relax
  the gate).

### 6.1 Generics on interfaces — deferred to TYPE, representation-ready

`interface Iterator<T> { fn next() -> T? }` is **out of scope for IFACE** but the representation must not
foreclose it. We reserve the slot now:

- The parser accepts (today, harmlessly) **no** type params in v1 — but the AST `InterfaceDecl` carries
  a `type_params: Vec<String>` (empty in v1), and `MethodReq`/`InterfaceInfo` carry their signatures as
  `CheckTy` (which TYPE will parameterize). Because the runtime predicate (§5) is **erased** (it checks
  method *names + arity*, never type arguments), generic interfaces need **no runtime change at all** —
  `instanceof Iterator` (raw) checks for a `next` method regardless of `T`. TYPE adds the static
  parameterization over the already-present `type_params`. Recording this keeps IFACE from painting TYPE
  into a corner: the descriptor is monomorphic-at-runtime by design, generic-at-checktime later.

## 7. Determinism & the four-mode differential

- The feature lives entirely at the **`Value`/`Interp` layer both engines share** — the `InterfaceDef`
  descriptor, the `conforms` predicate, and the `instanceof` dispatch are one source of truth: BOTH engines
  run the same free `apply_binop` (the tree-walker directly, the VM via `eval_binop_adaptive`'s delegation
  for the non-arithmetic `InstanceOf` op — there is no separate VM handler). So **`tree-walker ==
  specialized-VM == generic-VM` byte-identical holds by construction**, including the contract-panic
  text for an interface-typed annotation and the "class or interface" RHS error.
- **The verdict cache changes speed, never output** (§5.3): it is a pure memo over immortal descriptors,
  active in *all* modes (incl. generic), so the three-way differential is unaffected. A required test
  runs the same `instanceof`-heavy program with a warm and a cold cache and asserts identical output.
- **Determinism (SP9) is untouched.** Conformance is a pure function of class/interface identities — no
  clock, no RNG, no scheduling seam. The `.aso` constant pool gains an `Interface` constant kind (§8) so
  a compiled program's interface descriptors replay identically.
- **Four-mode byte-identity (Gate 1):** every interface example produces identical output on
  tree-walker, specialized VM, generic VM, and `.aso`-compiled (`tests/vm_differential.rs`, both feature
  configs) — including `instanceof` results, the contract panics, and `implements`-asserted classes
  behaving identically to structurally-conforming ones.

## 8. Implementation surface & cross-cutting subsystems

Per the `CLAUDE.md` "Touching syntax" checklist plus the interface-specific surfaces. **Every item is a
required deliverable**; the runtime-half items are the *first* milestone, the TYPE-half items
(`CheckTy::Interface`, `assignable`, `implements-violation`) land with/after TYPE.

**Values & core (`src/value.rs`):** add `Value::Interface(Rc<InterfaceDef>)` + the `InterfaceDef` /
`MethodReq` structs. The new arm needs (all compile-error-enforced exhaustive matches): `PartialEq`
(identity via `Rc::ptr_eq`, like `Class` at `:722`), `Debug` (`:772`) and `Display`
(`<interface Name>`, like `:886`), `type_name` (→ `"interface"`, the match at `:483`), `is_truthy`
(→ `true`, the match at `:687`), and GC `Value::trace` (**no-op** — acyclic, no `Value` edges). The
`conforms` predicate is **NOT** a free `value.rs` fn (it needs the module `env` for lazy flatten + the
verdict cache): it is the `&self` `Interp` method `fn conforms(&self, v, iface) -> Result<bool, Control>`
(§5.2). Only `is_instance_of` (`:557`) stays the free, env-free single source of truth for the *nominal*
walk; `conforms` is its structural sibling on the engine. **Do NOT** add an `implements` list to the
runtime `Class` struct (`:302`) — conformance is structural; storing `implements` would tempt a nominal
shortcut and is pure redundancy.

**AST (`src/ast.rs`):** a new `Stmt::Interface { name, type_params: Vec<String>, extends: Vec<String>,
methods: Vec<MethodReq>, span, name_span }` (a `MethodReq` AST node = name + params + optional ret +
spans, no body); add `implements: Vec<String>` to `Stmt::Class` (`:317`). `Type::Named` already covers
interface annotations — **no new `Type` arm** (an interface name is a `Named`, resolved at runtime by an
env lookup, statically by the checker). Exhaustive matches in `interp.rs` (exec), `fmt.rs`
(`write_stmt`), and `ast.rs` `Display` get the new `Stmt::Interface` arm (compile-error-enforced).

**Runtime contract resolution — the env-aware path (G1, load-bearing).** The free
`check_type(value, &Type)` (`src/interp.rs:5704`) is **environment-free**: its `Type::Named` arm (`:5744`)
compares `&c.name == name` up the *value's own* class chain and can never see an `InterfaceDef`. We do
**NOT** add an interface branch there — there is nothing to resolve a name against. Instead:
  - **Add an env-aware contract entry point** `fn check_type_env(&self, value: &Value, ty: &Type, env:
    &Environment) -> bool` (a `&self` method on `Interp`, so it can also read the
    interface/class binding and the §5.3 verdict cache). Its `Type::Named` arm does the resolution the
    free fn cannot: `env.get(name)` → if `Some(Value::Interface(i))` run `conforms(value, &i)` (§5.1); if
    `Some(Value::Class(c))` fall back to the existing `is_instance_of`-by-name behavior; otherwise (an
    unresolved name — a forward gradual annotation) preserve today's permissive name-string match. The
    pattern is **exactly the existing env-aware `Type::Named` arm at `src/interp.rs:4397**
    (`match (&val, env.get(name))`), which already resolves a `Named` to a `Value::Class` via `env.get` in
    `coerce_field`/`validate_into` — we generalize that same `env.get(name)` ladder to also accept
    `Value::Interface`. For composite types (`Array(elem)`, `Optional`, `Union`, `Map`, …) `check_type_env`
    recurses through itself so a nested `array<Reader>` resolves element-wise.
  - **Thread `env` to the contract call sites.** The ~8 `check_type` call sites in `interp.rs`
    (`:2484,4055,4079,4245,4335,4857,5592,5624`) are the param/return/field/`let`-annotation contract
    checks; the annotation-bearing ones that can name an interface (param/return/`let`, and class-field
    `FieldSchema.ty`) call `check_type_env` with the in-scope `Environment` they already hold. Purely
    primitive contracts (`number`, `string`, the structural `Result`/`Tuple` shapes) keep calling the free
    `check_type` (no name to resolve, no behavior change). The free `check_type` is **retained unchanged**
    for the primitive/structural cases and as the leaf the env-aware path delegates to for non-`Named` arms
    — so the only *new* behavior is "a `Named` that resolves to an interface runs `conforms`."

**Lexer/token (`src/lexer.rs`, `src/token.rs`):** `interface` → a new reserved `Tok::Interface` (like
`Tok::Class`). `implements` and the interface `extends` stay **contextual** (`Tok::Ident`, matched by
text — same treatment as class `extends`, `src/parser.rs:351`).

**Both parsers:** legacy (`src/parser.rs`) — a `interface_decl` (mirroring `enum_decl`/`class_decl`,
`:281`/`:334`) parsing the method-requirement list and `extends` composition list; extend `class_decl`
to parse an optional `implements I1, I2` clause after the optional `extends` superclass. CST
(`src/syntax/parser.rs`) — a `interface_decl(p)` (mirroring `class_decl`, `:1318`) and an `implements`
clause in `class_decl`; new `SyntaxKind`s `InterfaceDecl`, `InterfaceKw`, `MethodReq`/`MethodReqList`,
`ImplementsClause`, `ExtendsList` (`src/syntax/kind.rs` — register in the `is_type`/node lists as
needed). Reject `async`/`fn*`/`static`/`worker` on a requirement (parse error). **Frontend conformance**
(`tests/frontend_conformance.rs`) proves the two front-ends agree.

**Tree-sitter (`tree-sitter-ascript/grammar.js`):** add `interface_declaration` (name, optional
`extends` composition list, body of method-requirement signatures) parallel to `class_declaration`
(`:234`); add an optional `implements` clause to `class_declaration`; a `method_requirement` rule
(signature, no block). Add `interface`/`implements` to the keyword set. Regenerate `parser.c`
(`tree-sitter generate --abi 14`). Update `queries/highlights.scm` (tag `interface`/`implements` as
keywords; interface names as types). **Publish** (mandatory per `CLAUDE.md`/CONTRIBUTING whenever
`tree-sitter-ascript/**` changes): `./scripts/sync-grammar.sh` (subtree-split + push to the
`ascript-lang/tree-sitter-ascript` mirror; prints the SHA), then bump that SHA in
`editors/zed/extension.toml` (`commit`) and `editors/nvim/lua/ascript/treesitter.lua` (`revision`).

**Editor integrations (`editors/`):** add `interface`/`implements` to (a) the VS Code TextMate grammar
`editors/vscode/syntaxes/ascript.tmLanguage.json` (keyword/storage pattern — TextMate runs independent
of the LSP); (b) the bundled `editors/zed/languages/ascript/highlights.scm` and
`editors/nvim/queries/ascript/highlights.scm` copies; extend
`editors/nvim/tests/treesitter_spec.lua` if it asserts on keyword tokens.

**Both engines — `instanceof` routes through ONE shared `apply_binop` arm (not a per-op VM handler).**
The previous draft miscited a VM `Op::InstanceOf` *handler* at `src/vm/run.rs:4292`; that line is the
`binop_of` **opcode→`BinOp` mapping table** (`Op::InstanceOf => BinOp::InstanceOf`), not a handler. Both
engines actually converge on the SAME free function: the tree-walker calls `apply_binop`
(`src/interp.rs:5076`) and the VM, on `Op::InstanceOf`, calls `eval_binop_adaptive` (`src/vm/run.rs:664`),
which for a non-arithmetic op like `InstanceOf` immediately delegates to the same `apply_binop`
(`src/vm/run.rs:3761`). So the **only** place the class-vs-interface RHS branch is added is `apply_binop`'s
`InstanceOf` arm (`src/interp.rs:5100`, the `Value::Class(cls)` match + the `:5101` error message) — one
edit, automatically byte-identical on both engines. *This strengthens the byte-identity claim*: there is
no second VM code path to keep in sync. Tree-walker — `exec` handles `Stmt::Interface` (build the LAZILY-
flattened `InterfaceDef` (§4), bind it as a module-global). VM — a `Op::DefineInterface` (or reuse the
const-pool: emit the descriptor as a constant and `DEFINE_GLOBAL` it, mirroring how a class is defined)
builds + binds the descriptor. The contract path on both engines routes a `Named` annotation that resolves
to an interface through `check_type_env` (the env-aware path above). **No new arithmetic/IC opcode**, and
**no per-op `InstanceOf` handler is touched** — interfaces never participate in fast-pathed arithmetic.

**`.aso` (`src/vm/aso.rs` + `src/vm/verify.rs`):** the constant pool gains an `Interface` constant kind
(name + own method-requirement set + `extends` names — serialize the **unflattened** form, since flatten is
lazy at load and the `extends` targets are themselves module-globals that reload; §4); a class's serialized
layout gains the `implements` name list (checker metadata; the runtime ignores it but the checker reads it
from a loaded `.aso` for cross-module `implements-violation`). **Bump `ASO_FORMAT_VERSION` by one**
(currently **18**, `src/vm/aso.rs:105`) — but the bumps are **sequential by merge order** (NUM/ADT/IFACE
each +1, cross-cutting #5), so **do not hardcode 19**: IFACE's value is `<whatever-the-prior-merge-left> +
1` at merge time. Update
`src/vm/verify.rs` (verify the interface constant's method-set well-formedness — no duplicate names,
non-empty names). The new `Interface`-constant **reader** (`read_value`/`read_type` analog in `aso.rs`)
must clamp every attacker-controlled length (method-count, name lengths) with `.min(r.remaining())` like
the worker serializer does (`serialize.rs:564`) — cross-cutting #1's unbounded-`reserve` hazard applies to
any new variable-length `.aso` reader, so the method-set vector is `Vec::with_capacity(n.min(r.remaining()))`,
not `Vec::with_capacity(n)`.

**Worker code-shipping closure (`src/worker/dispatch.rs`) — the X1 deliverable.** An `InterfaceDef` is
**code, not data** — like a `Function`/`Class`, it rides the **code-shipping closure** (Workers Spec A §6),
not the structured-clone message channel. The closure walker is a global-**NAME** fixpoint
(`collect_get_global_names`/`collect_def_refs`, `dispatch.rs:295,355`) over a per-top-level-binding
classification (`classify_binding`, `:254`). **Today it already ships top-level classes** (a run ending in
`Op::Class` → `TopDef::Class`, `:285`, emitted via `emit_class_recursive`) **and enums** (which compile to
a value const → `TopDef::Const`, shipped by `emit_dep_closure`). So a `worker fn` doing
`x instanceof Reader` already emits `GET_GLOBAL Reader` and the name enters the fixpoint set — but the
**classifier has no arm for an interface define-op**, so `Reader` resolves to `None`/`ComputedConst` and
its descriptor never ships. **The X1 fix (real gap, three edits):**
  - **`TopDef::Interface`** — a new arm on the `TopDef` enum (`dispatch.rs:71`), parallel to
    `TopDef::Class`. A top-level interface binding compiles to a run ending in `Op::DefineInterface` (or the
    const-pool descriptor + `DEFINE_GLOBAL`, §8 VM); whichever lowering §8 picks, the classifier recognizes
    it.
  - **Classifier arm** in `classify_binding` (`:254`): a run ending in the interface-define op →
    `TopDef::Interface` (mirroring the `Some((Op::Class, _)) => TopDef::Class` arm at `:285`). Update the
    `top_level_defs` doc-comment table (`:97`) to list it.
  - **`collect_def_refs` arm** (`:355`): an interface's transitive top-level dependencies are **its
    extended interfaces** (the `extends` names) — `TopDef::Interface => out.extend(extends_names)`. This is
    the lazy-flatten dependency edge (§4), so shipping `ReadWriter` pulls in `Reader` + `Writer`. (Method
    *signatures* carry no executable bodies — no `GET_GLOBAL`s to walk, unlike a class's method table.)
  - **Emit site:** add an `emit_dep_closure`-side branch (or an `emit_interface_recursive` paralleling
    `emit_class_recursive`) that, for each included `TopDef::Interface` name, re-emits its define op +
    `DEFINE_GLOBAL` into the fragment, so the worker isolate rebuilds the descriptor (fresh `Rc`, fresh
    cache — §5.3 per-isolate immortality). The `emit_dep_closure` `match` over `TopDef` (`:802`) gains the
    `Interface` arm (today it has `Const`/`Fn`/`ComputedConst`/`Class|None`).

  **Nested (non-top-level) interfaces remain a documented non-goal** — exactly as nested classes/enums are
  today (the closure walker only classifies DIRECT-child top-level bindings); an interface declared inside a
  fn body does not ship. A `worker fn` that references one is a clean "unknown global" the same way a nested
  class would be.

**Worker serializer exhaustive arms (`src/worker/serialize.rs`) — the C5 deliverable.** `Value::Interface`
is **non-sendable as a value** (it is code), so the serializer must list it everywhere `Value` is matched
exhaustively, or the new arm is a compile error / a runtime `unreachable!` trap:
  - **`unsendable_kind` (`:109`)** — add `Value::Interface(_) => Some(("interface", None))` (alongside
    `Function`/`Closure` → `"function"`). This is what makes `check_sendable` (`:103`) reject it with a
    field path, message analog `value of kind interface cannot be sent to a worker at <path>` (the
    `DataCloneError` analog, Spec A §5).
  - **`encode_value` (`:380`)** — because `check_sendable` rejects interfaces *before* `encode_value` runs,
    `Interface` falls into the catch-all `other => unreachable!(...)` (`:500`). That `unreachable!` is the
    **trap the review flags**: it is only sound *because* `unsendable_kind` covers the kind. The deliverable
    is the `unsendable_kind` arm above (which keeps `encode_value` total over sendables); we explicitly do
    **not** add an `encode_value` arm (interfaces never reach it), and we add a serializer unit test
    asserting `check_sendable(Value::Interface(...))` errors so the `unreachable!` can never be reached.
  - **`decode_value`** — no `Interface` tag is ever written (encode never emits one), so the decoder needs
    **no** `Interface` case; a stray tag is the decoder's existing "unknown tag" error. (Recorded so the
    absence is intentional, not an oversight.)

So an interface round-trips via **code-shipping** (the closure above), never the value serializer; a *value*
of kind `Interface` as a worker arg/result is the non-sendable Tier-2 panic.

**Checker & types (`src/check/`):** *(TYPE-era)* `Table` gains `InterfaceInfo` + `interface_id`/
`interface(id)` + a flattened-method-set builder with a cycle guard (`interface-cycle`);
`CheckTy::Interface`; the `assignable` arms (§6); `instanceof`-interface narrowing in `pass.rs`; the
**`implements-violation`** rule (default **Error**) added to `rules::ALL`; the existing `type-mismatch`
reused for non-conforming annotated values. `src/check/std_arity.rs` unaffected (no new script-exposed
stdlib fns). **Fixable codes** (`src/check/fix.rs`): `implements-violation` is **not** auto-fixable
(generating method stubs is out of scope) — documented.

**LSP (`src/lsp/`):** **semantic tokens** — `interface`/`implements` as keywords, interface names as
types. **Hover (`infer::hover_type_at`, `src/check/infer/mod.rs:37`)** — hovering an interface name shows
`<interface Reader>` and its method set; hovering a `Reader`-typed binding shows `Reader`; hovering a
method *call on an interface-typed receiver* shows the requirement signature. **Go-to-definition** — an
interface method *call site* (`r.read(...)` where `r: Reader`) navigates to the **interface requirement**
declaration; a `Reader` type annotation / `implements Reader` clause navigates to the `interface Reader`
declaration (`src/lsp/workspace.rs` `WorkspaceIndex` indexes the interface decl + its requirements as
symbols). **Find-references / rename** — over an interface name and its requirement names (the existing
name-index covers them once the parser emits the decl; add LSP tests). **Completion** — offer `interface`
as a top-level declaration keyword and `implements` after a class header; offer interface names in type
position; offer the **required method names** when completing inside a `class … implements I` body (a
"stub the interface" affordance — *completion only*, no codegen).

**Formatter (`src/fmt.rs` + `ast.rs` `Display`):** render `interface Name extends A, B { fn m(...) ->
T }` canonically (requirements one-per-line, `extends` list comma-joined); render a class's `implements
A, B` clause in canonical order `class C extends Super implements A, B { … }` (after `extends`, before
the body). Idempotent; add formatter goldens. `ast.rs` `Display` renders identically to the formatter.

**REPL (`src/repl.rs`):** an `interface { … }` body uses braces → existing delimiter-depth
`is_incomplete` buffering handles multi-line entry; cross-line persistence binds the descriptor on the
session `Vm`/`Interp` like any top-level decl. Add a regression test (`interface R { fn read(b)->int }`
then `class F { fn read(b) { return 0 } }` then `F() instanceof R` → `true`).

**Docs:** a new **"Interfaces"** section in `docs/content/language/classes-enums.md` (structural
conformance, optional `implements`, `instanceof Interface`, composition via `extends`, the
runtime-vs-static split, and the deferred default-methods/fields/generics notes) — this page already
exists, so **no `NAV` change** (the docs-nav orphan gotcha applies only to *new* pages). Update
`docs/content/language/type-contracts.md` (an interface annotation as a runtime contract). Update
`README.md` (capabilities table — "structural interfaces" moves from the IFACE roadmap row to shipped).
Update `CLAUDE.md` (a §"Language features" interface note + the `Value` paragraph for the new arm) and
the campaign `goal.md`/`roadmap.md` status. Serve-site sanity check (`cd docs && python3 -m
http.server`).

**Tests:** `frontend_conformance.rs`, `treesitter_conformance.rs`, `vm_differential.rs` (both configs,
four modes), `check.rs` (`implements-violation`, `interface-cycle`, narrowing, zero corpus
false-positives — *TYPE era*), `lsp.rs` (tokens/hover/go-to-def/completion), plus the unit/integration
tests in §9.

**Unchanged:** `Value`'s `Rc`/`Cc` discipline (the new arm is `Rc`, no-op `Trace`); `Interp` async
model; the GC (an interface descriptor is GC-trivial); the worker pool/scheduler; all non-interface
stdlib; the single-threaded hot path; the nominal `instanceof` over classes (bit-for-bit preserved).

## 9. Testing, example corpus & migration

### 9.1 Unit & checker tests (the no-bugs pillar)
- **Parser (both front-ends):** `interface` decl with 0/1/N requirements; `extends A, B` composition;
  a class with `extends Super implements A, B`; rejecting `async`/`fn*`/`static`/`worker` on a
  requirement; `;`-separated requirements; frontend-conformance agreement.
- **Conformance predicate (`conforms`):** a class with a matching method conforms; a missing method →
  `false`; the §5.1 arity table — `fn read(b, opts=nil)` satisfies `read(b)` (defaulted param) and
  `read(b, o)`; `fn read(b)` does NOT satisfy a 2-param `read(b, opts)` requirement; a `rest`-param method
  satisfies any arity; a `rest`-param *requirement* needs a variadic method; inheritance — a method
  inherited from a superclass satisfies a requirement; a non-instance LHS (number, object, enum, nil) →
  `false`; `implements`-declared and structurally-only-conforming classes give the **identical** verdict.
- **`instanceof` dispatch:** `x instanceof Class` unchanged (regression); `x instanceof Interface`
  structural; a non-class/non-interface RHS Tier-2-panics with the new message, byte-identical on both
  engines.
- **Cache:** warm-vs-cold verdict identical; a cache is a pure memo (property: `conforms(c,i)` ==
  cached value for any access order); active in `--no-specialize`.
- **Composition + lazy flatten:** `ReadWriter extends Reader, Writer` requires both methods; a transitive
  (`extends` of an `extends`) flattens; a **forward-referenced** `extends` (`interface A extends B` where
  `B` is declared *after* `A`) resolves correctly (proves flatten is lazy, not eager-at-declaration); a
  cyclic `extends` → the runtime `cyclic interface extends` Tier-2 panic AND (TYPE era) the static
  `interface-cycle`, both terminating; a memoized flatten is reused (warm) and identical to cold.
- **Contract (env-aware path):** a `Reader`-annotated `let`/param/field rejects a non-conforming value
  with the same Tier-2 contract panic a class annotation produces (via `check_type_env` → `conforms`);
  accepts a conforming value; a `Reader` name that resolves to a class still nominal-checks; an unresolved
  name stays permissive (gradual). Nested `array<Reader>` resolves element-wise.
- **Checker (TYPE era):** `class C implements I` missing a method → blocking `implements-violation` at
  the clause; a provably-non-conforming value to a `Reader` param → `type-mismatch`; an untyped method
  present → `Unknown` → **silent**; `instanceof` narrowing; **`examples/**` emits zero
  `type-*`/`implements-*`** in both configs.

### 9.2 Four-mode byte-identity (REQUIRED)
Every interface example runs identically on tree-walker, specialized VM, generic VM, and `.aso`-compiled
(`tests/vm_differential.rs`, both feature configs) — including `instanceof` results, the contract
panics, and `implements`-asserted vs structurally-conforming classes producing identical behavior.

### 9.3 Example corpus (`examples/` — runnable, doubles as docs & four-mode tests)
The examples are **split across the two milestones** (Gate 9), so the runtime half ships a complete, gated
corpus without waiting on TYPE:

**Runtime-half milestone (ships first, no TYPE):**
- **`examples/interfaces.as`** — the canonical proof: a `Reader`/`Writer` pair and a generic
  `fn copy(src: Reader, dst: Writer) -> int` driven over **multiple conforming types** (e.g. an
  in-memory `BufferReader`, a `RepeatReader`, a `CountingWriter`, a `NullWriter`) — some declaring
  `implements`, some conforming purely structurally — showing one function abstracting over all of them,
  with `instanceof Reader` guards in the body, and a `ReadWriter extends Reader, Writer` composition.
- **`examples/advanced/interface_dispatch.as`** — a production-shaped, fully error-handled example:
  e.g. a `Codec` interface (`encode`/`decode`) with two implementations selected at runtime via
  `instanceof`, returning `[value, err]` Results.
- These exercise ONLY runtime-half behavior (declaration, `instanceof Interface`, the runtime contract
  panic on a non-conforming `Reader`-annotated value, composition) — they run green on the four engines the
  moment the runtime half lands, and emit no diagnostics (they don't depend on the static checker).

**TYPE-half milestone (lands with/after TYPE):**
- The **`implements-violation` edge is TYPE-era** — it is a *static* blocking diagnostic, so its negative
  example (a `class C implements Reader` that is **missing** `read`) lives in `tests/check.rs`, NOT in
  `examples/**` (the corpus must stay diagnostic-clean for Gate 5). Likewise the `type-mismatch`-on-a-
  `Reader`-param case and `instanceof` narrowing are `check.rs` fixtures.
- A TYPE-era *positive* example may be added to `examples/**` showing a statically-checked interface-typed
  function — written to **conform** so the gate (zero `type-*`/`implements-*` in both configs) holds.

No existing example/golden changes meaning (IFACE is additive) — but any example that *would benefit*
from an interface (e.g. a hand-rolled duck-typed dispatch) may be migrated for documentation value,
reviewed as corpus churn.

### 9.4 Performance — `instanceof Class` must NOT be taxed (Gate 12)
The class path is the common case and IFACE adds an `Interface` arm to the **shared** `apply_binop`
`InstanceOf` dispatch (§5.2). The risk is that the added match arm slows the hot `instanceof Class` path.
Deliverable: an **`instanceof Class` micro-benchmark** (a tight loop of `x instanceof SomeClass` over class
instances) asserting **no steady-state regression** vs the pre-IFACE baseline, in **both** specialized and
generic (`--no-specialize`) VM modes. The branch is a single `match` on the already-loaded RHS `Value`
discriminant (`Class` vs `Interface` vs other) ahead of the unchanged `is_instance_of` call, so the
expectation is a flat profile; the bench commits that expectation (mirroring the SP8/Gate-12 discipline).
A paired `instanceof Interface` bench (warm vs cold verdict cache) confirms the cache earns its keep.

### 9.5 FUZZ hook (continuous infra)
The conformance predicate + cache is a target for the FUZZ spec's differential fuzzer: generate random
class/interface method sets and assert `tree-walker == specialized == generic` on `instanceof`, and that
warm/cold cache verdicts agree. IFACE lands the §9.1 property tests; FUZZ generalizes them.

## 10. Scope & rejected alternatives

**In scope:** the `interface` declaration (method-set requirements); structural conformance by default;
optional explicit `class … implements I` (documentation + a blocking `implements-violation` guarantee);
interface composition via `extends`; the `Value::Interface` conformance descriptor (one `Rc` arm, no-op
`Trace`); `instanceof Interface` (structural runtime check + per-`(class, interface)` verdict cache);
interface type annotations as runtime contracts; the `.aso` + worker-code-shipping integration; the
`CheckTy::Interface` + `assignable` + narrowing + `implements-violation` static layer (TYPE era, designed
now); docs/examples.

**Out of scope (reserved/deferred — recorded decisions, not silent drops):**
- **Default method bodies** on interfaces. **Deferred** (the prompt's recommendation): they add a
  dispatch story (which body runs — the interface default or a conforming class's override?), interact
  with `super`, and are not needed for the core abstraction. Recorded decision: **v1 is method-set
  signatures only**; defaults are a clean additive follow-up (an interface requirement gains an optional
  body; a conforming class without that method inherits the default at call time) **if justified**, with
  its own mini-spec.
- **Required fields / properties** on an interface. Out of v1 — interfaces are behavioral. A
  structural-data requirement is a TYPE-era extension if ever justified.
- **Generic interfaces** (`interface Iterator<T>`). **Deferred to TYPE** (§6.1); the AST/`InterfaceInfo`
  reserve a `type_params` slot and the runtime predicate is erased, so adding them later is purely a
  checker change.
- **Bare-object conformance** (an `Object` whose keys are closures conforming to an interface). Out of
  v1 — conformance resolves through a class's method table (`find_method`); duck-typing arbitrary objects
  is a separate feature with its own edge cases. Documented, not silently dropped.
- **`async`/generator/`static`/`worker` requirements.** Rejected for v1 (a parse error) — the conformance
  shape for async/generator/static methods is a TYPE-era question; v1 requires plain instance methods.

**Rejected:**
- **Nominal-only traits (Java/C#-style: conformance requires an explicit `implements`).** Rejected — it
  forfeits retroactive conformance (the central reason to add interfaces) and forces editing a type's
  declaration to make it satisfy an abstraction. We adopt **structural-by-default** (Go/TypeScript), with
  `implements` as an *optional assertion*, not a *requirement*.
- **Inheritance-only abstraction (the prior non-goal — now reversed).** The main design spec's model was
  "abstraction = class inheritance." `goal.md` explicitly lists structural interfaces as a pillar-3
  capability; this spec **reverses the non-goal**. Single-inheritance abstraction cannot express
  "Reader *and* Writer *and* Closer," cannot be retroactive, and couples the abstraction to the
  hierarchy. Interfaces are orthogonal to inheritance and compose freely.
- **A heavyweight first-class interface `Value` (with methods/vtable/dispatch).** Rejected (§4) — an
  interface is a *descriptor*, never a receiver; a behavior-bearing kind would balloon the `Value` union
  and the GC surface for no capability gain.
- **No new `Value` arm — resolve interfaces to a tagged `Object` (the `std/schema`/`workflow` trick).**
  Rejected (§4) — it loses identity equality, forces `instanceof` to sniff a magic `__kind` on *every*
  Object RHS, and is a worse fit than one trivially-`Trace`-able `Rc` arm.
- **Storing the `implements` list on the runtime `Class` and short-circuiting `instanceof` to a nominal
  tag check.** Rejected — it would make `implements` *load-bearing at runtime*, breaking
  structural-by-default (a class that forgot `implements` would wrongly fail `instanceof`). `implements`
  is checker-only metadata; the runtime is always structural.
- **Shape-keyed conformance cache.** Rejected (§5.3) — conformance is a property of the *class method
  set*, not the *instance field shape*; keying on `shape_id` would be wrong-grained (two shapes of one
  class share conformance) and needlessly cold. We key on the **class pointer**.

## 11. Grounding (verified sources)

- **Structural conformance, no explicit declaration, retroactive** — Go interfaces (the Go spec
  "Interface types" + "a type implements an interface by implementing its methods"); Go's `io.Reader`/
  `io.Writer`/`io.ReadWriter` (composition via embedding) is the direct model for §2/§3.
- **Structural assignability of object/class types** — TypeScript's structural type system (a value is
  assignable to an interface iff it has compatible members) — the §6 `assignable` rule.
- **Optional explicit conformance + declaration-site guarantee** — Swift protocols (`struct S: P`
  asserted at the declaration, a compile error if unsatisfied) and Java/C# `implements`/`:` — the
  ergonomic basis for the optional `implements` clause + `implements-violation` (§2, §6).
- **Protocol/interface composition** — Swift protocol inheritance (`protocol RW: R, W`) and Go interface
  embedding — the §2 `extends` composition (transitive method-set union).
- **AScript-internal grounding (cited inline):** `is_instance_of` nominal walk (`src/value.rs:557`);
  `Value::Class` identity equality (`:722`) and `<class …>` display (`:886`) as the descriptor template;
  the `instanceof` RHS-must-be-class error both engines share via ONE shared dispatch — the free
  `apply_binop` (`src/interp.rs:5076`, the `InstanceOf` arm + error at `:5100`/`:5101`); the VM reaches it
  through `eval_binop_adaptive`'s delegation (`src/vm/run.rs:664,3761`), NOT a per-op handler (`run.rs:4292`
  is the `binop_of` opcode→`BinOp` table, not a handler); the env-free `check_type` and its name-string-only
  `Type::Named` arm (`src/interp.rs:5704,5744`) plus the env-aware `Type::Named` precedent in `coerce_field`
  (`src/interp.rs:4397`, `match (&val, env.get(name))`); `contract_panic` (`src/interp.rs:5775`);
  `CheckTy::Class` + nominal `assignable` (`src/check/infer/ty.rs:64,346`) and the
  `Table` class/enum lookup ladder (`table.rs:110,152`); the no-op-`Trace` acyclic-handle discipline and
  the engine-root memo pattern (`CLAUDE.md` §Values, §VM module-scope); `ASO_FORMAT_VERSION = 18`
  (`src/vm/aso.rs:105`); the worker code-shipping closure vs. structured-clone boundary
  (`2026-06-07-workers-foundation-stateless-design.md` §5–6).
