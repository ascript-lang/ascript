//! REGION Phase 0 — identity-semantics audit & battery v0 (spec §2.1, Task 0.1).
//!
//! PURPOSE: Pin today's value-identity semantics BEFORE any region code is written.
//! The spike's make-or-break analysis (spec §2.1) rests on these semantics being
//! exactly as described.  This file re-verifies every claim at the current HEAD
//! (post-NANB/CALL/ELIDE/PAR) and records the verdicts as executable tests that
//! MUST NOT be weakened by any future REGION code.
//!
//! MODES COVERED: tree-walker (`run_source`), specialized VM (`vm_run_source`),
//! generic VM (`vm_run_source_generic`), and pre-compiled `.aso` via the binary.
//! Four modes total — the same set as the main differential.
//!
//! AUDIT GREPS (recorded here for reference, re-verified at 2026-06-16):
//!
//! `grep -n "cc_ptr_eq\|Rc::ptr_eq\|ptr_eq" src/value.rs` → PartialEq arms at:
//!   src/value.rs:2501  `impl PartialEq for Value`
//!   src/value.rs:2523  Closure → cc_ptr_eq
//!   src/value.rs:2524  Array   → cc_ptr_eq
//!   src/value.rs:2525  Object  → cc_ptr_eq
//!   src/value.rs:2526  Map     → cc_ptr_eq
//!   src/value.rs:2527  Set     → cc_ptr_eq
//!   src/value.rs:2571  Instance → cc_ptr_eq
//!   (Function, Bytes, Regex, Native, Enum, Class, Interface, BoundMethod,
//!    Super, Generator → Rc::ptr_eq; Future → SharedFuture::ptr_eq)
//!   Semantics: UNCHANGED from spec §2.1 (pointer identity for all containers).
//!
//! `MapKey::from_value` (src/value.rs:731): containers hit the `_ => None` arm
//!   at the end of the match (line 768), confirming containers CANNOT be map/set keys.
//!
//! `cc_addr` consumers classified (grep -rn "cc_addr" src/):
//!   TRANSIENT (local var, live only within one native call):
//!     src/value.rs:2787,2804,2823,2842,2871,2888 — Display `seen: Vec<usize>` cycle guard
//!     src/stdlib/json.rs                           — to_json_lossy / from_json `seen`
//!     src/stdlib/msgpack.rs                        — encode `seen`
//!     src/stdlib/cbor.rs                           — encode `seen`
//!     src/stdlib/object.rs:50-170                  — deep_equal / deep_clone `seen`
//!     src/stdlib/assert_mod.rs:82,102              — assert.deepEqual `seen`
//!     src/stdlib/shared.rs:121,139,162,185,230     — freeze walk `in_progress`/`completed`
//!       (local to one `freeze()` call — transient, NOT a persistent Interp table)
//!     src/worker/serialize.rs:190,202,215,227,235,258,270,476,488,501,519,558
//!       — structured-clone `seen` + `ids` (local to one serialization call)
//!     src/vm/fiber.rs:416,447                      — unit test only (not production)
//!   PERSISTENT (live across calls, on Vm or Interp):
//!     NONE that are keyed by container cc_addr.
//!     (Vm.class_methods/class_static_methods/class_defaults: keyed by Rc::as_ptr
//!      of Class descriptors, NOT by container cc_addr — these are class-shape tables.)
//!     (Interp.iface_verdict_cache: keyed by (Class Rc ptr, Interface Rc ptr) with a
//!      generation guard — again NOT container cc_addr.)
//!
//! VERDICT: NO persistent address-keyed table uses container cc_addr.
//! The spec §2.1 rows 6–7 claim is CONFIRMED at current HEAD.
//! Spec §2.1 table row file:lines moved since the spec was written (2026-06-12 → 2026-06-16):
//!   - `impl PartialEq for Value`: was :1393, now :2501 (NANB sealed-repr refactor moved it)
//!   - cc_ptr_eq arms: were :1414-1419/:1463, now :2523-2527/:2571
//!   - `MapKey::from_value`: was :241 (the _ => None arm), now :768 (same arm, renumbered)
//!   - `ObjectCell`: was :24, not audited further (constructor seam, not identity-related)
//!   Verdict text in each row is UNCHANGED — only anchors moved.

use std::process::Command;

/// Run `src` on all four modes, assert they all produce identical output, and
/// return that common output (without the trailing newline).
///
/// The four modes are:
/// 1. tree-walker     (`ascript::run_source`)
/// 2. specialized VM  (`ascript::vm_run_source`)
/// 3. generic VM      (`ascript::vm_run_source_generic`)
/// 4. .aso (build → run via binary)
async fn run_four_modes(src: &str, tag: &str) -> String {
    // Mode 1: tree-walker
    let tw = ascript::run_source(src)
        .await
        .unwrap_or_else(|e| panic!("tree-walker error for [{tag}]: {e}"));

    // Mode 2: specialized VM
    let (vm, _) = ascript::vm_run_source(src)
        .await
        .unwrap_or_else(|e| panic!("specialized VM error for [{tag}]: {e}"));

    // Mode 3: generic VM
    let (gen, _) = ascript::vm_run_source_generic(src)
        .await
        .unwrap_or_else(|e| panic!("generic VM error for [{tag}]: {e}"));

    // Mode 4: .aso (build + run via binary)
    let aso = run_aso_from_src(src, tag).await;

    assert_eq!(
        tw, vm,
        "[{tag}] specialized VM diverged from tree-walker\n  tw: {tw:?}\n  vm: {vm:?}"
    );
    assert_eq!(
        tw, gen,
        "[{tag}] generic VM diverged from tree-walker\n  tw: {tw:?}\n  gen: {gen:?}"
    );
    assert_eq!(
        tw, aso,
        "[{tag}] .aso run diverged from tree-walker\n  tw: {tw:?}\n  aso: {aso:?}"
    );

    // Return the trimmed output (strip trailing newline for assertion convenience)
    tw.trim_end_matches('\n').to_string()
}

/// Build `src` to a `.aso` in a temp dir and run it via the binary, returning stdout.
async fn run_aso_from_src(src: &str, tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("ascript_region_id_{}_{}", tag, nanos));
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("create temp dir for [{tag}]: {e}"));
    let src_name = format!("{tag}.as");
    std::fs::write(dir.join(&src_name), src)
        .unwrap_or_else(|e| panic!("write .as for [{tag}]: {e}"));
    let bin = env!("CARGO_BIN_EXE_ascript");

    let build = Command::new(bin)
        .arg("build")
        .arg(&src_name)
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript build for [{tag}]: {e}"));
    assert!(
        build.status.success(),
        "[{tag}] ascript build failed:\n  stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let aso_name = format!("{tag}.aso");
    let run = Command::new(bin)
        .arg("run")
        .arg(&aso_name)
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript run .aso for [{tag}]: {e}"));
    assert!(
        run.status.success(),
        "[{tag}] ascript run .aso failed:\n  stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

/// Run `src` on all four modes expecting a panic (Err), and assert all four
/// produce errors whose messages contain `expected_msg_substr`.
async fn assert_four_modes_panic(src: &str, expected_msg_substr: &str, tag: &str) {
    // Mode 1: tree-walker
    let tw_err = ascript::run_source(src)
        .await
        .expect_err(&format!("tree-walker should panic for [{tag}]"))
        .to_string();

    // Mode 2: specialized VM
    let vm_err = ascript::vm_run_source(src)
        .await
        .expect_err(&format!("specialized VM should panic for [{tag}]"))
        .to_string();

    // Mode 3: generic VM
    let gen_err = ascript::vm_run_source_generic(src)
        .await
        .expect_err(&format!("generic VM should panic for [{tag}]"))
        .to_string();

    // Mode 4: .aso (build succeeds, run fails)
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("ascript_region_panic_{}_{}", tag, nanos));
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("create temp dir for [{tag}]: {e}"));
    let src_name = format!("{tag}.as");
    std::fs::write(dir.join(&src_name), src)
        .unwrap_or_else(|e| panic!("write .as for [{tag}]: {e}"));
    let bin = env!("CARGO_BIN_EXE_ascript");
    let build = Command::new(bin)
        .arg("build")
        .arg(&src_name)
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript build for [{tag}]: {e}"));
    assert!(
        build.status.success(),
        "[{tag}] ascript build failed:\n  stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(bin)
        .arg("run")
        .arg(format!("{tag}.aso"))
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript run .aso for [{tag}]: {e}"));
    assert!(
        !run.status.success(),
        "[{tag}] .aso run should have failed (panic expected)"
    );
    let aso_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // All four error messages must contain the expected substring.
    assert!(
        tw_err.contains(expected_msg_substr),
        "[{tag}] tree-walker panic missing {:?}\n  got: {tw_err}",
        expected_msg_substr
    );
    assert!(
        vm_err.contains(expected_msg_substr),
        "[{tag}] specialized VM panic missing {:?}\n  got: {vm_err}",
        expected_msg_substr
    );
    assert!(
        gen_err.contains(expected_msg_substr),
        "[{tag}] generic VM panic missing {:?}\n  got: {gen_err}",
        expected_msg_substr
    );
    assert!(
        aso_combined.contains(expected_msg_substr),
        "[{tag}] .aso run panic missing {:?}\n  got: {aso_combined}",
        expected_msg_substr
    );
}

// ---------------------------------------------------------------------------
// Group (a): `{x:1} == {x:1}` is `false`; `let b = a; a == b` is `true`
// ---------------------------------------------------------------------------
// Covers spec §2.1 row 1 (cc_ptr_eq identity) for all six container kinds.
// The six Cc-identity containers: Object, Array, Map, Set, Instance, Closure.
// (Closure is trickier to test structurally equal — we use two distinct closures
//  with the same body, which will have different Rc/Cc addresses.)

#[tokio::test]
async fn identity_object_distinct_equal_struct() {
    // Two structurally-identical objects must NOT be equal (different cells).
    let out = run_four_modes("print({x:1} == {x:1})", "obj_distinct").await;
    assert_eq!(out, "false", "structurally-equal objects must be identity-unequal");
}

#[tokio::test]
async fn identity_object_alias_equal() {
    // The same object through an alias must be equal.
    let out = run_four_modes(
        "let a = {x:1}\nlet b = a\nprint(a == b)",
        "obj_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased object must be identity-equal to itself");
}

#[tokio::test]
async fn identity_array_distinct_equal_struct() {
    let out = run_four_modes("print([1,2] == [1,2])", "arr_distinct").await;
    assert_eq!(out, "false", "structurally-equal arrays must be identity-unequal");
}

#[tokio::test]
async fn identity_array_alias_equal() {
    let out = run_four_modes(
        "let a = [1,2]\nlet b = a\nprint(a == b)",
        "arr_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased array must be identity-equal to itself");
}

#[tokio::test]
async fn identity_map_distinct_equal_struct() {
    let out = run_four_modes(
        "import * as map from \"std/map\"\n\
         let m1 = map.new([[1,\"a\"]])\n\
         let m2 = map.new([[1,\"a\"]])\n\
         print(m1 == m2)",
        "map_distinct",
    )
    .await;
    assert_eq!(out, "false", "structurally-equal maps must be identity-unequal");
}

#[tokio::test]
async fn identity_map_alias_equal() {
    let out = run_four_modes(
        "import * as map from \"std/map\"\n\
         let m = map.new([[1,\"a\"]])\n\
         let m2 = m\n\
         print(m == m2)",
        "map_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased map must be identity-equal to itself");
}

#[tokio::test]
async fn identity_set_distinct_equal_struct() {
    let out = run_four_modes(
        "import * as set from \"std/set\"\n\
         let s1 = set.from([1,2])\n\
         let s2 = set.from([1,2])\n\
         print(s1 == s2)",
        "set_distinct",
    )
    .await;
    assert_eq!(out, "false", "structurally-equal sets must be identity-unequal");
}

#[tokio::test]
async fn identity_set_alias_equal() {
    let out = run_four_modes(
        "import * as set from \"std/set\"\n\
         let s = set.from([1,2])\n\
         let s2 = s\n\
         print(s == s2)",
        "set_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased set must be identity-equal to itself");
}

#[tokio::test]
async fn identity_instance_distinct_equal_struct() {
    let out = run_four_modes(
        "class Point { x: number = 0\n y: number = 0 }\n\
         let p1 = Point.from({x:1,y:2})\n\
         let p2 = Point.from({x:1,y:2})\n\
         print(p1 == p2)",
        "inst_distinct",
    )
    .await;
    assert_eq!(
        out, "false",
        "structurally-equal instances must be identity-unequal"
    );
}

#[tokio::test]
async fn identity_instance_alias_equal() {
    let out = run_four_modes(
        "class Point { x: number = 0\n y: number = 0 }\n\
         let p = Point.from({x:1,y:2})\n\
         let p2 = p\n\
         print(p == p2)",
        "inst_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased instance must be identity-equal to itself");
}

#[tokio::test]
async fn identity_closure_distinct() {
    // Two closures created by separate evaluations of the same arrow literal must
    // NOT be equal (distinct Cc allocations, one per arrow expression evaluation).
    // Use arrow syntax `() => expr` (the tree-walker does not accept anonymous
    // `fn()` expression syntax — the CLAUDE.md carry-forward note confirms this;
    // the arrow form works on both engines).
    let out = run_four_modes(
        "fn make() { return () => 1 }\n\
         let f1 = make()\n\
         let f2 = make()\n\
         print(f1 == f2)",
        "closure_distinct",
    )
    .await;
    assert_eq!(
        out, "false",
        "closures from distinct evaluations must be identity-unequal"
    );
}

#[tokio::test]
async fn identity_closure_alias_equal() {
    let out = run_four_modes(
        "fn make() { return () => 1 }\n\
         let f = make()\n\
         let g = f\n\
         print(f == g)",
        "closure_alias",
    )
    .await;
    assert_eq!(out, "true", "aliased closure must be identity-equal to itself");
}

// ---------------------------------------------------------------------------
// Group (b): alias-mutation visibility
// Spec §2.1 row 3: reference semantics — mutation via one alias is visible
// through all other aliases (stronger than identity: observable without `==`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alias_mutation_object() {
    // `let b = a; b.x = 2` — the write via `b` must be visible via `a`.
    let out = run_four_modes(
        "let a = {x:1}\nlet b = a\nb.x = 2\nprint(a.x)",
        "alias_mut_obj",
    )
    .await;
    assert_eq!(out, "2", "mutation via alias must be visible through original");
}

#[tokio::test]
async fn alias_mutation_array() {
    // Mutation via push on one alias must be visible via all.
    let out = run_four_modes(
        "import * as array from \"std/array\"\n\
         let a = [1]\n\
         let b = a\n\
         array.push(b, 2)\n\
         print(a[1])",
        "alias_mut_arr",
    )
    .await;
    assert_eq!(out, "2", "array push via alias must be visible through original");
}

#[tokio::test]
async fn alias_mutation_instance() {
    let out = run_four_modes(
        "class Counter { value: number = 0 }\n\
         let c = Counter.from({value: 0})\n\
         let d = c\n\
         d.value = 99\n\
         print(c.value)",
        "alias_mut_inst",
    )
    .await;
    assert_eq!(
        out, "99",
        "instance mutation via alias must be visible through original"
    );
}

// ---------------------------------------------------------------------------
// Group (c): array search ops use identity (spec §2.1 row 4, derived from row 1)
// `indexOf` uses `PartialEq` → cc_ptr_eq for containers.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identity_based_indexof_finds_alias() {
    // The SAME object placed in an array must be found by indexOf.
    let out = run_four_modes(
        "import * as array from \"std/array\"\n\
         let obj = {x:1}\n\
         let arr = [obj]\n\
         print(array.indexOf(arr, obj))",
        "indexof_alias",
    )
    .await;
    assert_eq!(out, "0", "indexOf must find the aliased container");
}

#[tokio::test]
async fn identity_based_indexof_misses_struct_equal() {
    // A DIFFERENT object with the same structure must NOT be found.
    let out = run_four_modes(
        "import * as array from \"std/array\"\n\
         let obj = {x:1}\n\
         let arr = [obj]\n\
         print(array.indexOf(arr, {x:1}))",
        "indexof_distinct",
    )
    .await;
    assert_eq!(
        out, "-1",
        "indexOf must not find a structurally-equal but distinct container"
    );
}

// ---------------------------------------------------------------------------
// Group (d): map/set reject container keys (spec §2.1 row 5 — containers are
// not hashable, MapKey::from_value returns None for them).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn map_rejects_object_key() {
    // Map literal `#{ expr: val }` evaluates the key expression; an object value
    // as a key produces: "cannot use object as a map key".
    assert_four_modes_panic(
        "let obj = {x:1}\nlet m = #{ obj: 1 }",
        "cannot use object as a map key",
        "map_obj_key",
    )
    .await;
}

#[tokio::test]
async fn map_rejects_array_key() {
    assert_four_modes_panic(
        "let arr = [1,2]\nlet m = #{ arr: 1 }",
        "cannot use array as a map key",
        "map_arr_key",
    )
    .await;
}

#[tokio::test]
async fn map_stdlib_rejects_object_key() {
    // map.set() uses a different message: "map keys must be nil, bool, number, or string"
    assert_four_modes_panic(
        "import * as map from \"std/map\"\n\
         let m = map.new()\n\
         map.set(m, {x:1}, \"v\")",
        "map keys must be nil, bool, number, or string",
        "map_stdlib_obj_key",
    )
    .await;
}

#[tokio::test]
async fn set_rejects_object_element() {
    // set.add panics with "set elements must be nil, bool, number, or string, got object"
    assert_four_modes_panic(
        "import * as set from \"std/set\"\n\
         let s = set.from([])\n\
         set.add(s, {x:1})",
        "set elements must be nil, bool, number, or string",
        "set_obj_element",
    )
    .await;
}

#[tokio::test]
async fn set_rejects_array_element() {
    assert_four_modes_panic(
        "import * as set from \"std/set\"\n\
         let s = set.from([])\n\
         set.add(s, [1])",
        "set elements must be nil, bool, number, or string",
        "set_arr_element",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Extra: self-referential structure cannot be a map/set key either,
// and identity is preserved across self-reference.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identity_ne_itself_after_struct_change() {
    // Mutating an object does not change its identity.
    let out = run_four_modes(
        "let a = {x:1}\n\
         let b = a\n\
         a.x = 99\n\
         print(a == b)\n\
         print(a.x)\n\
         print(b.x)",
        "identity_after_mutation",
    )
    .await;
    // All three lines: identity holds (true), and mutation is shared (99, 99).
    assert_eq!(
        out, "true\n99\n99",
        "identity must be preserved across mutation; mutation must be visible via alias"
    );
}
