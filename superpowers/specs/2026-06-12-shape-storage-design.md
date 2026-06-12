# Shape-Native Object Storage + Compile-Time Literal Shapes + Interior Hashing — Design (SHAPE)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** SHAPE (PERF campaign, `goal-perf.md` — the Representation wave, first half)
- **Depends on:** nothing in the new campaign (parallel-able with LANE; consumes LANE's Task-0
  bench-corpus extension once merged for the functional-idiom A/B, but does not wait on it).
- **Depended on by:** **NANB** (the representation endgame waits for object internals to
  stabilize — `goal-perf.md` execution order: "NANB starts only after SHAPE merges"), **JIT**
  (shape-guarded slot loads become native loads).
- **Engines:** VM **storage representation** only. The tree-walker keeps its current
  `IndexMap` representation untouched — byte-identity is about OBSERVABLE behavior (iteration
  order, json output, panic messages, exit codes), asserted by the four-mode differential. A
  divergent internal representation with identical behavior is the POINT: the oracle proves the
  new storage by disagreeing with it the instant order or values drift.
- **Breaking:** **no.** No syntax change, no semantics change, no opcode change, no `.aso`
  change (`ASO_FORMAT_VERSION` unchanged by THIS spec — 27 at drafting, `src/vm/aso.rs:167`;
  SHAPE runs in a branch parallel to DEFER, which bumps it to 28, so the negative-space test is
  merge-base-relative, §8.1). Object construction opcodes
  (`NewObject`/`AppendObject`/`SpreadObject`, `src/vm/opcode.rs`) are byte-identical — shapes
  remain a per-`Vm` runtime side concern, exactly as V11 established; the only new state is a
  per-chunk **side table** following the `field_ics`/`arith_caches` offset-keyed precedent
  (`src/vm/chunk.rs:421-435`).

---

## 1. Summary & motivation (evidence)

AScript already has hidden classes. V11 built a per-`Vm` `ShapeRegistry`
(`src/vm/shape.rs:28`) that assigns a shape id to every ordered key layout via a memoized
transition tree, and polymorphic inline caches (`src/vm/ic.rs`) that warm a `GET_PROP` site to
`InlineCache::Mono { shape, index }` (`src/vm/ic.rs:60`) and read a field with one integer
compare plus `values.get_index(index)` (`src/vm/run.rs:4766-4772`). What V11 did **not** do is
change the storage: an object is still `Cc<ObjectCell { map: RefCell<IndexMap<String, Value>>,
shape: Cell<u32>, frozen: Cell<bool> }>` (`src/value.rs:24-32`), an instance is still
`fields: IndexMap<String, Value>` beside a `shape_id: Cell<u32>` (`src/value.rs:409-421`).
The shape is **metadata beside a hash table** instead of the owner of the layout. The measured
and code-confirmed consequences (Phase-0 profiling, `bench/PROFILING_RESULTS.md`; constant
factors re-verified against source 2026-06-12):

- **Hashing is 11–13% of the json/object workloads** — `json_roundtrip` 11%, `object_churn`
  13% (SipHash + hashbrown rehash; `bench/PROFILING_RESULTS.md` CPU attribution table) — and
  **allocation is 22–38%** (`object_churn` 22%, `json_roundtrip` 38%).
- **Every object literal hashes every key at construction although the keys are statically
  known.** `Op::NewObject` (`src/vm/run.rs:2296`) pops `n` (key, value) pairs into a temp
  `Vec<(Rc<str>, Value)>` (`run.rs:2305`), then `map.insert(k.to_string(), v)` per key
  (`run.rs:2320-2323` — one `String` allocation + one SipHash per key + the `IndexMap`'s own
  two-allocation table), and **then walks the keys through the registry anyway** to derive the
  shape (`run.rs:2327`). The compiler knew the full key list at `compile_object`
  (`src/compile/mod.rs:5525-5549`); the runtime re-derives it from scratch on every single
  construction. `object_churn` constructs 6M objects this way.
- **`resync_object_shape` clones every key on every resync.** `src/vm/run.rs:5384-5388`:
  `let keys: Vec<String> = obj.map.borrow().keys().cloned().collect()` then a full
  transition-tree walk from the empty shape — on every `SET_PROP` miss (`run.rs:5453`), every
  `SET_INDEX` on an object (`run.rs:2557-2559`), every `APPEND_OBJECT` (`run.rs:2462`), and
  every `SPREAD_OBJECT` (`run.rs:2502`). `resync_instance_shape` (`run.rs:5397-5401`) does the
  same for instances. (`goal-perf.md` cites this at `run.rs:4526`; the function has since moved
  to `run.rs:5384` — same code, verified.)
- **The registry itself allocates on every transition lookup, even a hit:**
  `ShapeRegistry::add_key` keys its map with `(u32, Box<str>)` and builds a fresh
  `Box::from(key)` just to probe (`src/vm/shape.rs:58`).
- **Inserts rehash.** Adding a key to an `IndexMap` hashes it (SipHash, the std default
  `RandomState` — `indexmap = "2"`, `Cargo.toml:41`, no custom hasher anywhere in
  `src/value.rs`), and growth rehashes the table.

The campaign completes what V11 started: **the shape owns the key→index layout; the object
stores a flat values slab.** A property read is (IC shape check) + `slab[index]` — no hash, no
`IndexMap`. A literal construction is (per-site cached shape) + popping values into a
pre-sized `Vec` — zero hashing, zero per-key inserts, one allocation. The `IndexMap`
representation survives as an explicit per-object **dictionary fallback mode** (V8's
dictionary-mode precedent) for the shapes-defeating cases, preserving exact insertion-order
semantics. Interior hash tables that never carry attacker-controlled keys at DoS-relevant
scale move from SipHash to FxHash; user-facing `Map`/`Set` and dictionary-mode objects keep
SipHash as a documented security decision (§6).

### 1.1 The split-brain is not just slow — it is a live bug source (found during drafting)

The current design stores the layout twice (the `IndexMap`'s own order AND the shape id) and
relies on every mutation site remembering to resync. One site forgot: `object.delete`
(`src/stdlib/object.rs:254`) does `shift_remove` **without touching the shape**, so a warmed
IC keyed on the stale shape serves the **wrong slot**. Verified live divergence (2026-06-12):

```as
import * as object from "std/object"
let o = {a: 1, b: 2, c: 3}
fn get_b(x) { return x.b }
print(get_b(o))          // warms the GET_PROP IC: Mono { shape(a,b,c), index 1 }
object.delete(o, "a")    // map is now {b:2, c:3}; shape STILL says (a,b,c)
print(get_b(o))          // IC hit → get_index(1) → ("c", 3) — WRONG VALUE
```

Specialized VM prints `2` then `3`; tree-walker and generic VM print `2` then `2`. The IC's
"defensive stale index" guard (`run.rs:4773-4775`) only catches **out-of-range** indices, not
shifted in-range ones. This is fixed FIRST, in-branch, failing-test-first (plan Phase 0), on
the **current** representation (`object.delete` demotes the shape to 0 — the never-cached
sentinel). The new storage then eliminates the bug **class** structurally: key removal is a
mode transition the storage itself performs, not a side effect a distant stdlib function must
remember to mirror (§3).

## 2. Mechanism 1 — shape-owned layout, slab storage

### 2.1 The registry owns the canonical key list per shape

`ShapeRegistry` (`src/vm/shape.rs`) already IS the layout authority — a shape id reached by
the transition walk `(s, k) → child` uniquely determines the ordered key list. SHAPE makes
that ownership explicit and queryable:

```rust
pub struct ShapeRegistry {
    /// parent shape → (key → child shape). Two-level so a lookup probes with a
    /// borrowed `&str` (Borrow<str>) — kills the per-probe `Box::from(key)`
    /// allocation at shape.rs:58. FxHash: §6 (compile-time/bounded keys).
    transitions: FxHashMap<u32, FxHashMap<Box<str>, u32>>,
    /// shape id → its canonical ordered key list, `Rc`-shared with every object
    /// of that shape (and with the literal-site caches). Dense, id-indexed —
    /// `keys_of` is an array index, not a hash probe. keys[0] = the empty list.
    keys: Vec<Rc<[Rc<str>]>>,
}
impl ShapeRegistry {
    /// Some(child) — or None when the parent's fan-out cap or the slab key cap
    /// is exceeded (the caller demotes the object to dictionary mode, §3).
    pub fn add_key(&mut self, shape: u32, key: &str) -> Option<u32>;
    pub fn shape_for<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) -> Option<u32>;
    pub fn keys_of(&self, shape: u32) -> Rc<[Rc<str>]>;
}
```

### 2.2 The object stores a slab — and stays self-describing

```rust
/// The interior of ObjectCell (and of Instance.fields). The Value SURFACE is
/// unchanged — Value::Object(Cc<ObjectCell>) stays; only the cell's interior
/// changes (rejected alternative: a new Value variant, §10).
pub enum ObjectStorage {
    /// Shape-native mode. `keys` is the registry's canonical Rc for this object's
    /// shape (shared, immutable — one allocation per LAYOUT, not per object);
    /// `values[i]` is the value of `keys[i]`. Invariant: keys.len() == values.len(),
    /// and (cell.shape != 0) ⇒ keys is ptr-equal to registry.keys_of(cell.shape).
    Slab { keys: Rc<[Rc<str>]>, values: Vec<Value> },
    /// Dictionary fallback — today's representation, exact order preserved.
    /// Always cell.shape == 0 (the EMPTY_SHAPE sentinel ICs never cache, ic.rs/
    /// run.rs:4761 guard) — dictionary objects take the generic path, as today's
    /// shape-0 objects do.
    Dict(IndexMap<String, Value>),
}

pub struct ObjectCell {
    pub storage: RefCell<ObjectStorage>,
    pub shape: Cell<u32>,
    pub frozen: Cell<bool>,
}
```

The slab carrying its own (shared) `keys` Rc is the load-bearing decoupling: **every
registry-free consumer** — the stdlib (`src/stdlib/object.rs` keys/values/entries), json/
msgpack/cbor serialization, `Display` (`src/value.rs:1603`), the worker serializer
(`src/worker/serialize.rs`), structural equality of named enum payloads
(`src/value.rs:1447`), `validate_into` (`src/interp.rs:5430`), the GC trace — reads keys and
values through a mode-branching accessor API on `ObjectCell` without ever touching the
`Vm`/registry. Only the VM's opcode paths (which own the registry) create or grow slabs;
everything else reads both modes uniformly and, if it must add an unknown key without a
registry in hand, **demotes to dictionary mode** (order-preserving, one-way, always sound).

- **Read** (`obj.k`): IC hit → `values[index]` (shape check already proves the key). IC
  miss / generic mode → bounded linear scan of `keys` (slabs are capped at `SLAB_MAX_KEYS`
  keys, §3.1, so the scan is short, branch-predictable, and allocation-free — and ICs make
  hot sites O(1) anyway).
- **Write to an existing key**: `values[index]` store (IC fast path), or scan + store.
- **Add a new key** (VM path: `SET_PROP` miss that inserted, `SET_INDEX` object arm,
  `APPEND_OBJECT`, `SPREAD_OBJECT` per-entry): ONE `registry.add_key(shape, k)` transition
  (memoized) + `values.push(v)` + swap `keys` for the child shape's canonical Rc.
  **`resync_object_shape`/`resync_instance_shape` are deleted** — the full-rewalk +
  `Vec<String>` clone has no reason to exist once the mutation site knows exactly which key
  was added (append-only: `IndexMap::insert` of a new key appends at the end, which is
  precisely a single transition edge).
- **Remove a key / exceed the cap**: demote to dictionary (§3).

### 2.3 The insertion-order proof — every object operation, enumerated

The language guarantees insertion order (iteration, json serialization). It falls out of the
slab because the shape's key order IS the insertion order. Operation by operation (each is a
plan test + an order-stress corpus case, §8.2):

| # | Operation | Today (IndexMap) | Slab behavior | Order identical because |
|---|---|---|---|---|
| 1 | Literal `{a: 1, b: 2}` (`Op::NewObject n`, `run.rs:2296`; tree-walker `interp.rs:3632`) | insert in source order; a later **duplicate** key overwrites the value but keeps the first-seen position (`run.rs:2301-2303`) | site-cached shape whose key list is the first-seen source order; values popped into slots; a duplicate pair writes the earlier slot (per-site `slot_of_pair` map, §4) | the cached key order is computed by the same first-seen/later-wins fold, once, at first execution |
| 2 | Spread-containing literal (`NewObject 0` + `AppendObject`/`SpreadObject`, `compile/mod.rs:5554-5573`) | per entry: insert = later-value-wins, first-position-kept | existing key → overwrite slot (position kept); new key → transition + push at end | `IndexMap::insert` of a new key appends; of an existing key keeps position — bit-for-bit the slab rule |
| 3 | `o.k = v` new key (`vm_set_prop` → `set_member`, `run.rs:5421`; SET_INDEX `run.rs:2542`) | append at end | transition + push at end | same append |
| 4 | `object.delete(o, k)` (`stdlib/object.rs:254`) | `shift_remove` — remaining order preserved | demote slab→dict materializing `(k, v)` pairs in slab order, then `shift_remove` | the materialized dict's order is the slab's order; `shift_remove` then behaves identically |
| 5 | Destructuring rest `let {a, ...rest} = o` (`Op::ObjectKey` + the rest collector) | collects leftover keys in `o`'s iteration order | accessor iteration in slab key order | iteration = key-list order = insertion order |
| 6 | `object.keys/values/entries` (`stdlib/object.rs:230-242`) | `IndexMap` iteration | accessor iteration | same |
| 7 | `json.stringify` / msgpack / cbor / `to_json_lossy` (`stdlib/json.rs:125`) | iterate in order | accessor iteration | same |
| 8 | Worker airlock structured clone (`worker/serialize.rs`) | iterate in order; rebuild as `IndexMap` on the far side | accessor iteration; far side builds a dict-mode object (fresh isolate may have no warm shapes — dict is always correct) | wire order = slab order; rebuild inserts in wire order |
| 9 | `validate_into` / `.from` / typed-parse (`interp.rs:5430`), incl. the Object→Map boundary coercion inside `coerce_field` | reads fields **by name**, iterates the supplied object for `strict`/Map coercion | by-name accessor + ordered iteration | reads are name-keyed (order-irrelevant); iterations use the same ordered accessor |
| 10 | `object.merge`/`pick`/`omit`/`fromEntries` (`stdlib/object.rs:257-289`) | build a NEW `IndexMap` | unchanged — they construct fresh dict-mode objects (`ObjectCell::new`) | construction order of the new map is the loop order, as today |
| 11 | `Display`/`print`/template interpolation (`value.rs:1603`) | iterate in order | accessor iteration | same |
| 12 | Equality: `Value::Object == Value::Object` is **identity** (`cc_ptr_eq`, `value.rs:1417`) — but a NAMED enum-payload compares its `ObjectCell` contents key-wise (`value.rs:1447`, `IndexMap::eq`, order-insensitive) | `IndexMap::eq` | accessor `content_eq`: same length ∧ every key's value equal (order-insensitive), mode-agnostic | replicates `IndexMap::eq` exactly, across mode pairs |
| 13 | Instance construction (`vm_construct`, `run.rs:5498`; tree-walker `construct`) | fields populated base-class-first per `merged_field_schema` — exactly the class base shape's order (`class_base_shape`, `run.rs:5100`) | slab pre-sized to the base shape; defaults/args fill slots; an undeclared field added in `init` is a transition | the base shape was already DEFINED as the merged-schema order |
| 14 | `Class.from`/typed-parse instances (`interp.rs:5430`) | built with `shape_id` 0 (`interp.rs:5376/5520`) — never IC'd today | unchanged: dict-mode instance, shape 0 | identical to today's behavior |

There is no other object-key mutation surface: a repo-wide audit (plan Task 1.1) found
exactly one in-place stdlib mutation of an existing object (`object.delete`,
`stdlib/object.rs:254`); every other stdlib object fn constructs a fresh cell.

### 2.4 Failure modes analyzed

- **Slab/shape desync** (the §1.1 bug class): structurally removed — the key list lives IN
  the storage next to the values, and the mode flag is co-located. A debug assertion
  (`debug_assert_eq!(keys.len(), values.len())`) guards the remaining invariant at every
  slab mutation. Removal cannot desync because removal IS the demotion.
- **Chunk/Vm affinity**: the literal-site cache (§4) stores per-`Vm` shape ids in a
  per-`Chunk` side table — sound for the same reason `GlobalCache::IndexBound` (a
  `user_globals` index in a chunk side table, `run.rs:1368-1392`) is sound: a `Chunk` is
  compiled by and lives with one `Vm` (modules are compiled per-Vm in `load_file_module`;
  worker isolates deserialize their own chunks). The plan adds a debug-build assertion that
  a site-cache hit's shape exists in the registry.
- **Aliasing during spread** (`{...o}` where the builder aliases the source): the existing
  snapshot-first discipline (`run.rs:2485-2491`) is kept — the accessor's snapshot iteration
  replaces the manual `Vec<(String, Value)>` clone.
- **Borrow discipline**: the accessor API keeps the `RefCell` granularity of today (one cell
  borrow per operation, never across `.await` — clippy `await_holding_refcell_ref` stays
  deny). Closure-based `for_each` iteration holds the borrow exactly as today's
  `o.borrow().iter()` loops do; every site that previously snapshotted before an await
  still snapshots (`entries()` → `Vec`).

## 3. Mechanism 2 — dictionary fallback mode

Some objects defeat shapes; the fallback keeps them exactly as fast and exactly as correct
as today.

**What demotes (slab → dict, one-way, order-preserving):**
1. **Key removal** — `object.delete` (`stdlib/object.rs:254`), the language's only removal
   surface (verified §2.3; `Map`/`Set` deletes are unrelated — they have no shapes).
2. **Key-count cap** — adding a key beyond `SLAB_MAX_KEYS` (default **64**, a tunable
   benchmarked in the plan's perf task; V8's fast-properties bound is the precedent).
3. **Transition fan-out cap** — a parent shape with more than `SHAPE_FANOUT_MAX` (default
   **128**) distinct outgoing keys stops minting children (`add_key` → `None`); the
   would-be transitioner demotes. This bounds pathological megamorphic key churn AND bounds
   the registry's attacker-reachable growth (§6.2).
4. **Registry-free new-key insert** — an `ObjectCell::insert` of an unknown key from a
   context without the registry (stdlib mutators other than delete do not exist today, but
   the accessor is total: any future one is sound by construction).

**What never happens:** dict → slab re-promotion. A demoted object would need its (possibly
megamorphic) key set re-interned and every warmed IC re-pointed; re-promotion buys nothing
the IC architecture wants (a dict object simply takes the generic path) and reintroduces the
aliasing hazards one-way-ness eliminates. Rejected (§10).

**How a dict object behaves:** `cell.shape == 0` permanently — and shape 0 is **already**
the "never cache" sentinel today (`run.rs:4761`, `run.rs:4794`, `vm_set_prop`'s
`shape != 0` gate at `run.rs:5440`): ICs neither serve nor record it; every read/write takes
the generic path; iteration/serialization order is the `IndexMap`'s. This is byte-identical
to how every tree-walker object and every `json.parse`-built object behaves today — the
dict mode is not a new behavior, it is today's behavior kept.

**Which objects are born dict:** everything built by `ObjectCell::new(IndexMap)` — the
tree-walker (all objects, `interp.rs:3632`), json/yaml/toml/csv decode, the worker airlock
receive side, `validate_into` instances, stdlib constructors (`merge`/`pick`/...). Only VM
opcode construction paths and `vm_construct` build slabs. This keeps `ObjectCell::new`'s
167 call sites compiling unchanged and keeps attacker-shaped data (decoded wire formats)
out of the registry by default (§6.2).

**Failure modes analyzed:** a wrong transition predicate is a **performance** cliff, never a
correctness one (dict is always correct) — the predicate constants are tunables measured in
the plan's bench task. The demotion itself materializes `(String, Value)` pairs in slab
order into an `IndexMap` — O(n) once per object lifetime, amortized against the removal/
overflow that triggered it. The differential-fuzzer coverage assertion (§8.4) proves both
modes actually run on the corpus, so neither path can rot silently.

## 4. Mechanism 3 — compile-time literal shapes, no `.aso` change

An object literal's keys are fully known at compile time (`compile_object`,
`src/compile/mod.rs:5525` — each key is a string constant pushed before its value;
`Op::NewObject n` then consumes the pairs). Today the VM re-derives the layout from those
constants on **every execution** (§1). SHAPE warms a **per-site cache** instead — the
`GlobalCache`/`arith_cache` side-table precedent (`src/vm/chunk.rs:421-435`,
`OffsetMap` keyed by op offset through the pass-through `OffsetHasher`, `chunk.rs:76`),
gated on `self.specialize`, with **no opcode or `.aso` change** (the side table is runtime
state, exactly like the four existing ones; `.aso` round-trip is unaffected):

```rust
/// Chunk side table: Op::NewObject offset → the literal's interned layout.
pub struct LitShapeCache {
    pub shape: u32,
    /// The registry's canonical key list for `shape` (Rc-shared with every object
    /// the site constructs — the per-construction key-list allocation disappears).
    pub keys: Rc<[Rc<str>]>,
    /// Pair-index → slot-index, present ONLY for the rare duplicate-key literal
    /// (`{a: 1, a: 2}`): n pairs fold into fewer slots, later value wins, first
    /// position kept. `None` = identity mapping (the common case).
    pub slot_of_pair: Option<Rc<[u16]>>,
}
```

- **First execution (slow path, specialize on):** pop pairs as today, fold duplicates
  first-seen/later-wins, intern the shape (`shape_for`), record
  `{shape, keys, slot_of_pair}` at the op offset. If interning fails (caps, §3) the site
  records a **negative entry** and constructs dict-mode forever (a >64-key literal is
  legal and simply stays dictionary — today's behavior).
- **Warm executions:** pop `n` values into a pre-sized `Vec<Value>` (popping the key
  constants is an `Rc` drop each — the bytecode still pushes them; the stack discipline and
  the verifier are untouched), apply `slot_of_pair` if present, set the cached shape, done.
  **Zero hashing, zero `IndexMap`, one allocation** (the values `Vec`).
- **`--no-specialize` (generic mode):** the site cache is never consulted or recorded —
  but construction still produces a **slab** via the per-execution registry walk (hashing
  confined to the registry's Fx probes). The representation is NOT toggleable (§8.3).
- **The empty literal** (`NewObject 0`, also the spread-builder seed, `compile/mod.rs:5554`)
  starts as `Slab { keys: keys_of(EMPTY_SHAPE), values: vec![] }` — `AppendObject`/
  `SpreadObject` then grow it by single transitions (§2.2), replacing today's
  insert-then-full-resync at `run.rs:2462/2502`.

**Why the cache is sound:** the keys at a given `NewObject` offset are constants in an
immutable `Chunk` (`Chunk.code` is byte-identical for the program's lifetime — the DBG
`Op::Break` patching never touches operands of other ops, and a patched site re-dispatches
the ORIGINAL opcode); same offset ⇒ same keys ⇒ same layout. Chunk/Vm affinity is the
`GlobalCache::IndexBound` precedent (§2.4). **Failure modes:** duplicate keys (handled by
`slot_of_pair`, tested); cap overflow at first execution (negative entry, dict path);
`u16` pair count already bounded by the compiler (`compile/mod.rs:5545-5547`).
`resync_object_shape`'s per-key `String` clone is gone with the function itself (§2.2);
the registry's per-probe `Box::from` allocation is gone via the two-level borrowed-probe
map (§2.1).

## 5. Mechanism 4 — instances

`Instance.fields: IndexMap<String, Value>` (`src/value.rs:411`) becomes the same
`ObjectStorage`. The class's **base shape** already exists (`class_base_shape`,
`run.rs:5100` — the merged base-first declared-field order, cached per class `Rc`
identity); it now also yields the canonical keys Rc, so `vm_construct` (`run.rs:5498`)
allocates `Slab { keys: base_keys, values }` pre-sized to the schema, fills defaults and
`init` assignments by slot, and never hashes a declared field name again. Field reads/
writes ride the same IC fast paths (`ic_get_field`'s Instance arm, `run.rs:4791-4818`;
`vm_set_prop`'s Instance arm **always** routes through `set_member` so the declared
field-type CONTRACT applies byte-identically — the slab never bypasses it,
`run.rs:5415-5418`). Dynamically added fields (`init` writing an undeclared name; a later
`SET_PROP` on a new name — both legal today) are single transitions; exceeding the caps
demotes the instance's fields to dict (shape 0, generic path — same rules as objects).
Tree-walker instances and `validate_into`-built instances stay dict at shape 0, exactly as
they are today (`interp.rs:5376/5520`). Method dispatch is untouched (the `MethodCache`
keys on class identity, not field shape, `src/vm/ic.rs:147`).

## 6. Mechanism 5 — interior hashing

**The hasher decision: FxHash (`rustc-hash`), not ahash.** `rustc-hash` is rustc's own
hasher — the fastest option for the short string/integer keys these tables carry, no
DoS resistance, no RNG/seed machinery. It is **already in the production dependency graph**
via `cstree` (verified: `cargo tree -i rustc-hash` → `rustc-hash v2.1.2 └── cstree └──
ascript`), so promoting it to a direct dependency adds **zero new crates** — consistent
with the pure-Rust, no-new-deps posture (`Cargo.toml`'s posture comments; the JIT spec's
"ordinary crates, no native toolchain" stance). ahash would add a new crate + its seeding
story for tables that need neither.

### 6.1 Tables that move to FxHash — each justified by what keys flow in

| Table | Keys | Why safe |
|---|---|---|
| `ShapeRegistry.transitions` (`shape.rs:31`, restructured §2.1) | object-literal keys + class field names (compile-time constants) + dynamically-added property names | §6.2 — the only table with any attacker-reachable inflow; bounded by construction (caps), see below |
| `Vm.class_methods` / `class_static_methods` / `class_defaults` / `class_base_shapes` (`run.rs:74-92`) | `Rc::as_ptr` addresses (`usize`) + method-name strings from compiled source | pointer keys and source identifiers — never data-derived; tables are get/insert-only (audited: never iterated into output, so iteration order cannot leak) |
| `Vm.user_globals` (`run.rs:165`, `IndexMap<Rc<str>, GlobalSlot>`) | top-level binding names — source identifiers, compile-time | program text, not data; **iteration order is insertion order regardless of hasher** (`IndexMap` order is hasher-independent), so the `run.rs:714` iteration is provably unaffected |
| Compile-time maps in `src/compile/` + `src/syntax/resolve/` scope tables (where profiled hot) | source identifiers | compile-time only; never sees runtime data |

Already done, kept as the precedent: the four chunk side tables use the pass-through
`OffsetHasher` (`chunk.rs:76-99`); the new `lit_shapes` table joins them.

**Considered and NOT changed** (recorded so it isn't re-litigated): `file_modules`
(`HashMap<PathBuf, …>`, cold — one probe per import), the tree-walker `Environment` scope
chain (the oracle's performance is not gated), `Interp.resources` (cold, id-keyed).

### 6.2 The security decision — what keeps SipHash, and the registry threat model

**User-facing `Map`/`Set` (`MapKey`-keyed `IndexMap`/`IndexSet`, `value.rs:117/153`) and
dictionary-mode objects keep SipHash.** Threat model: their keys come directly from program
DATA — `json.parse` of network input, HTTP headers/params, csv/yaml/msgpack decode — the
classic hash-flooding DoS surface (attacker crafts colliding keys → O(n²) inserts). SipHash
with random per-process seeding is the documented defense and it stays. This is a security
property of the language, stated here as a decision: **no measured construction win
justifies making `m[attacker_key]` quadratic.**

**The registry is NOT entirely attacker-free — handled honestly.** Compile-time keys
dominate, but a dynamically-added property name can be data-derived
(`o[user_string] = v` on a slab object is a transition keyed by `user_string`). Two
bounds make Fx acceptable there: (a) **born-dict default** — decoded wire data
(`json.parse` etc.) is dictionary-mode from birth (§3) and never transitions the registry
at all; an attacker reaches the registry only through a program that takes a data string
and assigns it as a property name on a slab object; (b) **the caps** — one object
contributes at most `SLAB_MAX_KEYS` transitions before demoting, and one parent shape
accepts at most `SHAPE_FANOUT_MAX` distinct edges before refusing (`add_key → None` →
demotion) — so the registry's total attacker-driven growth AND the per-bucket collision
mass are bounded by constants, leaving no quadratic lever. The spec records this as the
designed trade; the plan's adversarial test drives a million hostile dynamic keys through
the caps and asserts bounded registry size and linear time.

## 7. Mechanism 6 — GC

`ObjectCell` is cycle-traced (`Cc`; `impl Trace for ObjectCell`, `src/gc.rs:252-266`).
**Invariant, stated explicitly: the slab's `Vec<Value>` is traced exactly as the
`IndexMap`'s values were — every contained `Value` is visited; nothing else is.**

```rust
impl Trace for ObjectCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Ok(storage) = self.storage.try_borrow() {   // keep the try_borrow
            match &*storage {                              // discipline (gc.rs:258)
                ObjectStorage::Slab { values, .. } => {
                    for v in values { v.trace(tracer); }   // keys: Rc<str>, acyclic — not traced
                }
                ObjectStorage::Dict(map) => trace_index_map(map, tracer), // unchanged (gc.rs:168)
            }
        }
    }
    fn is_type_tracked() -> bool { true }
}
```

The `keys: Rc<[Rc<str>]>` is acyclic immutable string data (the same class as `MapKey`'s
no-op trace rationale, `gc.rs:183-191`) — it holds no `Value`, no `Cc`, and is owned by the
registry on the `Vm` (a GC root), so it adds zero GC edges. `Instance`'s `Trace` gets the
identical two-arm treatment. The **native-handle no-trace rule is untouched** — no native
resource enters object storage in either mode (they are `Value::Native` ids), and the
`Cell<u32>`/`Cell<bool>` fields remain non-edges. The shipped GC unit tests (object cycles,
self-references, the V13 battery) must pass with slab-mode cycle members — the plan adds a
slab-object cycle reclamation test.

## 8. Correctness

### 8.1 The four-mode differential is the primary gate

The tree-walker keeps `IndexMap` storage (it constructs only dict-mode cells), so ANY
order/value/panic divergence the new storage introduces is caught immediately by the
standing identity over the whole corpus + goldens in BOTH feature configs
(`tests/vm_differential.rs`; `assert_three_way_matches`, `tests/vm_differential.rs:5590`):

> tree-walker == specialized-VM == generic-VM (and `.aso`-compiled)

Fix the engine, never the assertion. `.aso` adds nothing new here — the chunk is
byte-identical (§ front matter) and a deserialized chunk warms its own side tables, exactly
like `field_ics` today; a negative-space test asserts `ASO_FORMAT_VERSION` is **unchanged vs
the branch's merge-base** (compare the constant against the value recorded at branch time —
never a literal: SHAPE runs PARALLEL to DEFER, which legitimately bumps 27 → 28; the assertion
is "unchanged by THIS spec") and a
golden `.aso` byte-compare across the branch (the SRV `srv_negative_space` precedent).

### 8.2 The order-stress corpus

A dedicated example pair joins `examples/` (intro) + `examples/advanced/`
(production-shaped, error-handled), exercising — with printed output so the differential
bites on ORDER, not just values:

literal order · duplicate-key literals · add-order (`SET_PROP`/`SET_INDEX` new keys) ·
spread merge order incl. later-value-wins-first-position and self-spread · destructuring
rest collection order · `object.delete` then continued reads/adds (the §1.1 reproducer
inlined) · json round-trip order · a >`SLAB_MAX_KEYS` object (dictionary-transition order)
· hostile dynamic keys (cap demotion) · the same matrix on class instances (declared,
defaulted, `init`-added, post-construction-added fields) · `keys`/`values`/`entries` ·
worker-airlock round-trip order.

### 8.3 What `--no-specialize` disables — specified exactly

`--no-specialize` (`Vm::new_generic`, `run.rs:117/225`) disables **site caches**: the new
literal-shape cache joins the field/method ICs, adaptive arithmetic, and global caches as
skipped fast paths. **The storage representation itself is NOT toggleable** — generic mode
constructs and reads slabs through the per-execution registry walk and the bounded key
scan. Justification: the representation is not a *speculation* — it has **no guard that can
fail** (nothing is assumed that a different input could violate; the slab is definitionally
correct for its keys). A kill switch exists to prove guarded fast paths equal the generic
path; the representation's oracle is the **tree-walker** (which genuinely keeps the old
representation), and that is the differential mode that proves it. Making generic mode
construct `IndexMap`s would *reduce* coverage — the slab would then never run under the
kill-switch differential at all.

### 8.4 Fuzzing + coverage assertion + property tests

- **Differential fuzzer axis:** the grammar-aware generator (`src/fuzzgen/`,
  `fuzz/fuzz_targets/differential.rs`) already emits object literals/mutations; its
  generator gains spread/delete/rest weight so generated programs exercise both modes. The
  in-suite battery (`tests/property.rs::three_way_differential_*`) runs it on every
  `cargo test`.
- **Coverage assertion (the anti-false-green rule, campaign Gate 15):** per-`Vm` counters
  `slab_constructed`/`dict_constructed`/`demotions`, **compiled only under
  `feature = "fuzzgen"`** (which the crate's self-dev-dependency enables for every
  `cargo test`, both configs — and which production builds never enable, so the counters
  are *not there* on the hot path, the JIT-counter `cfg` discipline, not merely
  not-taken). A corpus-runner test asserts: slab-mode count > 0, dict-mode count > 0, AND
  demotion count > 0 after the differential corpus — proving all three paths genuinely ran.
- **Property tests (the FUZZ precedent, `tests/property.rs`):** proptest drives random op
  sequences — `Insert(k,v)` / `Overwrite(k,v)` / `Delete(k)` / `SpreadFrom(snapshot)` /
  `Iterate` / `JsonRoundTrip` — against a **model `IndexMap`** and a real
  `ObjectCell`+`ShapeRegistry` pair (slab-born), asserting entry-by-entry order + value
  equality after every step, across cap boundaries (sequences sized to straddle
  `SLAB_MAX_KEYS`). A saboteur self-test plants an order bug to prove the harness can fail.

## 9. Performance — measured, not promised

Same-session A/B (campaign Gate 16; the SRV MINOR-2 lesson) on `bench/profiling/`
`json_roundtrip.as` + `object_churn.as`, the LANE Task-0 functional corpus once merged, and
the `tests/vm_bench.rs` suite; results in `bench/SHAPE_RESULTS.md` with the shipped
profiler (`ascript run --profile cpu`) as the instrument. Allocation counts + peak RSS per
workload (Gate 18: `/usr/bin/time -l` + the harness), before/after.

**Honest expectations:**
- **`object_churn`** (literal construction + shaped reads in a tight loop; alloc 22%,
  hashing 13%) is the headline target: construction hashing → ~zero (warm site cache),
  per-object allocations drop (one `Vec` vs `IndexMap`'s table + the temp pairs `Vec` +
  per-key `String`s), reads unchanged-or-better (IC path loses the `IndexMap` indirection).
- **`json_roundtrip`** gains are **bounded by design**: `json.parse`-built objects are
  born dict (SipHash kept — the §6.2 security decision), so its 11% hashing share does NOT
  go to zero; the win there is literal/temporary-object allocation and the stringify
  iteration path. State this in the report rather than discovering it.
- **IC reads:** unchanged-or-better; the `Mono` hit becomes shape-check + `values[idx]`
  with no `IndexMap::get_index` bucket math.
- **Risk, measured not assumed:** slab `Vec` push patterns vs `IndexMap`'s amortization on
  the add-key-heavy paths (every add swaps a keys-`Rc` + may realloc the values `Vec`);
  the demotion cliff placement (`SLAB_MAX_KEYS`/`SHAPE_FANOUT_MAX` tuned against the
  corpus); the linear key scan on IC-cold generic reads (bounded by the cap — benchmarked
  under `--no-specialize`).
- **Gate-12 floor:** spec/tw bench geomean ≥2× holds at merge; the dispatch loop is touched
  (the `NewObject`/`AppendObject`/`SpreadObject`/prop arms), so the **`dbg_zero_cost_gate`
  re-run is mandatory** (instrument==None ≈ armed-idle, `tests/vm_bench.rs`). A memory
  regression is a bug to fix, never a tradeoff to accept (Gate 18).

## 10. Scope & rejected alternatives

**In scope:** the `ObjectStorage` slab/dict dual mode inside `ObjectCell` + `Instance.fields`;
the registry restructure (canonical keys ownership, borrowed-probe transitions, caps); the
accessor API migration of every interior consumer; deletion of
`resync_object_shape`/`resync_instance_shape`; the `lit_shapes` chunk side table; FxHash for
the §6.1 tables; the Phase-0 `object.delete` stale-shape bug fix on the current
representation; the order-stress corpus, fuzzer axis, coverage counters, property tests,
bench report, docs/CLAUDE/roadmap updates.

**Rejected:**
- **NaN-boxing / `Value` layout changes** — NANB's spec, sequenced after SHAPE precisely so
  object internals stabilize first (`goal-perf.md`).
- **Changing `Map`/`Set` (or dict-mode) hashing** — DoS resistance is a documented security
  property (§6.2). No.
- **Changing the tree-walker representation** — it is the permanent oracle; divergent
  representations with identical observable behavior is the POINT (front matter).
- **A new `Value` variant for slab objects** — `ObjectCell`'s interior changes; the `Value`
  surface, the worker wire, `.aso`, and every `match` on `Value` do not. A new variant would
  re-touch every exhaustive match for zero semantic gain.
- **`.aso` shape serialization / pre-seeded shapes** — shapes are per-`Vm` runtime state;
  PGO-style pre-seeding is WARM's manifest feedback section, not SHAPE.
- **Dict→slab re-promotion** — §3; complexity and IC-staleness hazards for no architectural
  benefit.
- **A per-shape key→index hash map** — the slab cap makes the bounded linear scan cheaper
  than a hash probe for the generic path, and ICs own the hot path; a per-shape map would
  re-introduce a hashed structure per layout.
- **ahash** — adds a new crate + seeding machinery; FxHash is already in the graph and
  faster for these key types (§6).
- **Demote-on-raw-`borrow()` migration shim** (keep `borrow_mut()` returning the `IndexMap`
  and silently demote slabs) — a silent performance footgun that hides un-migrated sites;
  the accessor migration is mechanical and the compiler finds every site (no `borrow()` on
  the storage = no stragglers).

## 11. Grounding (verified 2026-06-12; all line numbers checked against source)

- **Evidence:** `bench/PROFILING_RESULTS.md` (hashing 11–13%, allocation 22–38%,
  `object_churn` dispatch table); `goal-perf.md` §"Evidence base" + the SHAPE entry (note:
  its `run.rs:4526` resync citation is now `run.rs:5384` — code moved, unchanged).
- **Storage today:** `src/value.rs:24-32` (`ObjectCell{map, shape, frozen}`),
  `:409-421` (`Instance{fields, shape_id, frozen}`), `:1133` (`Value::Object(Cc<ObjectCell>)`),
  `:188-200` (`MapKey`), `:1417` (Object `==` is identity), `:1447` (named-payload key-wise
  eq), `:1603` (ordered `Display`); `ObjectCell::new` = 167 call sites, `Value::Object(` =
  379 matches in 48 files (the migration blast radius, measured).
- **Shapes/ICs today:** `src/vm/shape.rs:28-35` (registry), `:57-65` (`add_key`, the
  `Box::from(key)` probe alloc at `:58`); `src/vm/ic.rs:48` (`POLY_MAX`), `:54-69`
  (`InlineCache`, `Mono{shape,index}` at `:60`), `:147` (`MethodCache::Mono` keys on class
  identity).
- **VM paths:** `src/vm/run.rs:2296` (`Op::NewObject` — temp pairs `Vec` `:2305`, per-key
  insert `:2320-2323`, post-hoc shape walk `:2327`), `:2442` (`AppendObject` + resync
  `:2462`), `:2477` (`SpreadObject`, snapshot `:2485-2491`, resync `:2502`), `:2542`
  (`SetIndex` + resync `:2557-2559`), `:2563` (`GetProp`), `:4751-4823` (`ic_get_field`,
  shape-0 guards `:4761/:4794`, defensive fall-through `:4773-4775`), `:5100`
  (`class_base_shape`), `:5117` (`object_shape_for`), `:5384/:5397` (the resyncs),
  `:5421` (`vm_set_prop`, frozen check `:5433`, instance-contract routing `:5415-5418`),
  `:5498` (`vm_construct`), `:64-205` (`Vm` fields: `shapes:88`, `specialize:117`,
  `user_globals:165`, `struct_gen:181`), `:714` (`user_globals` iteration —
  insertion-ordered), `:1368-1392` (`GlobalCache::IndexBound`, the chunk-side-cache-of-
  Vm-state precedent).
- **Compiler:** `src/compile/mod.rs:5525-5578` (`compile_object`: const-key pushes,
  `NewObject n` `:5549`, the spread builder `:5554-5573`, u16 overflow guard `:5545-5547`).
- **Side-table precedent:** `src/vm/chunk.rs:63-99` (`OffsetHasher`/`OffsetMap`),
  `:421-435` (the four side tables), `:793-845` (accessors).
- **Tree-walker (oracle):** `src/interp.rs:3632` (`ExprKind::Object` — IndexMap, shape never
  touched), `:4361` (`read_member` Object arm), `:5430` (`validate_into` + `coerce_field`
  Object→Map boundary), `:5376/:5520` (instances built at shape 0).
- **Removal surface:** `src/stdlib/object.rs:250-256` (`object.delete` → `shift_remove`,
  no shape resync — the §1.1 live bug, reproduced 2026-06-12: specialized VM `2,3` vs
  tree-walker/generic `2,2`); repo-wide grep: the only in-place key mutation in the stdlib.
- **GC:** `src/gc.rs:252-266` (`Trace for ObjectCell`, `try_borrow` discipline `:258`),
  `:168-181` (`trace_index_map`/`trace_index_set`), `:183-191` (`MapKey` no-op rationale).
- **Hashers:** `Cargo.toml:41` (`indexmap = "2"`, std default = SipHash `RandomState`);
  `cargo tree -i rustc-hash` → already in the production graph via `cstree v0.14.0`;
  ahash NOT in the default production graph.
- **Format stability:** `src/vm/aso.rs:167` (`ASO_FORMAT_VERSION = 27` at drafting, untouched
  by SHAPE — asserted merge-base-relative, §8.1).
- **Test infrastructure:** `tests/vm_differential.rs:5590` (`assert_three_way_matches`),
  `:977` (`EXAMPLE_SKIPS`); `tests/property.rs` (proptest differential + saboteur
  self-test precedent); `fuzz/fuzz_targets/differential.rs`; `tests/vm_bench.rs`
  (`dbg_zero_cost_gate`, the ≥2× geomean gate).
- **External precedent:** V8 hidden classes + dictionary mode (fast-properties ↔
  slow-properties one-way demotion on delete/overflow — the fallback model §3 adopts);
  SpiderMonkey shape trees (shape-owned layout, slot vectors); rustc's `FxHashMap`
  (compiler-interior hashing of identifier-class keys); SipHash's hash-flooding rationale
  (aumasson/bernstein) for the §6.2 keep-SipHash decision; PEP 659 (the warm-site side-table
  discipline `adapt.rs` already implements and §4 extends).
