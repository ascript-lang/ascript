//! End-to-end test: build the binary and run a real `.as` file.

use std::process::Command;

#[test]
fn runs_a_program_file_and_prints_result() {
    let file = std::env::temp_dir().join("ascript_skeleton_hello.as");
    std::fs::write(&file, "print(1 + 2 * 3)\n").unwrap();

    // Cargo sets CARGO_BIN_EXE_<name> for integration tests.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn runs_factorial_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/factorial.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // 1*2*3*4*5 = 120, which is > 100, then the value itself.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "big\n120\n");
}

#[test]
fn runs_functions_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/functions.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // fib(10) = 55; triple(7) = 21; count of odd fib(0..10) = fib values
    // [0,1,1,2,3,5,8,13,21,34] -> odd ones: 1,1,3,5,13,21 -> 6
    assert_eq!(String::from_utf8_lossy(&output.stdout), "55\n21\n6\n");
}

#[test]
fn runs_data_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/data.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // ages 36+41+45 = 122; average 122/3 ≈ 40.66...; oldest is Grace
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("sum of ages: 122"));
    assert!(out.contains("Grace"));
}

#[test]
fn reports_usage_without_args() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage"));
}
