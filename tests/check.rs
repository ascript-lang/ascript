//! Integration tests for `ascript check` (spawns the built binary).
use std::process::Command;
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}
fn write_tmp(name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("ascript_check_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}
#[test]
fn clean_file_exits_zero() {
    let p = write_tmp("ok.as", "let x = 1\nprint(x)\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn syntax_error_exits_nonzero_and_reports() {
    let p = write_tmp("bad.as", "let = 1\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(!out.status.success(), "should fail on syntax error");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("syntax-error"), "should name the rule: {combined}");
}
#[test]
fn json_output_is_a_json_array() {
    let p = write_tmp("bad2.as", "let = 1\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--json")
        .arg(&p)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim_start().starts_with('['), "json output: {stdout}");
    assert!(stdout.contains("\"code\":\"syntax-error\""));
}

// The checker must NOT false-positive on idiomatic code: every example program
// should produce zero diagnostics (or only ones a maintainer has suppressed in
// the source). Any new false positive fails this and must be fixed (rule made
// more conservative) or suppressed in the example with a reason.
mod corpus {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn corpus() -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("as") {
                    out.push(p);
                }
            }
        }
        let mut v = Vec::new();
        walk(Path::new("examples"), &mut v);
        v.sort();
        v
    }

    #[test]
    fn checker_is_clean_on_the_corpus() {
        use ascript::check::Severity;
        let mut offenders = Vec::new();
        for path in corpus() {
            let src = fs::read_to_string(&path).unwrap();
            // The gate is about no false ERRORS/WARNINGS on idiomatic code. Advisory
            // Hint/Info (e.g. `shadowing`) may legitimately appear and are allowed.
            let actionable: Vec<_> = ascript::check::analyze(&src)
                .diagnostics
                .into_iter()
                .filter(|d| matches!(d.severity, Severity::Error | Severity::Warning))
                .map(|d| format!("{}@{}", d.code, d.range.start))
                .collect();
            if !actionable.is_empty() {
                offenders.push(format!("{}: {:?}", path.display(), actionable));
            }
        }
        assert!(
            offenders.is_empty(),
            "checker false-positived (error/warning) on idiomatic examples (make the rule conservative or suppress with a reason):\n{}",
            offenders.join("\n")
        );
    }
}
