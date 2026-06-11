//! End-to-end tests for `ascript build` (→ `.aso`), running a `.aso` on the VM,
//! and FILE-module import resolution (`.as`/`.aso`) — VM plan V12-T4.
//!
//! Each test runs the real built binary (`CARGO_BIN_EXE_ascript`) in its OWN unique
//! temp directory so the module cache and `.aso` files of one test never leak into
//! another. The central invariant proven throughout: `run <stem>.aso` (VM path)
//! produces byte-identical stdout + exit code to `run <stem>.as` (tree-walker).

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// A fresh, unique temp directory for one test (created; caller writes files into it).
fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ascript_aso_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, src).unwrap();
    p
}

/// Run the binary with `args` (cwd = `dir`), returning (stdout, exit_code).
fn run(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// `ascript build <file>` must succeed and produce the expected `.aso`.
fn build(dir: &Path, file: &str) {
    let out = Command::new(bin())
        .arg("build")
        .arg(file)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build {file} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// build + run .aso == run .as
// ---------------------------------------------------------------------------

#[test]
fn build_then_run_aso_matches_tree_walker_simple() {
    let dir = unique_dir("simple");
    write(&dir, "p.as", "let x = 2\nprint(x + 3 * 4)\n");
    build(&dir, "p.as");
    assert!(dir.join("p.aso").exists(), "p.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "p.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "p.as"]);
    assert_eq!(aso_out, "14\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_object_rest() {
    // Proves the V12-T2 object-rest serialization fix: `let {a, ...rest} = obj`
    // emits an Op::ObjectRest with a const Array-of-Str key list, which must now
    // serialize into the `.aso` and run byte-identically.
    let dir = unique_dir("objrest");
    write(
        &dir,
        "r.as",
        "let {a, ...rest} = {a: 1, b: 2, c: 3}\nprint(a)\nprint(rest)\n",
    );
    build(&dir, "r.as");

    let (aso_out, _) = run(&dir, &["run", "r.aso"]);
    let (as_out, _) = run(&dir, &["run", "r.as"]);
    assert_eq!(aso_out, "1\n{b: 2, c: 3}\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn build_then_run_aso_matches_classes() {
    let dir = unique_dir("class");
    write(
        &dir,
        "c.as",
        "class Point {\n  x: number\n  y: number\n  fn init(x, y) { self.x = x\n    self.y = y }\n  fn sum() { return self.x + self.y }\n}\nlet p = Point(3, 4)\nprint(p.sum())\n",
    );
    build(&dir, "c.as");

    let (aso_out, _) = run(&dir, &["run", "c.aso"]);
    let (as_out, _) = run(&dir, &["run", "c.as"]);
    assert_eq!(aso_out, "7\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn build_then_run_aso_matches_record_auto_init() {
    // SP2 §5 records: a field-only class (NO explicit `init`) auto-derives a
    // positional constructor at construction time from the proto's field schema —
    // no new serialized data, no `.aso` version bump. The built `.aso` must
    // round-trip and run byte-identically to the tree-walker (positional bind,
    // defaulted-field optional trailing param, base-first inheritance order).
    let dir = unique_dir("record");
    write(
        &dir,
        "r.as",
        "class Point {\n  x: number\n  y: number = 0\n}\nclass P3 extends Point { z: number }\n\
         print(Point(1).y)\nprint(Point(2, 3).y)\nlet r = P3(4, 5, 6)\nprint(r.x)\nprint(r.y)\nprint(r.z)\n",
    );
    build(&dir, "r.as");
    assert!(dir.join("r.aso").exists(), "r.aso should exist");

    let (aso_out, _) = run(&dir, &["run", "r.aso"]);
    let (as_out, _) = run(&dir, &["run", "--tree-walker", "r.as"]);
    assert_eq!(aso_out, "0\n3\n4\n5\n6\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
}

#[test]
fn build_then_run_aso_matches_generator_method() {
    // SP1 Phase B: a class with a `fn*` generator method (using `self`) compiles to
    // a generator `FnProto` that serializes through the `.aso` (generator protos
    // already round-trip) and runs byte-identically to the tree-walker.
    let dir = unique_dir("genmethod");
    write(
        &dir,
        "g.as",
        "class C { fn init() { self.n = 10 }\n  fn* g() { yield self.n\n    yield self.n + 1\n    yield self.n + 2 } }\n\
         let c = C()\nfor await (v in c.g()) { print(v) }\n",
    );
    build(&dir, "g.as");
    assert!(dir.join("g.aso").exists(), "g.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "g.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "g.as"]);
    assert_eq!(aso_out, "10\n11\n12\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_instanceof() {
    // SP2 §1: a program using the `instanceof` operator compiles to the (formerly
    // dead) `Op::InstanceOf` opcode, which round-trips through the `.aso` (format
    // v12) and runs byte-identically to the tree-walker.
    let dir = unique_dir("instanceof");
    write(
        &dir,
        "io.as",
        "class A {}\nclass B extends A {}\nlet b = B()\nprint(b instanceof A)\nprint(b instanceof B)\nprint(A() instanceof B)\nprint(5 instanceof A)\n",
    );
    build(&dir, "io.as");
    assert!(dir.join("io.aso").exists(), "io.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "io.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "io.as"]);
    assert_eq!(aso_out, "true\ntrue\nfalse\nfalse\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_adt_payload_enum() {
    // ADT: a payload-carrying enum — positional + named + unit variants, construction,
    // structural equality, `.value` reflection, and a variant-destructuring `match`
    // (positional bind + qualified unit) — round-trips through the `.aso` (per-variant
    // schema + constructed payload constants) and runs byte-identically to the
    // tree-walker. This is the `.aso` leg of the four-mode ADT differential.
    let dir = unique_dir("adt");
    write(
        &dir,
        "a.as",
        "enum Shape {\n  Circle(radius: float),\n  Pair(int, int),\n  Point,\n}\n\
         fn area(s: Shape): float {\n  return match s {\n    Circle(r) => r * r,\n    \
         Pair(a, b) => float(a * b),\n    Shape.Point => 0.0,\n  }\n}\n\
         let c = Shape.Circle(2.0)\nlet p = Shape.Pair(3, 4)\n\
         print(area(c))\nprint(area(p))\nprint(area(Shape.Point))\n\
         print(c == Shape.Circle(2.0))\nprint(c.value)\nprint(p.value)\n",
    );
    build(&dir, "a.as");
    assert!(dir.join("a.aso").exists(), "a.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "a.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "a.as"]);
    assert_eq!(aso_out, "4.0\n12.0\n0.0\ntrue\n{radius: 2.0}\n[3, 4]\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_default_params() {
    // SP2 §2: a function with default parameters — including a default that
    // references an EARLIER param and one composing with a rest param — compiles
    // its default-eval prologue into the body chunk (the param's default-presence
    // flag round-trips, format v13) and runs byte-identically to the tree-walker.
    let dir = unique_dir("defaultparams");
    write(
        &dir,
        "d.as",
        "fn f(a, b = a + 1, c = 10) { return [a, b, c] }\n\
         fn h(a, b = 2, ...xs) { return [a, b, xs] }\n\
         print(f(1))\nprint(f(1, 5))\nprint(f(1, 5, 6))\n\
         print(h(1))\nprint(h(1, 9, 8, 7))\n",
    );
    build(&dir, "d.as");
    assert!(dir.join("d.aso").exists(), "d.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "d.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "d.as"]);
    assert_eq!(
        aso_out,
        "[1, 2, 10]\n[1, 5, 10]\n[1, 5, 6]\n[1, 2, []]\n[1, 9, [8, 7]]\n"
    );
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_static_methods() {
    // SP1 Phase C: a class with a sync `static fn` factory AND a `static async fn`
    // factory (the blessed async-construction pattern) compiles to static protos
    // that round-trip through the `.aso` (format v10) and run byte-identically to
    // the tree-walker.
    let dir = unique_dir("staticmethods");
    write(
        &dir,
        "s.as",
        "class C {\n  fn init() { self.x = 0 }\n  static fn make(v) { let c = C()\n    c.x = v\n    return c }\n  static async fn create() { let c = C()\n    c.x = await 7\n    return c }\n}\nprint(C.make(3).x)\nlet c = await C.create()\nprint(c.x)\n",
    );
    build(&dir, "s.as");
    assert!(dir.join("s.aso").exists(), "s.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "s.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "s.as"]);
    assert_eq!(aso_out, "3\n7\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_stepped_match_pattern() {
    // RANGES FEATURE, Phase 5: a stepped MATCH-RANGE pattern (strided membership)
    // serializes through the `.aso` (the `Op::MATCH_RANGE` flags byte + the step
    // operand survive the round-trip) and runs byte-identically to the tree-walker.
    let dir = unique_dir("patstep");
    write(
        &dir,
        "m.as",
        // 3 ∈ {1,3,5,7,9} → "odd"; 4 ∈ {0,2,..} → "even"; 11 → out.
        "fn cls(n) { return match n { 1..=10 step 2 => \"odd\", 0..=10 step 2 => \"even\", _ => \"out\" } }\n\
         print(cls(3))\nprint(cls(4))\nprint(cls(11))\n",
    );
    build(&dir, "m.as");
    assert!(dir.join("m.aso").exists(), "m.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "m.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "m.as"]);
    assert_eq!(aso_out, "odd\neven\nout\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_stepped_field_default() {
    // RANGES FEATURE: a STEPPED range as a class FIELD DEFAULT lowers to
    // `ExprKind::Range { step: Some(..) }` and must survive the `.aso` round-trip,
    // running byte-identically to the tree-walker oracle.
    let dir = unique_dir("fieldstep");
    write(
        &dir,
        "f.as",
        "class Box { vals: array<number> = 1..10 step 2 }\nlet b = Box()\nprint(b.vals)\n",
    );
    build(&dir, "f.as");
    assert!(dir.join("f.aso").exists(), "f.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "f.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "f.as"]);
    assert_eq!(aso_out, "[1, 3, 5, 7, 9]\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_arrow_and_match_field_defaults() {
    // SP1 Phase E (E2): a class with an ARROW field default AND a `match` field
    // default. The VM lowers these by RE-PARSING the node source text through the
    // legacy front-end (`cst_default_expr` → `reparse_default_expr`); the `.aso`
    // serializer persists that source text and re-lowers it on load, so the built
    // bytecode runs byte-identically to the tree-walker oracle. Before E2, `ascript
    // build` rejected these defaults (`NonLiteralConst("arrow-default")`).
    let dir = unique_dir("arrowmatch");
    write(
        &dir,
        "f.as",
        "let base = 100\n\
         class C {\n  \
         f: fn = (n) => n + base\n  \
         label: string = match 2 { 1 => \"one\", 2 => \"two\", _ => \"many\" }\n  \
         doubled: array<number> = 1..=3\n  \
         fn init() {}\n}\n\
         let c = C()\nprint(c.f(5))\nprint(c.label)\nprint(c.doubled)\n",
    );
    build(&dir, "f.as");
    assert!(dir.join("f.aso").exists(), "f.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "f.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "f.as"]);
    assert_eq!(aso_out, "105\ntwo\n[1, 2, 3]\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_arrow_match_via_from() {
    // The `.from` path reads `FieldSchema.default` and EVALUATES it (the construct
    // path uses the compiled thunk). Both come from the SAME re-parsed source, so a
    // missing field filled by an arrow/match default through `.from` must also be
    // byte-identical after the `.aso` round-trip.
    let dir = unique_dir("arrowmatchfrom");
    write(
        &dir,
        "f.as",
        "class C {\n  \
         id: number\n  \
         label: string = match 1 { 1 => \"one\", _ => \"z\" }\n}\n\
         let c = C.from({id: 7})\nprint(c.id)\nprint(c.label)\n",
    );
    build(&dir, "f.as");

    let (aso_out, aso_code) = run(&dir, &["run", "f.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "f.as"]);
    assert_eq!(aso_out, "7\none\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_map_literals() {
    // SP2 Phase C: `#{…}` map literals must round-trip through `.aso` (the new
    // `Op::NewMap`/`Op::MapEntry` opcodes + a `#{…}` class-field default that the
    // field-default serializer persists as `EX_MAP`). Built bytecode must run
    // byte-identically to the tree-walker oracle.
    let dir = unique_dir("maplit");
    write(
        &dir,
        "f.as",
        "import * as map from \"std/map\"\n\
         class C {\n  \
         m: map<string, number> = #{ \"a\": 1, \"b\": 2 }\n  \
         fn init() {}\n}\n\
         let c = C()\n\
         let top = #{ 1: \"x\", 2: \"y\", 1: \"z\" }\n\
         print(map.get(c.m, \"b\"))\n\
         print(map.get(top, 1))\n\
         print(#{})\n",
    );
    build(&dir, "f.as");
    assert!(dir.join("f.aso").exists(), "f.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "f.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "--tree-walker", "f.as"]);
    assert_eq!(aso_out, "2\nz\nmap {}\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

// ---------------------------------------------------------------------------
// FILE-module import resolution (named + namespace)
// ---------------------------------------------------------------------------

#[test]
fn named_file_import_on_vm_matches_tree_walker() {
    let dir = unique_dir("named");
    write(
        &dir,
        "mathmod.as",
        "export fn add(x, y) { return x + y }\nexport const PI = 3\n",
    );
    write(
        &dir,
        "main.as",
        "import { add, PI } from \"./mathmod\"\nprint(add(2, 3))\nprint(PI)\n",
    );
    build(&dir, "mathmod.as");
    build(&dir, "main.as");

    let (aso_out, _) = run(&dir, &["run", "main.aso"]);
    let (as_out, _) = run(&dir, &["run", "main.as"]);
    assert_eq!(aso_out, "5\n3\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn namespace_file_import_on_vm_matches_tree_walker() {
    let dir = unique_dir("ns");
    write(
        &dir,
        "mathmod.as",
        "export fn add(x, y) { return x + y }\nexport const PI = 3\n",
    );
    write(
        &dir,
        "nsmain.as",
        "import * as m from \"./mathmod\"\nprint(m.add(10, 20))\nprint(m.PI)\n",
    );
    build(&dir, "mathmod.as");
    build(&dir, "nsmain.as");

    let (aso_out, _) = run(&dir, &["run", "nsmain.aso"]);
    let (as_out, _) = run(&dir, &["run", "nsmain.as"]);
    assert_eq!(aso_out, "30\n3\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn transitive_file_imports_on_vm() {
    // a imports b imports c.
    let dir = unique_dir("transitive");
    write(&dir, "c.as", "export const C = 100\n");
    write(
        &dir,
        "b.as",
        "import { C } from \"./c\"\nexport fn bfn() { return C + 1 }\n",
    );
    write(&dir, "a.as", "import { bfn } from \"./b\"\nprint(bfn())\n");
    build(&dir, "a.as");

    let (aso_out, _) = run(&dir, &["run", "a.aso"]);
    let (as_out, _) = run(&dir, &["run", "a.as"]);
    assert_eq!(aso_out, "101\n");
    assert_eq!(aso_out, as_out);
}

// ---------------------------------------------------------------------------
// .aso / .as precedence rules
// ---------------------------------------------------------------------------

#[test]
fn aso_only_dependency_runs_without_source() {
    // SELF-CONTAINED-BUNDLES: `build use.as` EMBEDS `./dep` into the artifact, so after
    // deleting BOTH dep sources (dep.as removed below; the embedded copy lives in use.aso)
    // the program still runs — the self-contained artifact carries its whole module graph.
    let dir = unique_dir("asoonly");
    write(&dir, "dep.as", "export const V = 42\n");
    write(&dir, "use.as", "import { V } from \"./dep\"\nprint(V)\n");
    build(&dir, "dep.as");
    build(&dir, "use.as");
    std::fs::remove_file(dir.join("dep.as")).unwrap();

    let (out, code) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out, "42\n");
    assert_eq!(code, 0);
}

#[test]
fn built_aso_embeds_deps_and_is_frozen_until_rebuilt() {
    // SELF-CONTAINED-BUNDLES: a multi-module `build use.as` embeds the whole reachable
    // module graph (`./dep`) into the produced `.aso` (an `ASCRIPTA` archive), so the
    // artifact is SELF-CONTAINED and FROZEN at build time — running it does NOT consult
    // (or recompile from) the on-disk `dep.as`. Editing the source after the build does
    // not change the artifact; you REBUILD to pick up the change. (This intentionally
    // replaces the pre-feature "recompile a stale dependency `.aso` from newer source"
    // behavior, which was the external-reference model self-containment deliberately
    // removed: the user asked for imports to be INCLUDED, not referenced externally. The
    // live-edit dev workflow is `ascript run use.as`, which always recompiles from source.)
    let dir = unique_dir("frozen");
    write(&dir, "dep.as", "export const V = \"old\"\n");
    write(&dir, "use.as", "import { V } from \"./dep\"\nprint(V)\n");
    build(&dir, "use.as"); // embeds dep="old" into use.aso

    // The built artifact runs the embedded dep → "old".
    let (out1, _) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out1, "old\n");

    // Editing the source AFTER the build does NOT change the frozen artifact: it embeds
    // the build-time dep, and a re-run resolves the import from the embedded archive (an
    // archive hit), never the (now-newer) on-disk `dep.as`.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&dir, "dep.as", "export const V = \"new\"\n");
    let (out2, _) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out2, "old\n", "a built .aso is self-contained — it does not pick up source edits");

    // REBUILDING the artifact embeds the new source → "new".
    build(&dir, "use.as");
    let (out3, _) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out3, "new\n", "rebuilding re-embeds the edited dependency");
}

#[test]
fn built_aso_ignores_corrupt_ondisk_dep() {
    // SELF-CONTAINED-BUNDLES: because a multi-module `build use.as` EMBEDS `./dep` into
    // the artifact, running `use.aso` resolves the import from the embedded archive (an
    // archive hit) and NEVER consults the on-disk `dep.aso` — so a corrupt/garbage
    // sibling `dep.aso` is irrelevant and cannot break the self-contained run. (This
    // replaces the pre-feature "corrupt dep.aso falls back to recompiling dep.as" test:
    // under the external-reference model the loader consulted the on-disk dep; under
    // self-containment the embedded dep is authoritative. The corrupt→recompile fallback
    // still exists for the archive-MISS disk path, just not for an embedded module.)
    let dir = unique_dir("corrupt");
    write(&dir, "dep.as", "export const V = \"src\"\n");
    write(&dir, "use.as", "import { V } from \"./dep\"\nprint(V)\n");
    build(&dir, "use.as"); // embeds dep="src"
    // Drop a garbage `dep.aso` next to the artifact — the self-contained run must ignore it.
    std::fs::write(dir.join("dep.aso"), b"GARBAGE-NOT-A-VALID-ASO").unwrap();

    let (out, code) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out, "src\n", "embedded dep is authoritative; the corrupt on-disk dep.aso is ignored");
    assert_eq!(code, 0);
}

#[test]
fn corrupt_aso_entry_errors_clearly() {
    // Directly running a corrupt `.aso` (no recompile fallback for the ENTRY file)
    // must fail with a non-zero exit and a clear error.
    let dir = unique_dir("entrycorrupt");
    std::fs::write(dir.join("bad.aso"), b"NOT-A-VALID-ASO-FILE").unwrap();
    let out = Command::new(bin())
        .arg("run")
        .arg("bad.aso")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "corrupt .aso entry should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot load") || stderr.contains("bad.aso"),
        "expected a clear load error, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// .as cutover: `run <file.as>` now executes on the bytecode VM (tree-walker kept
// as the differential oracle, reachable via ASCRIPT_ENGINE=tree-walker).
// ---------------------------------------------------------------------------

/// Repo root (CARGO_MANIFEST_DIR) so tests can run the real `examples/*.as`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Run the binary from the repo root (so relative `examples/...` paths resolve),
/// optionally setting `ASCRIPT_ENGINE`. Returns (stdout, exit_code).
fn run_root(args: &[&str], engine: Option<&str>) -> (String, i32) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(repo_root());
    match engine {
        Some(v) => {
            cmd.env("ASCRIPT_ENGINE", v);
        }
        None => {
            cmd.env_remove("ASCRIPT_ENGINE");
        }
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// The cutover gate: `run <file.as>` (now VM) is byte-identical to the same
/// program built + run as `.aso` (VM) AND to the tree-walker (oracle escape hatch),
/// across a hello, an imports example, and a class/compute example.
#[test]
fn run_as_on_vm_matches_aso_and_tree_walker() {
    for example in [
        "examples/hello.as",
        "examples/modules/main.as",
        "examples/oop.as",
    ] {
        // Default (no env): VM path.
        let (vm_out, vm_code) = run_root(&["run", example], None);
        // Oracle escape hatch: tree-walker.
        let (tw_out, tw_code) = run_root(&["run", example], Some("tree-walker"));
        assert_eq!(
            vm_out, tw_out,
            "{example}: VM stdout must match tree-walker oracle"
        );
        assert_eq!(
            vm_code, tw_code,
            "{example}: VM exit must match tree-walker"
        );

        // And byte-identical to the built .aso (VM, no compile step at run time).
        let dir = unique_dir("cutover");
        let src = std::fs::read_to_string(repo_root().join(example)).unwrap();
        write(&dir, "prog.as", &src);
        // hello/oop are self-contained; modules/main imports siblings, so copy them.
        if example == "examples/modules/main.as" {
            let moddir = repo_root().join("examples/modules");
            for entry in std::fs::read_dir(&moddir).unwrap() {
                let p = entry.unwrap().path();
                if p.extension().and_then(|e| e.to_str()) == Some("as") {
                    let name = p.file_name().unwrap().to_str().unwrap();
                    if name != "main.as" {
                        std::fs::copy(&p, dir.join(name)).unwrap();
                    }
                }
            }
        }
        build(&dir, "prog.as");
        let (aso_out, aso_code) = run(&dir, &["run", "prog.aso"]);
        assert_eq!(
            aso_out, vm_out,
            "{example}: .aso stdout must match VM .as run"
        );
        assert_eq!(
            aso_code, vm_code,
            "{example}: .aso exit must match VM .as run"
        );
    }
}

/// WS3 Deliverable 1 — the comprehensive all-features showcase example. The
/// three execution paths must agree byte-for-byte: VM-from-source (`run .as`),
/// the built `.aso` (VM bytecode), and the tree-walker oracle (`--tree-walker`).
///
/// Gated on `data`: the example imports `std/json` to showcase serialization,
/// which is unavailable under `--no-default-features` (the import would error
/// identically on both engines, with no exit-0 reference output to compare). The
/// VM-vs-tree-walker parity for the bare-language constructs in this file is
/// still covered by the whole-corpus differential in `tests/vm_differential.rs`.
#[cfg(feature = "data")]
#[test]
fn all_features_example_aso_vm_and_tree_walker_agree() {
    let example = "examples/all_features.as";

    // VM from source vs the tree-walker oracle.
    let (vm_out, vm_code) = run_root(&["run", example], None);
    let (tw_out, tw_code) = run_root(&["run", example], Some("tree-walker"));
    assert_eq!(
        vm_out, tw_out,
        "{example}: VM stdout must match tree-walker"
    );
    assert_eq!(
        vm_code, tw_code,
        "{example}: VM exit must match tree-walker"
    );
    assert_eq!(vm_code, 0, "{example}: should exit 0");
    assert!(
        vm_out.ends_with("all_features ok\n"),
        "{example}: should end with the success line, got tail: {:?}",
        &vm_out[vm_out.len().saturating_sub(40)..]
    );

    // Build to .aso and run the bytecode: must match the VM-from-source run.
    let dir = unique_dir("allfeatures");
    let src = std::fs::read_to_string(repo_root().join(example)).unwrap();
    write(&dir, "prog.as", &src);
    build(&dir, "prog.as");
    assert!(dir.join("prog.aso").exists(), "prog.aso should exist");
    let (aso_out, aso_code) = run(&dir, &["run", "prog.aso"]);
    assert_eq!(
        aso_out, vm_out,
        "{example}: .aso stdout must match VM .as run"
    );
    assert_eq!(
        aso_code, vm_code,
        "{example}: .aso exit must match VM .as run"
    );
}

/// WS3 Deliverable 2 — the multi-file local-import application (`examples/app/`):
/// `main.as` imports `shapes.as` (which TRANSITIVELY imports `util.as`) plus a
/// namespace import of `util.as`. Building the entry point must resolve the local
/// imports, and the resulting `.aso` (VM) must be byte-identical to both the
/// VM-from-source run and the tree-walker oracle.
#[test]
fn local_import_app_example_aso_vm_and_tree_walker_agree() {
    let entry = "examples/app/main.as";

    // VM from source vs the tree-walker oracle (relative ./imports resolve from
    // the repo root because run_root sets cwd to the repo root).
    let (vm_out, vm_code) = run_root(&["run", entry], None);
    let (tw_out, tw_code) = run_root(&["run", entry], Some("tree-walker"));
    assert_eq!(vm_out, tw_out, "{entry}: VM stdout must match tree-walker");
    assert_eq!(vm_code, tw_code, "{entry}: VM exit must match tree-walker");
    assert_eq!(vm_code, 0, "{entry}: should exit 0");
    assert!(
        vm_out.ends_with("app ok\n"),
        "{entry}: should end with the success line, got: {vm_out:?}"
    );

    // Copy the whole app/ module set into a unique temp dir, build each module +
    // the entry, then run the entry `.aso` — mirrors the existing transitive
    // file-import tests. The built `.aso` must match the VM-from-source run.
    let dir = unique_dir("appimport");
    let appdir = repo_root().join("examples/app");
    for name in ["util.as", "shapes.as", "main.as"] {
        std::fs::copy(appdir.join(name), dir.join(name)).unwrap();
    }
    // Build leaf-first so each dependency's `.aso` exists when the next is built;
    // building `main.as` resolves its (already-built) local imports.
    build(&dir, "util.as");
    build(&dir, "shapes.as");
    build(&dir, "main.as");
    assert!(dir.join("main.aso").exists(), "main.aso should exist");
    let (aso_out, aso_code) = run(&dir, &["run", "main.aso"]);
    assert_eq!(
        aso_out, vm_out,
        "{entry}: .aso stdout must match VM .as run"
    );
    assert_eq!(
        aso_code, vm_code,
        "{entry}: .aso exit must match VM .as run"
    );
}

/// The oracle escape hatch stays CLI-reachable: `ASCRIPT_ENGINE=tree-walker`
/// routes `.as` back to the tree-walker and still produces correct output.
#[test]
fn ascript_engine_tree_walker_escape_hatch_works() {
    let (out, code) = run_root(&["run", "examples/hello.as"], Some("tree-walker"));
    assert_eq!(out, "7\n");
    assert_eq!(code, 0);
}

/// A Tier-2 panic in a `.as` program run on the VM renders a proper diagnostic
/// (source attached: file path + line/col) and exits non-zero.
#[test]
fn run_as_on_vm_panic_shows_diagnostic() {
    let dir = unique_dir("panic");
    write(&dir, "boom.as", "let y = nil\nprint(y.field)\n");
    let out = Command::new(bin())
        .args(["run", "boom.as"])
        .current_dir(&dir)
        .env_remove("ASCRIPT_ENGINE")
        .output()
        .unwrap();
    assert_eq!(out.status.code().unwrap_or(-1), 1, "panic should exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("boom.as") && stderr.contains("cannot read property"),
        "diagnostic should name the file and the error; got: {stderr}"
    );
}

#[test]
fn build_refuses_compile_error() {
    let dir = unique_dir("builderr");
    write(&dir, "bad.as", "fn f(x x) {}\n"); // malformed param list — compile error
    let out = Command::new(bin())
        .arg("build")
        .arg("bad.as")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "build of a bad program should fail");
    assert!(
        !dir.join("bad.aso").exists(),
        "no .aso should be written on compile error"
    );
}
