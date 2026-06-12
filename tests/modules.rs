use std::fs;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ascript_mod_{}_{}", tag, std::process::id()));
    let _ = fs::create_dir_all(&d);
    d
}

#[test]
fn named_and_namespace_imports() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("basic");
    fs::write(
        d.join("util.as"),
        "export const PI = 3\nexport fn double(x) { return x * 2 }\nfn secret() { return 99 }",
    )
    .unwrap();
    fs::write(d.join("main.as"),
        "import { PI, double } from \"./util\"\nimport * as u from \"./util\"\nprint(PI)\nprint(double(21))\nprint(u.double(5))").unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n42\n10\n");
}

#[test]
fn importing_non_export_errors() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("noexport");
    fs::write(d.join("lib.as"), "export const A = 1\nconst B = 2").unwrap();
    fs::write(d.join("app.as"), "import { B } from \"./lib\"\nprint(B)").unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("app.as"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no export 'B'"));
}

#[test]
fn module_body_runs_once() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("once");
    // side.as prints when loaded; importing it from two places must print once.
    fs::write(d.join("side.as"), "print(\"loaded\")\nexport const V = 1").unwrap();
    fs::write(
        d.join("a.as"),
        "import { V } from \"./side\"\nexport const A = V",
    )
    .unwrap();
    fs::write(
        d.join("main.as"),
        "import { V } from \"./side\"\nimport { A } from \"./a\"\nprint(V)\nprint(A)",
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // "loaded" appears exactly once despite two import paths to side.as.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "loaded\n1\n1\n");
}

#[test]
fn circular_import_resolves_partial() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("circular");
    // a imports b; b imports a. a defines X before importing b, so b can use it.
    fs::write(
        d.join("a.as"),
        "export const X = 10\nimport { Y } from \"./b\"\nexport fn sum() { return X + Y }",
    )
    .unwrap();
    fs::write(
        d.join("b.as"),
        "import { X } from \"./a\"\nexport const Y = X + 5",
    )
    .unwrap();
    fs::write(
        d.join("main.as"),
        "import { sum } from \"./a\"\nprint(sum())",
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // a.X=10 is defined before a imports b; b reads X=10, sets Y=15; a.sum()=25.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "25\n");
}

#[test]
fn from_resolves_inherited_default_in_base_module_scope() {
    // A base class declares a defaulted field whose default references a
    // module-scoped binding visible only in the base module. A subclass in
    // another module inherits the field; `Sub.from({...})` must resolve that
    // default in the *declaring* (base) class's def env — matching `Sub(...)`.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("from_inherited_default_scope");
    fs::write(
        d.join("base.as"),
        "const BASE_ROLE = \"admin\"\nexport class Base {\n  id: number\n  role: string = BASE_ROLE\n}",
    )
    .unwrap();
    fs::write(
        d.join("main.as"),
        "import { Base } from \"./base\"\nclass Sub extends Base {\n  name: string\n}\n\
         let s = Sub.from({ id: 1, name: \"x\" })\nprint(s.role)\nprint(s.id)\nprint(s.name)",
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // BASE_ROLE is not in scope in main.as; the default must still resolve to it.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "admin\n1\nx\n");
}

#[test]
fn exports_destructured_names() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("destructure_export");
    fs::write(d.join("lib.as"), "export let [a, b] = [1, 2]").unwrap();
    fs::write(
        d.join("main.as"),
        "import { a, b } from \"./lib\"\nprint(a)\nprint(b)",
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n2\n");
}

#[test]
fn module_import_defers_run_at_import() {
    // DEFER §2.3: a `defer` at an imported MODULE's top level must run when the
    // module body finishes loading (at import time), BEFORE the importer reads its
    // exports / runs its own body. The module body runs to completion during import;
    // its defers drain first. Tree-walker engine (the VM's defer is Phase 3).
    //
    // mod.as: top-level body prints "mod-body", registers `defer print("mod-defer")`,
    //         exports V. The defer must fire at the end of loading mod.as.
    // main.as: imports V, prints "main-body". Because mod.as finishes loading (and
    //          drains its defer) BEFORE main.as's body runs, the order is:
    //          mod-body, mod-defer, main-body.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("module_defer");
    fs::write(
        d.join("mod.as"),
        "print(\"mod-body\")\ndefer print(\"mod-defer\")\nexport const V = 1",
    )
    .unwrap();
    fs::write(
        d.join("main.as"),
        "import { V } from \"./mod\"\nprint(\"main-body\")\nprint(V)",
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg("--tree-walker")
        .arg(d.join("main.as"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The module's defer ran at import time: "mod-defer" appears AFTER "mod-body"
    // and BEFORE "main-body" (the importer's body runs only after the module loaded).
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "mod-body\nmod-defer\nmain-body\n1\n",
        "imported-module top-level defer must run at import time"
    );
}

// Gated on the `shared` feature: the built binary ships `std/shared` only when the
// feature is on (it folds into `default`). Under `--no-default-features` the import is
// an unknown-module error, so the test is skipped there.
#[cfg(feature = "shared")]
#[test]
fn srv_frozen_value_crosses_worker_airlock_zero_copy() {
    // SRV Task 6: a `shared.freeze`d value crosses the worker airlock by an Arc bump
    // (the TAG_SHARED side-vector path), is read inside the worker, and is byte-identical
    // on BOTH engines. Exercises dispatch_worker → WorkerRequest.shared → decode_args.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("srv_worker_shared");
    let src = "import { freeze } from \"std/shared\"\n\
               import { gather } from \"std/task\"\n\
               let table = freeze({ \"a\": 1, \"b\": 2, \"c\": 3 })\n\
               worker fn lookup(t, key) { return t[key] }\n\
               async fn main() {\n\
                 let r = await gather([lookup(table, \"a\"), lookup(table, \"b\"), lookup(table, \"c\")])\n\
                 print(r)\n\
               }\n\
               await main()\n";
    fs::write(d.join("w.as"), src).unwrap();
    // VM (default engine)
    let vm = std::process::Command::new(bin)
        .arg("run")
        .arg(d.join("w.as"))
        .output()
        .unwrap();
    assert!(
        vm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm.stdout), "[1, 2, 3]\n");
    // Tree-walker oracle — byte-identical.
    let tw = std::process::Command::new(bin)
        .arg("run")
        .arg("--tree-walker")
        .arg(d.join("w.as"))
        .output()
        .unwrap();
    assert!(
        tw.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tw.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&tw.stdout),
        String::from_utf8_lossy(&vm.stdout),
        "tree-walker must match the VM for a frozen value across the airlock"
    );
}
