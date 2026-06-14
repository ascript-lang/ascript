# SHAPE — Shape-Native Object Storage + Literal Shapes + Interior Hashing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task is executed by a **fresh implementer subagent**, then verified by an **independent reviewer subagent** that runs the commands and probes edges (code quality + spec adherence) before acceptance. At the end of each phase, a **holistic per-phase review subagent** reviews the phase's combined changes before the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Make the shape OWN the key→index layout: VM objects/instances store a flat values slab (dictionary-mode `IndexMap` fallback preserved for removal/overflow/churn), object literals construct through a per-site cached shape with zero hashing, `resync_object_shape`'s per-key clones disappear, and interior hash tables move to FxHash — with the tree-walker untouched as the representation oracle, zero observable behavior change (four-mode byte-identity over the corpus), and a same-session A/B + allocation/RSS report. Plus: fix the live `object.delete` stale-shape wrong-value bug found while drafting the spec (failing-test-first, before anything else).

**Architecture:** `ObjectCell`'s interior becomes `RefCell<ObjectStorage>` — `Slab { keys: Rc<[Rc<str>]>, values: Vec<Value> }` (keys = the registry's canonical per-shape list, `Rc`-shared) or `Dict(IndexMap<String, Value>)` (today's representation; always shape 0, never IC-cached). A mode-branching accessor API on `ObjectCell` replaces every raw `borrow()/borrow_mut()` so all registry-free consumers (stdlib, json, worker serializer, Display, GC, equality) read both modes uniformly; only VM opcode paths (which own the per-`Vm` `ShapeRegistry`) create/grow slabs. A new offset-keyed chunk side table (`lit_shapes`, the `field_ics` precedent) caches each `Op::NewObject` site's interned shape. `Instance.fields` gets the identical treatment via the class base shape. No opcode/`.aso` change (`ASO_FORMAT_VERSION` unchanged vs the merge-base — 27 at drafting; DEFER bumps to 28 in a parallel branch, so the guard is merge-base-relative, never a literal). Spec: `superpowers/specs/2026-06-12-shape-storage-design.md` (read it first; every mechanism, failure mode, and rejected alternative is there).

**Tech stack:** Rust, single binary `ascript`; `src/value.rs` (storage), `src/vm/{shape,ic,run,chunk}.rs` (registry/ICs/dispatch/side tables), `src/gc.rs` (trace), `rustc-hash` (FxHash — already in the production graph via `cstree`, becomes a direct dep), `indexmap 2` (dict mode, SipHash kept), proptest (`tests/property.rs`), the four-mode differential (`tests/vm_differential.rs`), `tests/vm_bench.rs` (Gate 12 + `dbg_zero_cost_gate`), `bench/profiling/` + `/usr/bin/time -l` (Gate 18).

**Binding execution standards (production-grade mandate, `goal.md` Gates 1–14 + `goal-perf.md` Gates 15–18):** TDD per task (failing test → minimal code → green → commit, house trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); any bug found en route — ours or pre-existing — is fixed in-branch with a failing-test-first regression guard, never deferred; clippy clean + tests green in BOTH feature configs at every phase close; the tree-walker is never relaxed; no placeholder/TODO on a reachable path. Branch: `feat/shape-storage` off `main`.

---

## File structure

**New files:**
- `examples/object_order_stress.as` — the intro order-stress corpus example (spec §8.2).
- `examples/advanced/object_order_pipeline.as` — production-shaped order-stress (records + spread defaults + delete + json round-trip + instances + worker round-trip, fully error-handled).
- `bench/SHAPE_RESULTS.md` — the same-session A/B + allocation/RSS report (Gate 16/18).

**Modified files:**
- `src/value.rs` — `ObjectStorage`, the accessor API, `Instance.fields` migration, `content_eq`.
- `src/vm/shape.rs` — registry v2: canonical `keys_of`, two-level Fx transitions (borrowed probe), `SLAB_MAX_KEYS`/`SHAPE_FANOUT_MAX` caps, `Option` returns.
- `src/vm/chunk.rs` — `lit_shapes: RefCell<OffsetMap<LitShapeCache>>` + accessors.
- `src/vm/run.rs` — `NewObject`/`AppendObject`/`SpreadObject`/`SetIndex`/`GetProp`/`SetProp` arms, `ic_get_field`, `vm_set_prop`, `vm_construct`, slab-insert helper; **delete** `resync_object_shape`/`resync_instance_shape`; FxHash on the `Vm` class tables; fuzzgen-gated mode counters.
- `src/gc.rs` — two-arm `Trace for ObjectCell` (+ Instance), slab-cycle test.
- `src/interp.rs`, `src/stdlib/*.rs`, `src/worker/serialize.rs` — accessor migration (mechanical; no behavior change).
- `src/stdlib/object.rs` — Phase 0 `object.delete` fix.
- `Cargo.toml` — `rustc-hash` direct dependency (zero new crates).
- `tests/vm_differential.rs` — order-stress battery + delete-bug regression + coverage assertion.
- `tests/property.rs` — the model-IndexMap property suite + saboteur.
- `src/fuzzgen/` — generator weight for delete/spread/rest.
- `tests/aso.rs` or `tests/vm_limits.rs` — the negative-space guard (`ASO_FORMAT_VERSION` unchanged vs merge-base, golden byte-compare).
- `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`, `docs/content/language/values-types.md` — docs/status.

---

## Phase 0 — the live `object.delete` stale-shape bug (fix FIRST, on the current representation)

### Task 0.1: failing-test-first regression + the one-line fix

**The bug (spec §1.1, reproduced 2026-06-12):** `object.delete` (`src/stdlib/object.rs:254`) `shift_remove`s a key without touching `ObjectCell.shape`, so a warmed `GET_PROP` IC (`Mono { shape, index }`) serves a **shifted, in-range, wrong slot** — the `run.rs:4773` defensive guard only catches out-of-range. Specialized VM prints `3` where tree-walker/generic print `2`.

**Files:**
- Modify: `src/stdlib/object.rs` (the `"delete"` arm, ~250)
- Test: `tests/vm_differential.rs` (focused battery, uses `assert_three_way_matches`)

- [ ] **Step 1: Write the failing test** in `tests/vm_differential.rs` beside the V11-T5 battery:

```rust
/// SHAPE Phase 0 regression: `object.delete` must invalidate the hidden-class
/// shape. Before the fix the specialized VM served the WRONG SLOT from a warmed
/// IC (printed "2\n3\n" vs the tree-walker/generic "2\n2\n").
#[tokio::test]
async fn object_delete_invalidates_shape_ic() {
    assert_three_way_matches(
        r#"
        import * as object from "std/object"
        let o = {a: 1, b: 2, c: 3}
        fn get_b(x) { return x.b }
        print(get_b(o))            // warm the IC: Mono{shape(a,b,c), index 1}
        object.delete(o, "a")
        print(get_b(o))            // must re-resolve: 2, not 3
        o.d = 4                    // re-shaping after delete must also be sound
        print(get_b(o))
        print(o)                   // remaining order: {b: 2, c: 3, d: 4}
        "#,
    )
    .await;
}
```

- [ ] **Step 2: Run it — expect FAIL** (specialized != tree-walker):
  `cargo test --test vm_differential object_delete_invalidates_shape_ic` → assertion failure showing `"2\n3\n…"` vs `"2\n2\n…"`.
- [ ] **Step 3: Apply the fix** — reset the shape to the never-cached sentinel:

```rust
"delete" => {
    let o = want_object(&arg(args, 0), span, &ctx("delete"))?;
    let key = want_string(&arg(args, 1), span, &ctx("delete"))?;
    // shift_remove preserves the order of the remaining keys.
    let existed = o.borrow_mut().shift_remove(key.as_ref()).is_some();
    if existed {
        // Removal changes the key LAYOUT. Reset the hidden-class shape to the
        // EMPTY/unset sentinel: shape 0 is never consulted or recorded by the
        // field ICs (run.rs:4761/5440), so every later access re-resolves
        // generically instead of serving a stale slot. A subsequent key ADD
        // re-derives a fresh, correct shape via resync_object_shape.
        o.shape.set(0);
    }
    Ok(Value::Bool(existed))
}
```

- [ ] **Step 4: Run it — expect PASS**; then the full `cargo test --test vm_differential` (BOTH configs) — expect all green (378+/0).
- [ ] **Step 5: Blast-radius audit (the bug-class hunt, Gate 14):** grep every in-place object/instance key mutation outside the VM's resync'd opcode paths — `grep -rn "borrow_mut().shift_remove\|borrow_mut().insert\|borrow_mut().clear\|borrow_mut().sort" src/stdlib/ src/interp.rs` — and confirm `object.delete` was the only layout-changing site on a possibly-shaped cell (drafting audit says yes: json/worker/`merge`/`fromEntries` all build FRESH cells at shape 0). Document the audit result in the commit body. Any second site found gets the same fix + test in this task.
- [ ] **Step 6: Independent review checkpoint** — reviewer reruns the reproducer on the pre-fix commit (must fail) and post-fix (must pass), probes `delete` on a poly-warmed site and on an instance-receiver (must be a Tier-2 `want_object` error, unchanged), and confirms the audit grep.
- [ ] **Step 7: Commit** — `git commit -m "fix(vm): object.delete resets the hidden-class shape (stale-IC wrong-value)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 0.2: Phase 0 holistic review

- [ ] **Step 1:** Holistic-review subagent: the fix is minimal and on the current representation (no Phase-2 concepts leaked in); the regression test genuinely trips pre-fix; both feature configs green (`cargo test` + `cargo test --no-default-features`), clippy clean both.
- [ ] **Step 2:** Tick only when green. This commit is cherry-pickable to `main` independently if the owner wants the bug fix shipped before SHAPE merges (note it in the PR description either way).

---

## Phase 1 — the accessor API migration (mechanical, behavior-identical)

> The blast radius is measured: 379 `Value::Object(` matches across 48 files, 167 `ObjectCell::new` sites. This phase introduces the mode-AGNOSTIC accessor API while the storage is still `IndexMap`-only, and migrates every consumer off raw `borrow()/borrow_mut()`. At phase close the `map` field is private — the compiler proves there are no stragglers. Zero behavior change: the differential is run after EVERY task.

### Task 1.1: the accessor API on `ObjectCell` (storage still `IndexMap`)

**Files:**
- Modify: `src/value.rs`
- Test: inline `#[test]`s in `src/value.rs`

- [ ] **Step 1: Write the failing tests** — accessor semantics mirror `IndexMap` exactly:

```rust
#[test]
fn object_accessors_mirror_indexmap_semantics() {
    let mut m = IndexMap::new();
    m.insert("a".to_string(), Value::Int(1));
    m.insert("b".to_string(), Value::Int(2));
    let o = ObjectCell::new(m);
    assert_eq!(o.len(), 2);
    assert_eq!(o.get("a"), Some(Value::Int(1)));
    assert_eq!(o.get_index_of("b"), Some(1));
    o.insert("a", Value::Int(9));                  // overwrite: position kept
    assert_eq!(o.get_index(0).map(|(k, _)| k.to_string()), Some("a".into()));
    o.insert("c", Value::Int(3));                  // new key: appended
    let keys: Vec<String> = { let mut v = vec![]; o.for_each(|k, _| v.push(k.to_string())); v };
    assert_eq!(keys, ["a", "b", "c"]);
    assert_eq!(o.shift_remove("b"), Some(Value::Int(2)));
    assert_eq!(o.get_index_of("c"), Some(1));      // order preserved after removal
}

#[test]
fn object_content_eq_is_order_insensitive_like_indexmap_eq() {
    // replicates IndexMap::eq for the named-enum-payload comparison (value.rs:1447)
    let a = obj(&[("x", 1), ("y", 2)]);
    let b = obj(&[("y", 2), ("x", 1)]);
    assert!(a.content_eq(&b));
    assert!(!a.content_eq(&obj(&[("x", 1)])));
}
```

- [ ] **Step 2: Run — expect FAIL** (methods don't exist): `cargo test -p ascript object_accessors`
- [ ] **Step 3: Implement** on `ObjectCell` (backed by `self.map` for now; signatures chosen so Phase 2 only changes BODIES):

```rust
impl ObjectCell {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, key: &str) -> Option<Value>;            // cloned (Value clone = Rc bump)
    pub fn contains_key(&self, key: &str) -> bool;
    pub fn get_index_of(&self, key: &str) -> Option<usize>;
    pub fn value_at(&self, i: usize) -> Option<Value>;        // the IC read primitive
    pub fn set_value_at(&self, i: usize, v: Value) -> bool;   // the IC write primitive (existing slot only)
    pub fn insert(&self, key: &str, v: Value);                // later-wins / first-position-kept
    pub fn shift_remove(&self, key: &str) -> Option<Value>;   // order-preserving removal
    pub fn entries(&self) -> Vec<(Rc<str>, Value)>;           // snapshot (for await-crossing / aliasing sites)
    pub fn for_each<F: FnMut(&str, &Value)>(&self, f: F);     // zero-alloc in-order iteration
    pub fn try_for_each<E, F: FnMut(&str, &Value) -> Result<(), E>>(&self, f: F) -> Result<(), E>;
    pub fn content_eq(&self, other: &ObjectCell) -> bool;     // == IndexMap::eq semantics
}
```

Doc-comment each with its order guarantee (the spec §2.3 table row it implements). Borrow discipline: each accessor takes ONE `RefCell` borrow internally; none returns a guard — so no caller can hold a borrow across `.await` through this API (clippy `await_holding_refcell_ref` stays structurally satisfied).

- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Commit** — `git commit -m "feat(value): mode-agnostic ObjectCell accessor API (IndexMap-backed)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.2: migrate the stdlib + serializers + Display + equality

**Files:** `src/stdlib/{object,json,msgpack,cbor,schema,…}.rs` (every `o.borrow()` on an ObjectCell), `src/value.rs` (Display `:1603`, the named-payload eq arm `:1447` → `content_eq`), `src/worker/serialize.rs`.

- [ ] **Step 1:** Mechanical migration, file by file: `o.borrow().get(k).cloned()` → `o.get(k)`; read-only `for (k, v) in o.borrow().iter()` → `o.for_each(|k, v| …)` (or `try_for_each` where the body errors); sites that snapshot (await-crossing, self-aliasing) → `o.entries()`; `o.borrow_mut().insert(k.to_string(), v)` → `o.insert(&k, v)`; `schema_kind`/`is_schema_value` → `o.get("__kind")`. The named-payload `*oa.borrow() == *ob.borrow()` (`value.rs:1447`) → `oa.content_eq(ob)`. The Phase-0 `object.delete` arm → `o.shift_remove(...)` + keep the `shape.set(0)` (it moves INTO `shift_remove`'s slab arm in Phase 2; leave a `// Phase 2 absorbs this` marker comment).
- [ ] **Step 2:** `cargo test` (default config) green; `cargo test --test vm_differential` green — the corpus is the proof of behavior identity.
- [ ] **Step 3: Commit** — `git commit -m "refactor(stdlib): route object access through the ObjectCell accessors" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.3: migrate `interp.rs` + `src/vm/run.rs` + seal the field

**Files:** `src/interp.rs` (`read_member:4361`, `set_member`, `validate_into:5430`, destructuring, spread eval `:3632`), `src/vm/run.rs` (every `cell.map.borrow()` site incl. `ic_get_field:4751`, `vm_set_prop:5421`, `resync_object_shape:5384`, the opcode arms), then make `ObjectCell.map` **private**.

- [ ] **Step 1:** Migrate; `ic_get_field`'s hit path becomes `cell.value_at(idx as usize)`; `vm_set_prop`'s hit path `cell.set_value_at(idx, v)`; `resync_object_shape` reads keys via a temporary `keys_snapshot()` helper (deleted in Phase 3). `Instance.fields` stays a pub `IndexMap` until Task 3.4 (instances migrate with their slab task — one representation change at a time).
- [ ] **Step 2:** Change `pub map` → `map` (private). **The compiler now finds every straggler** — fix all. `ObjectCell::new(IndexMap)` stays the public constructor.
- [ ] **Step 3:** Full `cargo test` AND `cargo test --no-default-features` green; `cargo clippy --all-targets` + `--no-default-features --all-targets` clean.
- [ ] **Step 4: Commit** — `git commit -m "refactor(core): seal ObjectCell behind the accessor API (map private)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.4: Phase 1 holistic review

- [ ] **Step 1:** Holistic subagent over the combined Phase-1 diff: zero behavior change (four-mode differential both configs — the entire corpus), no accessor returns a borrow guard, no new clone in a hot loop that wasn't cloning before (spot-check `json.stringify` and `ic_get_field` disassembly-level: `value_at` inlines to the same `get_index` + clone), allocation profile unchanged on `bench/profiling/object_churn.as` (quick same-session sanity: `/usr/bin/time -l` before/after within noise).
- [ ] **Step 2:** Findings → tracked tasks, fixed before phase close. Tick only when both configs are fully green.

---

## Phase 2 — the storage representation: registry v2 + `ObjectStorage` + GC

### Task 2.1: `ShapeRegistry` v2 — canonical keys, borrowed probes, caps

**Files:**
- Modify: `src/vm/shape.rs`, `Cargo.toml` (add `rustc-hash = "2"` — already in the graph via `cstree`, zero new crates; verify with `cargo tree -i rustc-hash` in the commit body)
- Test: extend the inline `#[test]`s

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn keys_of_returns_canonical_shared_list() {
    let mut reg = ShapeRegistry::new();
    let ab = reg.shape_for(["a", "b"]).unwrap();
    let k1 = reg.keys_of(ab);
    let k2 = reg.keys_of(ab);
    assert!(Rc::ptr_eq(&k1, &k2), "one allocation per LAYOUT, shared");
    assert_eq!(&*k1[0], "a"); assert_eq!(&*k1[1], "b");
    assert_eq!(reg.keys_of(EMPTY_SHAPE).len(), 0);
}

#[test]
fn caps_refuse_instead_of_minting() {
    let mut reg = ShapeRegistry::new();
    // fan-out cap: SHAPE_FANOUT_MAX distinct children of one parent, then None.
    for i in 0..SHAPE_FANOUT_MAX { assert!(reg.add_key(EMPTY_SHAPE, &format!("k{i}")).is_some()); }
    assert_eq!(reg.add_key(EMPTY_SHAPE, "one_too_many"), None);
    // an ALREADY-MINTED edge still resolves after the cap (memoized, not refused):
    assert!(reg.add_key(EMPTY_SHAPE, "k0").is_some());
    // key-count cap: a chain longer than SLAB_MAX_KEYS refuses.
    assert!(reg.shape_for((0..=SLAB_MAX_KEYS).map(|i| /* leaked label */ key_label(i))).is_none());
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** (spec §2.1/§3): `transitions: FxHashMap<u32, FxHashMap<Box<str>, u32>>` (probe with `&str` via `Borrow<str>` — **kills the `Box::from(key)` per-probe allocation at `shape.rs:58`**); `keys: Vec<Rc<[Rc<str>]>>` dense by id (`keys_of` = index, no hash); `pub const SLAB_MAX_KEYS: usize = 64; pub const SHAPE_FANOUT_MAX: usize = 128;` (spec-mandated tunables, re-measured in Task 6.1); `add_key(shape, key) -> Option<u32>` (None on either cap — memoized edges keep resolving), `shape_for(keys) -> Option<u32>`. Keep every existing test passing (adapt signatures: `.unwrap()` where unbounded).
- [ ] **Step 4: Run — expect PASS** (`cargo test -p ascript shape::`).
- [ ] **Step 5: Commit** — `git commit -m "feat(vm): ShapeRegistry v2 — canonical key lists, Fx borrowed probes, demotion caps" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.2: `ObjectStorage` — slab + dict behind the accessors

**Files:**
- Modify: `src/value.rs`
- Test: inline `#[test]`s + `tests/property.rs`

- [ ] **Step 1: Write the failing tests** — the Task 1.1 accessor battery re-run against a SLAB-born cell, plus the property suite (spec §8.4) in `tests/property.rs`:

```rust
proptest! {
    /// SHAPE: random op sequences against a model IndexMap (the FUZZ precedent).
    /// Drives insert/overwrite/remove/spread/iterate across the SLAB_MAX_KEYS
    /// boundary; after EVERY op the cell's entries() must equal the model's
    /// (key order AND values), in whichever mode the cell is in.
    #[test]
    fn object_storage_matches_indexmap_model(ops in prop::collection::vec(op_strategy(), 1..200)) {
        let mut reg = ShapeRegistry::new();
        let cell = ObjectCell::new_slab(reg.keys_of(EMPTY_SHAPE), vec![], EMPTY_SHAPE);
        let mut model: IndexMap<String, Value> = IndexMap::new();
        for op in ops {
            apply_to_cell(&cell, &mut reg, &op);   // VM-style: transition via reg, demote on None/remove
            apply_to_model(&mut model, &op);
            let got: Vec<(String, Value)> = cell.entries().into_iter()
                .map(|(k, v)| (k.to_string(), v)).collect();
            let want: Vec<(String, Value)> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            prop_assert_eq!(got, want);
        }
    }
}

#[test]
fn saboteur_property_harness_can_fail() {
    // plant an order bug (swap two slots after a demotion) and assert the
    // harness DETECTS it — the anti-false-green self-test (FUZZ precedent).
}
```

- [ ] **Step 2: Run — expect FAIL** (`ObjectStorage`/`new_slab` don't exist).
- [ ] **Step 3: Implement** (spec §2.2/§3):

```rust
pub enum ObjectStorage {
    Slab { keys: Rc<[Rc<str>]>, values: Vec<Value> },
    Dict(IndexMap<String, Value>),
}
pub struct ObjectCell {
    storage: RefCell<ObjectStorage>,
    pub shape: Cell<u32>,
    pub frozen: Cell<bool>,
}
impl ObjectCell {
    pub fn new(map: IndexMap<String, Value>) -> Cc<ObjectCell> { /* Dict, shape 0 — UNCHANGED for all 167 sites */ }
    pub fn new_slab(keys: Rc<[Rc<str>]>, values: Vec<Value>, shape: u32) -> Cc<ObjectCell> {
        debug_assert_eq!(keys.len(), values.len());
        /* Slab, shape set */
    }
    /// One-way slab→dict demotion, order-preserving; shape → 0 (the never-cached
    /// sentinel) so every IC keyed on the old shape misses forever (spec §3).
    pub fn demote_to_dict(&self) {
        let mut st = self.storage.borrow_mut();
        if let ObjectStorage::Slab { keys, values } = &mut *st {
            let mut map = IndexMap::with_capacity(keys.len());
            for (k, v) in keys.iter().zip(std::mem::take(values)) {
                map.insert(k.to_string(), v);
            }
            *st = ObjectStorage::Dict(map);
        }
        drop(st);
        self.shape.set(crate::vm::shape::EMPTY_SHAPE);
    }
}
```

Accessor bodies branch on mode: slab `get`/`get_index_of` = bounded linear key scan (≤ `SLAB_MAX_KEYS`, length-check before bytes); `insert` of an UNKNOWN key on a slab (registry-free caller) → `demote_to_dict()` then dict insert (total, always sound); `shift_remove` on a slab → demote then remove (this ABSORBS the Phase-0 `shape.set(0)` — delete the marker comment, keep the Phase-0 regression test green); `value_at`/`set_value_at` = direct slab indexing. The VM-side slab GROWTH primitive (registry in hand) is added here too:

```rust
/// VM-only append: the caller already minted `child` = add_key(self.shape, key)
/// and fetched its canonical keys. Keeps the invariant keys==registry.keys_of(shape).
pub fn slab_append(&self, child_shape: u32, child_keys: Rc<[Rc<str>]>, v: Value) -> bool;
```

- [ ] **Step 4: Run — expect PASS** (accessor battery on both modes + property suite + saboteur).
- [ ] **Step 5: Commit** — `git commit -m "feat(value): ObjectStorage slab/dict dual mode behind the accessors" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.3: GC — two-arm trace + slab cycle reclamation

**Files:**
- Modify: `src/gc.rs` (`Trace for ObjectCell`, `:252`)
- Test: inline `#[test]` beside the V13 battery

- [ ] **Step 1: Write the failing test** — a self-referencing CYCLE through a slab-mode object (`o.values[0]` is an array containing `o`) is reclaimed by `gcmodule::collect_thread_cycles()` (mirror the existing object-cycle test, slab-born via `new_slab`).
- [ ] **Step 2: Run — expect FAIL** (trace doesn't reach slab values → cycle leaks / count wrong).
- [ ] **Step 3: Implement** the spec §7 impl verbatim: `try_borrow` kept; `Slab` arm traces every `Value` in `values` (keys are acyclic `Rc<str>` — NOT traced, the `MapKey` rationale `gc.rs:183`); `Dict` arm = `trace_index_map` unchanged. **Invariant comment:** the slab is traced exactly as the IndexMap values were; the native-handle no-trace rule and the `Cell` non-edges are untouched.
- [ ] **Step 4: Run — expect PASS** + the whole existing GC battery.
- [ ] **Step 5: Commit** — `git commit -m "feat(gc): trace ObjectStorage slab values (cycle-safe, keys acyclic)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.4: Phase 2 holistic review

- [ ] **Step 1:** Holistic subagent: invariants hold (`keys.len()==values.len()` debug-asserted at every slab mutation; `shape!=0 ⇒` keys ptr-eq canonical — add a `debug_assert` helper used by `slab_append`/`new_slab`); demotion is the ONLY slab exit and is one-way; no public way to build a desynced cell; property suite genuinely crosses the cap boundary (assert demotions occurred in at least one proptest case via a counter probe); both configs green, clippy clean both.
- [ ] **Step 2:** Findings fixed before phase close.

---

## Phase 3 — VM wiring: construction, mutation, ICs, instances

### Task 3.1: slab construction on the generic path + precise transitions (no site cache yet)

**Files:**
- Modify: `src/vm/run.rs` (`Op::NewObject:2296`, `AppendObject:2442`, `SpreadObject:2477`, `SetIndex:2542`, `vm_set_prop:5421`; DELETE `resync_object_shape:5384`)
- Test: `tests/vm_differential.rs` (the existing corpus IS the test; plus a focused battery)

- [x] **Step 1: Write the failing test** — a focused `assert_three_way_matches` battery: literal order, duplicate-key literal (`{a: 1, a: 2}` prints `{a: 2}`), spread merge later-wins-first-position, self-spread, `SET_INDEX` add, post-cap >64-key object (build in a loop via `o["k"+i]=i`), then `print(o)` + `json.stringify` + rest-destructure for each. (These pass TODAY — they are the behavior lock; they must still pass on slab.)
- [x] **Step 2:** Implement `Op::NewObject` (generic, ALWAYS — not specialize-gated; the representation is not toggleable, spec §8.3):

```rust
Op::NewObject => {
    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
    // Pop pairs source-order (as today, run.rs:2305-2319), folding duplicates
    // first-seen/later-wins into (ordered_keys, ordered_values).
    // Then intern: shape_for(ordered_keys) →
    //   Some(shape) → ObjectCell::new_slab(reg.keys_of(shape), ordered_values, shape)
    //   None (caps) → ObjectCell::new(IndexMap from the pairs)   // dict, shape 0 — today's path
    fiber.push(Value::Object(cell));
}
```

`AppendObject`/`SpreadObject`-per-entry/`SetIndex`-object-arm/`vm_set_prop`-object-miss all route through ONE `Vm` helper replacing `set_member`-then-resync for `Value::Object` receivers:

```rust
/// Store `name = value` on an OBJECT cell, preserving exact IndexMap semantics:
/// existing key → overwrite in place (shape unchanged); new key → ONE registry
/// transition + slab append, or demotion on cap refusal; dict-mode → plain insert.
/// Frozen check is the CALLER's (unchanged sites). Replaces resync_object_shape.
fn vm_object_insert(&self, cell: &Cc<ObjectCell>, name: &str, value: Value) {
    if let Some(i) = cell.get_index_of(name) { cell.set_value_at(i, value); return; }
    let shape = cell.shape.get();
    if shape != 0 || cell.is_slab() {
        let mut reg = self.shapes.borrow_mut();
        if let Some(child) = reg.add_key(shape, name) {
            let keys = reg.keys_of(child);
            drop(reg);
            if cell.slab_append(child, keys, value) { return; }
        }
        cell.demote_to_dict();
    }
    cell.insert(name, value); // dict path
}
```

**Delete `resync_object_shape`** — every caller now does a precise transition. Non-object receivers in `SET_INDEX`/`SET_PROP` keep their exact `set_member`/`index_set` routing and panic messages (byte-identity).

- [x] **Step 3:** `cargo test --test vm_differential` BOTH configs — the full corpus + the new battery green. Any divergence is a bug in the wiring: fix the engine.
- [x] **Step 4: Independent review checkpoint** — reviewer hand-probes: empty literal `{}`, the spread-builder seed growing past the cap mid-spread, a frozen slab object write (same `cannot mutate a frozen object` panic, `vm_set_prop:5433` ordering unchanged — frozen check BEFORE any mode change), `--no-specialize` runs of the battery. **DONE:** completeness review found 4 unmigrated slab-panic sites (compress ×2, ffi_alloc, ai/json_schema, +interp TestSummary) — all fixed in `b69180c`+`0135dce` with non-vacuous VM-slab regression tests; reviewer re-verified zero `Cc<ObjectCell>::borrow()` survives outside the shim, 426/0 both configs, clippy 0/0 both.
- [x] **Step 5: Commit** — `git commit -m "feat(vm): slab-native object construction + precise shape transitions (resync deleted)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.2: the literal-shape site cache (`lit_shapes`, specialize-gated)

**Files:**
- Modify: `src/vm/chunk.rs` (the side table + accessors beside `field_ics:421`), `src/vm/run.rs` (`Op::NewObject` warm path)
- Test: inline `#[test]` in `src/vm/run.rs` (the `run.rs:7480` hand-built-chunk idiom) + `tests/vm_differential.rs`

- [x] **Step 1: Write the failing test** — hand-build a chunk with one `NewObject 2` site executed in a loop (the `run.rs:7480` idiom); after the run, assert `chunk.lit_shape(off)` is `Some` with the right shape and that two objects from the same site share their keys `Rc` (`Rc::ptr_eq`) AND their shape id; a duplicate-key site (`{a,a}`) gets `slot_of_pair = Some([0, 0])` and constructs `{a: later}`.
- [x] **Step 2: Implement** (spec §4): `LitShapeCache { shape: u32, keys: Rc<[Rc<str>]>, slot_of_pair: Option<Rc<[u16]>> }` (+ a `Negative` marker variant for cap-refused sites); `Chunk.lit_shapes: RefCell<OffsetMap<LitShapeCache>>`; in `Op::NewObject`, `if self.specialize` consult the cache first — warm hit pops `n` values into a pre-sized `Vec<Value>` (keys popped as `Rc` drops), applies `slot_of_pair` when present, `new_slab(keys.clone(), values, shape)`; first execution runs Task 3.1's generic path then records. `--no-specialize` never consults/records (spec §8.3 — generic still builds slabs). **DONE (`f23032c`):** both lanes call ONE shared `Vm::exec_new_object` helper (identity by construction); `lit_shapes` runtime-only (empty-on-load like `field_ics`), `ASO_FORMAT_VERSION` unchanged at 28.
- [x] **Step 3:** `cargo test --test vm_differential` both configs green (the three-way now exercises cached-slab vs generic-slab vs tree-walker-dict — the strongest possible cross-check). **426/0 both configs.** Independent reviewer PROVED the warm branch is live (eprintln probe: 4 fires on iterations 2–5 at stable off, 0 under `--no-specialize`), offset keys stable + collision-free across two sites, slot_of_pair later-wins + identity ordering correct, Negative cap-path no re-probe loop.
- [x] **Step 4: Commit** — `git commit -m "feat(vm): per-site literal shape cache — zero-hash object literal construction" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.3: IC read/write paths over the slab

**Files:**
- Modify: `src/vm/run.rs` (`ic_get_field:4751`, `vm_set_prop:5421`)
- Test: the existing IC differential battery + a new slab-specific test

- [x] **Step 1: Write the failing test** — `assert_three_way_matches` on a poly-site program (4 distinct literal shapes through one `fn get(o) { return o.x }`) + a mega-site (5+), + reads after demotion (delete mid-stream — extends the Phase-0 test).
- [x] **Step 2: Implement** — `ic_get_field` Object/Instance hit arms use `value_at(idx)`; miss arms use `get_index_of` (bounded scan on slab, hash on dict) and record ONLY when `shape != 0` (dict/demoted cells stay generic — today's shape-0 rule, unchanged guards `run.rs:4761/4794`). `vm_set_prop` hit arm uses `set_value_at`; the Object miss arm routes to `vm_object_insert` (3.1) then records the (possibly transitioned) fresh shape's index — replacing the `set_member`+resync+re-probe dance at `run.rs:5451-5462`. The defensive out-of-range fall-through is KEPT (belt and braces; spec §2.4). **DONE:** the Object read/write slab paths front-loaded into Tasks 1.3/3.1; Task 3.3 delivered the poly/mega/demotion LOCK battery (5 tests). Instance arms still use `b.fields` IndexMap (migrates in 3.4).
- [x] **Step 3:** Differential both configs green; `cargo test ic` unit suite green. **431/0 both configs.** Independent review found 2 vacuous tests (demotion test used `"k"+i` → Tier-2 panic, never built 64 keys; mega test saturated to Mega before any hit) — both FIXED (`29c9041`): template-string key build (`o[`k${i}`]`, verified len 72 > cap → demotion), warmed `get_mid` at non-zero slab index 2 pre-demotion, poly-HIT ramp for mega. Reviewer's independent saboteur (`value_at(0)` vs `value_at(idx)`) now catches 4/5 incl. both fixed tests.
- [x] **Step 4: Commit** — `git commit -m "feat(vm): IC fast paths read/write the slab directly" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (`29c9041`, amended)

### Task 3.4: instances — slab fields via the class base shape

**Files:**
- Modify: `src/value.rs` (`Instance.fields: ObjectStorage` + accessor methods mirroring `ObjectCell`'s), `src/vm/run.rs` (`vm_construct:5498`, `class_base_shape:5100` returns `(u32, Rc<[Rc<str>]>)`, instance arms of `ic_get_field`/`vm_set_prop`; DELETE `resync_instance_shape:5397`), `src/interp.rs` (tree-walker `construct` + `validate_into:5376/5520` build Dict-mode fields — unchanged semantics, new constructor call)
- Test: differential battery + `tests/vm_differential.rs` class corpus

- [x] **Step 1: Write the failing test** — `assert_three_way_matches`: declared+defaulted+inherited fields print in merged-schema order; `init` assigning declared fields (contract panics byte-identical — wrong-type assignment message/span unchanged); `init` adding an UNDECLARED field (transition); post-construction undeclared add; a >cap instance (demotes, still correct); `C.from({...})` instances (dict, as today); `instanceof` + method dispatch unaffected (MethodCache keys on class identity, not field shape). **13-test battery + reverse-order-init trap.**
- [x] **Step 2: Implement** — `vm_construct` allocates `Slab` pre-sized from the base shape's canonical keys (declared order = `merged_field_schema` = today's insertion order); defaults/ctor args fill by slot; the instance arm of `vm_set_prop` KEEPS routing declared/existing fields through `set_member` (the field-type CONTRACT chokepoint, `run.rs:5415-5418` — the slab never bypasses it) and handles the undeclared-NEW-field add via the instance flavor of `vm_object_insert`. Tree-walker instances: `fields: ObjectStorage::Dict` always (the oracle untouched). **DONE (`d939ae1`):** `Instance.fields: ObjectStorage` + accessors sharing `ObjectStorage` bodies with `ObjectCell`; VM builds Slab via incremental precise transitions (`vm_instance_insert`); `resync_instance_shape` + `class_base_shape`/`object_shape_for` DELETED as dead; contract hoisted to shared `Interp::check_instance_field_contract` (byte-identical message/span — verified span `3:7` both engines); tree-walker/`Class.from`/shared-freeze/airlock build Dict via `Instance::from_dict`.
- [x] **Step 3:** Differential both configs green, including every class/enum/interface example in the corpus. **442/0 both configs.** Reviewer-verified: only `Instance.fields` changed type (enum/`Class.fields`/`NativeObject.fields` untouched); shape soundness across defaults/reverse-order-init/undeclared-add/>64-demotion; dict paths byte-identical; saboteur (stale `slab_append` shape) caught ≥20 tests; GC two-arm trace sound; `ASO_FORMAT_VERSION` 28 unchanged.
- [x] **Step 4: Commit** — `git commit -m "feat(vm): instance fields on the class-base-shape slab (resync_instance_shape deleted)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.5: mode-coverage counters + the anti-false-green assertion

**Files:**
- Modify: `src/vm/run.rs` (counters), `tests/vm_differential.rs` (assertion)
- Test: this IS the test

- [x] **Step 1:** Add to `Vm`, **compiled only under `#[cfg(feature = "fuzzgen")]`** (the crate's self-dev-dependency enables `fuzzgen` for every `cargo test` in BOTH configs; production builds never do — the counters are *not there* on the hot path, the JIT-counter `cfg` discipline): `obj_slab_constructed / obj_dict_constructed / obj_demotions: Cell<u64>` + bump sites in `new_slab`-producing arms, the dict fallback arms, and `demote_to_dict` callers; expose `pub fn obj_mode_stats(&self) -> (u64, u64, u64)`. **DONE (`fd5880a`):** `ShapeStats` struct mirroring `CallFastStats`, all 6 bump sites + field + accessor + lib entry `vm_run_source_obj_mode_stats` gated `#[cfg(any(test, feature="fuzzgen", fuzzing))]`; production `cargo build` warning-clean. Reviewer-classified every `ObjectCell::new(` VM site (3 slab + 3 dict + demotion) — all counted.
- [ ] **Step 2:** Add the corpus coverage test (campaign Gate 15 — prove the paths RAN):

```rust
/// SHAPE coverage assertion: running the differential corpus must construct
/// BOTH storage modes and take at least one demotion — a zero in any column
/// means a path silently stopped executing (the anti-false-green rule).
#[tokio::test]
async fn shape_storage_corpus_exercises_both_modes_and_demotion() {
    let (slab, dict, demote) = run_corpus_collecting_mode_stats().await;
    assert!(slab > 0,   "no slab-mode object constructed on the corpus");
    assert!(dict > 0,   "no dictionary-mode object constructed on the corpus");
    assert!(demote > 0, "no slab→dict demotion exercised on the corpus");
}
```

- [x] **Step 2:** corpus coverage test `shape_storage_corpus_exercises_both_modes_and_demotion` — runs the example corpus IN-PROCESS (`all_corpus_examples` + skip helpers) plus an explicit 70-key-growth driver (deterministic demotion source until Phase-5 order-stress examples land).
- [x] **Step 3:** Run — must PASS with all three nonzero (the order-stress examples land in Phase 5; if `demote` is 0 before they land, drive it with the Phase-0/3.1 batteries — do not weaken the assertion). **PASS: slab=231, dict=142, demote=1, all >0 both configs.** Reviewer saboteur (suppress slab bumps → fails `slab>0`; suppress demote bump → fails `dict>0`) confirms each column bites.
- [x] **Step 4: Commit** — `git commit -m "test(vm): fuzzgen-gated storage-mode counters + corpus coverage assertion" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (`fd5880a`, amended)

### Task 3.6: Phase 3 holistic review

- [x] **Step 1:** Holistic subagent over the combined VM wiring: every panic message/span byte-identical (frozen, non-object spread, `NEW_OBJECT key is not a string constant`, contract violations — grep each literal string and diff against `main`); `resync_object_shape`/`resync_instance_shape` and their `Vec<String>` clones are GONE (grep returns nothing); `--no-specialize` genuinely builds slabs (assert via the 3.5 counters under `vm_run_source_generic`); chunk/Vm affinity debug-assert present; full differential + full suite + clippy, both configs. **APPROVED:** panic byte-identity confirmed (frozen via shared `frozen_kind`, contract via single shared `check_instance_field_contract`); resync/`class_base_shape`/`object_shape_for` gone (only doc/test-name mentions); `--no-specialize` empirically builds slabs (slab=2/dict=0 via `Vm::new_generic` probe); combined diff coherent, no opcode/`.aso` change; 443/0 both configs, clippy 0/0 both, production build warning-clean, `ASO_FORMAT_VERSION` 28.
- [x] **Step 2:** Findings fixed before phase close. **chunk/Vm affinity:** satisfied STRUCTURALLY (the `lit_shapes` warm path is self-contained — uses the `keys` Rc + shape id stored IN the cache entry, never re-derives from the current Vm's registry → cross-Vm confusion cannot occur; `.aso` load lands a fresh Vm with empty `lit_shapes`, debug-assert `aso.rs:784`). This self-contained-entry design is a stronger guarantee than the anticipated affinity assert, so no chunk→Vm identity state was added (documented design improvement, semantics unchanged).

---

## Phase 4 — interior hashing (FxHash where justified; SipHash kept where it matters)

### Task 4.1: Fx for the Vm tables + `user_globals`; the iteration-order audit

**Files:**
- Modify: `src/vm/run.rs` (`class_methods:74`, `class_static_methods:79`, `class_defaults:84`, `class_base_shapes:92` → `FxHashMap`; `user_globals:165` → `IndexMap<Rc<str>, GlobalSlot, FxBuildHasher>`)
- Test: existing suites (behavior-identical) + the audit

- [x] **Step 1: The audit FIRST (the divergence hazard):** for every table changing hasher, prove its iteration order never reaches output: `class_*` tables are get/insert/contains-only (verified at drafting: `run.rs:3780/4607/4643` — re-verify with grep); `user_globals` is iterated (`run.rs:714`) but `IndexMap` iteration is **insertion-ordered regardless of hasher** — cite this in the code comment. `ShapeRegistry` is already Fx from Task 2.1 (never iterated). Record the audit table in the commit body. **DONE:** audit table in commit body; `class_defaults` order comes from `Class.fields.keys()` (declared order), Fx map only `contains_key`/`get` (reviewer-verified, the subtle one).
- [x] **Step 2:** Swap the hashers; doc-comment each with its spec §6.1 justification line (pointer keys / source identifiers / bounded inflow). **Do NOT touch** `Map`/`Set`/`MapKey`/dict-mode `IndexMap`s or any decode-path map — the §6.2 security decision; add a tripwire comment on `MapCell`/`SetCell`: "SipHash is load-bearing (hash-flooding DoS) — do not 'optimize'". **DONE:** 4 interior tables → Fx; MapCell/SetCell/Dict tripwire comments; decode buffers untouched (reviewer grep-confirmed).
- [x] **Step 3:** Add the adversarial bound test (spec §6.2): a script driving 100k hostile DISTINCT dynamic keys through `o[k]=v` on slab objects must complete in linear time with the registry's transition count bounded by the caps (assert via a registry-size probe), and the same keys into a `Map` stay on SipHash (type-level: the hasher is the default `RandomState` — a compile-time `assert_type` or a unit test on the type alias). **DONE:** `tests/shape_security.rs` — 100k hostile keys demote to SipHash dict, 50k-vs-100k ratio 1.947× (<3×, robust, reviewer ran 3×); Map-SipHash type-level proof (adversarial narrow → compile error, reviewer-verified).
- [x] **Step 4:** Full suite + differential both configs; quick same-session `object_churn` sanity (must not regress). **443/0 both configs, clippy 0/0 both, object_churn ~2.3s stable.**
- [x] **Step 5: Commit** — `git commit -m "perf(vm): FxHash for interior tables (registry/class/user_globals); Map/Set keep SipHash (security)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (`9b9c118`)

---

## Phase 5 — the correctness corpus: order-stress examples, fuzzer axis, negative space

### Task 5.1: order-stress examples (intro + advanced)

**Files:**
- Create: `examples/object_order_stress.as`, `examples/advanced/object_order_pipeline.as`
- Test: they join the differential corpus automatically (`all_corpus_examples`)

- [x] **Step 1:** Write `examples/object_order_stress.as` printing (so the differential bites on ORDER): literal order; duplicate-key literal; add-order via `o.k=` and `o["k"]=`; spread merge later-wins-first-position + self-spread; rest collection; `object.delete` then reads/adds (the Phase-0 reproducer); json round-trip; a 70-key loop-built object (crosses `SLAB_MAX_KEYS` → dictionary-transition order); `keys`/`values`/`entries`. Then the SAME matrix on a class instance (declared/defaulted/inherited/`init`-added/post-added fields). **DONE: FEATURE-FREE (core `std/object` only — json round-trip swapped for `object.entries`→`fromEntries`) so it runs the full matrix under `--no-default-features` (not a vacuous import-error pass).**
- [x] **Step 2:** Write `examples/advanced/object_order_pipeline.as` — production-shaped, fully error-handled: build records from defaults via spread, validate with `Class.from` (`[value, err]` handled), delete transient keys, round-trip through `json` AND a `worker fn` (airlock order, spec §2.3 row 8), assert orders with explicit checks that print `ok`/diagnostics. Deterministic output, no ports/clock → NOT in `EXAMPLE_SKIPS`. **DONE: 14 `ok:` assertions, worker airlock order verified.**
- [x] **Step 3:** Verify: `target/release/ascript run examples/object_order_stress.as` and the advanced one — output identical across `run`, `run --tree-walker`, `--no-specialize` (via the differential), and `build`+`run *.aso`; `ascript fmt` idempotent; `ascript check` clean. **DONE + 2 recorded goldens (corpus requires one per byte-identical example). Reviewer-verified: goldens match the oracle, cross-mode identical (4-way, both builds), corpus 102→104, 443/0 both configs.**
- [x] **Step 4: Commit** — `git commit -m "examples(shape): order-stress corpus — literals/spread/delete/caps/instances/json/worker" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (`cf0f338`, amended)

### Task 5.2: fuzzer axis weight + generator coverage

**Files:**
- Modify: `src/fuzzgen/` (generator weights), `tests/property.rs`
- Test: the proptest battery + a fuzz smoke run

- [x] **Step 1:** Teach the generator to emit (or up-weight, if present) object spread, `object.delete`, rest destructuring, and loop-built wide objects so generated programs cross both modes; confirm via the 3.5 counters on a 200-seed batch (both nonzero — extend `stress_differential_many_seeds` with the stats assertion). **DONE:** 4 productions (composite_expr spread/delete/rest + stmt() wide-object demotion driver, all deterministic-int); non-ignored `generator_crosses_both_storage_modes_and_demotion` asserts slab>0 ∧ dict>0 ∧ demote>0 over 200 seeds. Non-vacuous by construction (demote>0 ⟹ wide-object fired — only >64-key generated construct).
- [x] **Step 2:** `cargo test --test property` green both configs; if cargo-fuzz is available, a smoke campaign: `cargo +nightly fuzz run differential -- -runs=50000` → zero divergences. **property 27/0 both configs; stress_differential 2000 seeds 0 divergences; vm_differential 443/0; clippy 0/0 both. (ORCHESTRATOR-VERIFIED: a reviewer report showed implausible tool_uses=1 — re-ran all gates directly; ASO 28 unchanged.)**
- [x] **Step 3: Commit** — `git commit -m "test(fuzz): object spread/delete/wide-object weight in the differential generator" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (`24b4507`)

### Task 5.3: negative-space guard (the no-format-change proof)

**Files:**
- Modify: `tests/aso.rs` (or a new focused test beside it)

- [x] **Step 1:** Write the test (the `srv_negative_space` precedent): assert `ASO_FORMAT_VERSION` equals a `const ASO_AT_MERGE_BASE` recorded from this branch's merge-base at branch time (27 at drafting — **never assert the literal as a campaign-wide fact**: SHAPE runs PARALLEL to DEFER, which bumps to 28; the message explains SHAPE must not bump it and the const is re-recorded on rebase); compile a fixed multi-feature source to `.aso` on this branch and assert the bytes are IDENTICAL to the same compile at the merge-base (commit the golden bytes, or assert structurally: opcode stream + const pool of a representative chunk unchanged); `Op` count unchanged. **DONE (`7b8203e`):** `tests/shape_negative_space.rs` — `ASO_AT_MERGE_BASE=28` pin + Op-variant-count pin (`Op::DeferPushMethod as u16 + 1 == 120`, no opcode added) + a core-language `subject.as` build→`.aso`→run round-trip. Merge-base byte-equivalence proven by AUDIT: `git diff main` touches only `src/vm/aso.rs` (+1 non-serializing `debug_assert`), ZERO `src/compile/`/`src/vm/opcode.rs` changes (the structural alternative — `.aso` is gitignored so a byte-golden can't be committed). NOTE: golden-via-merge-base-rebuild was AVOIDED (a worktree rebuild filled the disk — see [[no-merge-base-worktree-rebuilds]]).
- [x] **Step 2:** Run — PASS; **Commit** — `git commit -m "test(aso): SHAPE negative space — no opcode/.aso/format change" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` **3/3 pass both configs; vm_differential 443/0 both; clippy 0/0 both; ASO 28. (Verified in Phase-5 holistic 5.4.)**

### Task 5.4: Phase 5 holistic review

- [x] **Step 1:** Holistic subagent: the examples genuinely demote (counters), the fuzzer axis genuinely generates the new constructs (inspect 20 generated programs), the negative-space test trips on a planted 1-byte opcode change (saboteur check, then revert), four-mode + full suite + clippy both configs. **APPROVED (opus reviewer):** examples feature-free+deterministic+in-corpus; generator inspection showed spread=2/delete=8/rest=8/wide=69 over 200 seeds (real constructs, non-vacuous); ASO-version saboteur trips correctly; 443/0 + property 27/0 + negative-space 3/0 + clippy 0/0 all both configs. **FINDING FIXED (`0290a25`):** Op-count pin had an append blind-spot (anchored on `DeferPushMethod as u16+1`) — retightened to count valid `from_u8` discriminants over 0..=255 (trips on a variant added anywhere) + a complementary last-variant sanity pin; 3/3 still pass, clippy clean.

---

## Phase 6 — performance, docs, full gates

### Task 6.1: same-session A/B + allocation/RSS report + cap tuning + zero-cost gates

**Files:**
- Create: `bench/SHAPE_RESULTS.md`
- Test: `tests/vm_bench.rs` (the standing gates)

- [ ] **Step 1: Same-session A/B (Gate 16):** in ONE session on one machine, build `main` and `feat/shape-storage` (`--profile profiling`), run `bench/profiling/run.sh` workloads (`json_roundtrip`, `object_churn`; plus the LANE Task-0 functional corpus if merged by now) 5× each, record medians; profile both with the shipped profiler (`ascript run --profile cpu`) and capture the hashing/allocation attribution deltas.
- [ ] **Step 2: Gate 18:** record allocation counts + peak RSS per workload (`/usr/bin/time -l`, plus a `count_allocations`-style harness run if available). A memory regression is a bug to fix in this task, not a tradeoff.
- [ ] **Step 3: Tune the tunables:** sweep `SLAB_MAX_KEYS ∈ {32, 64, 128}` × `SHAPE_FANOUT_MAX ∈ {64, 128, 256}` on `object_churn` + the order-stress examples; record the sweep table; keep the spec defaults unless the data says otherwise (then update spec + constants together).
- [ ] **Step 4: Gate 12 + DBG:** `cargo test --test vm_bench` — spec/tw geomean ≥ 2× holds; the dispatch loop was touched (`NewObject`/prop arms) so **re-run `dbg_zero_cost_gate`** (instrument==None ≈ armed-idle, bound 1.05×) and record both numbers.
- [ ] **Step 5:** Write `bench/SHAPE_RESULTS.md`: headline table (before/after per workload: time, alloc count, peak RSS, hashing% from the profiler), the honest `json_roundtrip` bound (decoded objects are born dict — SipHash kept by design, spec §9), the cap sweep, the Gate-12/DBG numbers, machine/date/commit hashes. **Expectations were stated, results are measured — if a number disappoints, the report says so and why.**
- [ ] **Step 6: Commit** — `git commit -m "bench(shape): same-session A/B + allocation/RSS report + cap tuning" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 6.2: docs + status

**Files:**
- Modify: `CLAUDE.md` (the "Object/instance SHAPES" bullet → describe slab/dict storage, the demotion rules, the lit_shapes side table, the delete-bug lesson), `superpowers/roadmap.md` (the SHAPE entry), `goal-perf.md` (status table 🏗️ → ✅ at merge), `docs/content/language/values-types.md` (one user-visible note: object key order is guaranteed and unchanged; no new page → no `NAV` change).

- [ ] **Step 1:** Write the updates; serve the docs site (`cd docs && python3 -m http.server`) and sanity-check the edited page renders.
- [ ] **Step 2: Commit** — `git commit -m "docs(shape): CLAUDE.md storage model, roadmap, goal-perf status, values-types note" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 6.3: full matrix + whole-effort holistic review (Definition of Done)

- [ ] **Step 1:** `cargo test` (default) — ALL test binaries green, 0 failures.
- [ ] **Step 2:** `cargo test --no-default-features` — green.
- [ ] **Step 3:** `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` — clean.
- [ ] **Step 4:** `cargo test --test vm_differential` both configs — full corpus + goldens + the new batteries, byte-identical four-mode.
- [ ] **Step 5:** Coverage assertion (3.5) all-nonzero; property suite + saboteur green; fuzz smoke (if available) zero divergences; negative-space green.
- [ ] **Step 6:** Gate 12 (≥2× geomean) + `dbg_zero_cost_gate` numbers recorded in `bench/SHAPE_RESULTS.md`; Gate 18 alloc/RSS recorded; no unexplained regression.
- [ ] **Step 7:** Tooling parity confirmed-working: no grammar/syntax change (no tree-sitter regen, no editor-pin bump), `fmt` idempotent over the new examples, REPL session sanity (objects across lines), LSP suite green.
- [ ] **Step 8:** Whole-effort holistic-review subagent over the ENTIRE branch diff: spec §1–§10 coverage table; zero TODO/placeholder/silent deferral; every discovered bug (incl. Phase 0) has a failing-test-first regression guard; the tree-walker diff is ZERO behavior lines (accessor renames only — verify `interp.rs` semantics untouched by reading the diff); invariants intact (`Value: !Send` assertion, no borrow across await, native handles untraced).
- [ ] **Step 9:** Every checkbox in this plan ticked. Merge `feat/shape-storage` → `main` with `--no-ff`; update `goal-perf.md` status table (SHAPE → ✅) in the merge; NANB is now unblocked.

---

## Self-review (author pass)

- **Spec coverage:** §1.1 bug → Phase 0; §2 slab + order proof → Phases 1–3 + the 2.3-table-driven batteries (each table row has a named test in 3.1/3.4/5.1); §3 dict fallback + caps → 2.1/2.2/3.1 + the coverage assertion; §4 literal site cache → 3.2; §5 instances → 3.4; §6 hashing + security → 4.1; §7 GC → 2.3; §8 correctness → 3.5/5.x; §9 performance → 6.1; §10 rejections respected (no Value variant, no `.aso` change — 5.3 guards it, no tree-walker change, no dict→slab).
- **No placeholders:** every code-changing task shows the concrete types/signatures later tasks consume (`ObjectStorage`, `new_slab`, `slab_append`, `demote_to_dict`, `add_key -> Option`, `keys_of`, `LitShapeCache`, `vm_object_insert`, the accessor list) — names are referenced consistently across Tasks 1.1 → 2.2 → 3.1 → 3.2 → 3.3 → 3.4.
- **Risk ordering:** the mechanical (accessor) migration lands and is proven behavior-identical BEFORE the representation flips; the representation flips on the generic path BEFORE the site cache speeds it up; counters land before the corpus that must light them up.
- **The oracle is never touched:** every phase's gate is the four-mode differential with the tree-walker on unchanged `IndexMap` dict cells — the strongest available proof that order and values survived the rewrite.
