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
    fs::write(d.join("util.as"), "export const PI = 3\nexport fn double(x) { return x * 2 }\nfn secret() { return 99 }").unwrap();
    fs::write(d.join("main.as"),
        "import { PI, double } from \"./util\"\nimport * as u from \"./util\"\nprint(PI)\nprint(double(21))\nprint(u.double(5))").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n42\n10\n");
}

#[test]
fn importing_non_export_errors() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("noexport");
    fs::write(d.join("lib.as"), "export const A = 1\nconst B = 2").unwrap();
    fs::write(d.join("app.as"), "import { B } from \"./lib\"\nprint(B)").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("app.as")).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no export 'B'"));
}

#[test]
fn module_body_runs_once() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("once");
    // side.as prints when loaded; importing it from two places must print once.
    fs::write(d.join("side.as"), "print(\"loaded\")\nexport const V = 1").unwrap();
    fs::write(d.join("a.as"), "import { V } from \"./side\"\nexport const A = V").unwrap();
    fs::write(d.join("main.as"),
        "import { V } from \"./side\"\nimport { A } from \"./a\"\nprint(V)\nprint(A)").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // "loaded" appears exactly once despite two import paths to side.as.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "loaded\n1\n1\n");
}

#[test]
fn circular_import_resolves_partial() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("circular");
    // a imports b; b imports a. a defines X before importing b, so b can use it.
    fs::write(d.join("a.as"),
        "export const X = 10\nimport { Y } from \"./b\"\nexport fn sum() { return X + Y }").unwrap();
    fs::write(d.join("b.as"),
        "import { X } from \"./a\"\nexport const Y = X + 5").unwrap();
    fs::write(d.join("main.as"),
        "import { sum } from \"./a\"\nprint(sum())").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // a.X=10 is defined before a imports b; b reads X=10, sets Y=15; a.sum()=25.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "25\n");
}
