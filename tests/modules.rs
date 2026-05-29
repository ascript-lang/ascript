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
