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
fn nil_return_type_is_not_a_syntax_error() {
    // `nil` is a valid type (Type::Nil); the CST type parser must accept it so
    // the checker does not emit a false `syntax-error`. Regression for the
    // missing `NilKw` arm in `type_primary`.
    let p = write_tmp("nilret.as", "fn f(): nil { print(\"hi\") }\nf()\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`: nil` must not produce a syntax error; output: {combined}"
    );
    assert!(
        !combined.contains("syntax-error"),
        "no syntax-error expected for `: nil`; output: {combined}"
    );
}
#[test]
fn fn_type_is_not_a_syntax_error() {
    // `fn` is a valid type (Type::Fn); the CST type parser must accept it so the
    // checker does not emit a false `syntax-error`. Regression for the missing
    // `FnKw` arm in `type_primary`.
    let p = write_tmp(
        "fntype.as",
        "fn apply(g: fn, x) { return g(x) }\napply((n) => n, 1)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`: fn` must not produce a syntax error; output: {combined}"
    );
    assert!(
        !combined.contains("syntax-error"),
        "no syntax-error expected for `: fn`; output: {combined}"
    );
}

#[test]
fn match_guard_ending_in_ident_is_not_a_syntax_error() {
    // A match guard ending in a bare identifier right before `=>` (`n if n == lim
    // => ...`) must not be mis-parsed as an arrow, which would leave the arm body
    // dangling and surface a `syntax-error`. Regression for the CST front-end's
    // greedy bare-arrow parsing inside guards.
    let p = write_tmp(
        "guardident.as",
        "fn d(v, lim) {\n  return match v {\n    n if n == lim => \"eq\",\n    other => \"o\",\n  }\n}\nprint(d(2, 2))\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "guard ending in ident must not produce a syntax error; output: {combined}"
    );
    assert!(
        !combined.contains("syntax-error"),
        "no syntax-error expected for guard-ending-in-ident; output: {combined}"
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
    assert!(
        combined.contains("syntax-error"),
        "should name the rule: {combined}"
    );
}
#[test]
fn warning_only_exits_zero_without_deny_but_one_with() {
    // `let x = 1` (unused-binding, a Warning) — no syntax error.
    let p = write_tmp("warnonly.as", "let x = 1\n");
    let plain = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(
        plain.status.success(),
        "warning-only must exit 0 without --deny-warnings; stderr: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let denied = Command::new(bin())
        .arg("check")
        .arg("--deny-warnings")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        !denied.status.success(),
        "warning-only must exit non-zero WITH --deny-warnings"
    );
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
    assert!(
        stdout.trim_start().starts_with('['),
        "json output: {stdout}"
    );
    assert!(stdout.contains("\"code\":\"syntax-error\""));
}

// --- CFG-T2: --deny / --warn / --allow flags ------------------------------

#[test]
fn deny_promotes_warning_to_error_exit() {
    // `let x = 1` is unused-binding (Warning by default → exit 0).
    let p = write_tmp("deny_warn.as", "let x = 1\n");
    let plain = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(
        plain.status.success(),
        "warning-only must exit 0 by default"
    );
    // --deny unused-binding promotes it to Error → non-zero exit.
    let denied = Command::new(bin())
        .arg("check")
        .arg("--deny")
        .arg("unused-binding")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        !denied.status.success(),
        "--deny unused-binding must promote to Error and exit non-zero"
    );
}

#[test]
fn allow_silences_rule_and_removes_from_output() {
    let p = write_tmp("allow_rule.as", "let x = 1\n");
    // Sanity: without --allow the rule appears in output.
    let plain = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let plain_out = format!(
        "{}{}",
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        plain_out.contains("unused-binding"),
        "sanity: unused-binding should appear by default; got {plain_out}"
    );
    // --allow drops the diagnostic entirely and exits 0.
    let allowed = Command::new(bin())
        .arg("check")
        .arg("--allow")
        .arg("unused-binding")
        .arg(&p)
        .output()
        .unwrap();
    assert!(allowed.status.success(), "--allow must exit 0");
    let allowed_out = format!(
        "{}{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(
        !allowed_out.contains("unused-binding"),
        "--allow must remove the diagnostic from output; got {allowed_out}"
    );
}

#[test]
fn allow_beats_deny_warnings() {
    // An --allow'd rule produces NO diagnostic, so it cannot trip --deny-warnings.
    let p = write_tmp("allow_vs_denywarn.as", "let x = 1\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--deny-warnings")
        .arg("--allow")
        .arg("unused-binding")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "an --allow'd rule must not trip --deny-warnings; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn warn_demotes_a_denied_rule_to_warning() {
    // `--deny unused-binding --warn unused-binding`: last-write-wins in LintConfig,
    // so the rule ends up Warning (exit 0 without --deny-warnings) — proving --warn
    // maps to Warning. The label in human output is "warning".
    let p = write_tmp("warn_demote.as", "let x = 1\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--warn")
        .arg("unused-binding")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "--warn keeps it a Warning → exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.to_lowercase().contains("warning"),
        "human output should label it a warning; got {combined}"
    );
}

#[test]
fn unknown_rule_code_is_a_usage_error() {
    let p = write_tmp("unknown_rule.as", "let x = 1\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--deny")
        .arg("nonsense-rule")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an unknown rule code must be a non-zero usage error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown lint rule") && stderr.contains("nonsense-rule"),
        "must name the unknown rule clearly; stderr: {stderr}"
    );
}

#[test]
fn allow_syntax_error_is_accepted_but_noop() {
    // `--allow syntax-error` is a KNOWN code (no usage error) but a NO-OP:
    // syntax-error is immune, so the file still fails.
    let p = write_tmp("allow_syntax.as", "let = 1\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--allow")
        .arg("syntax-error")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--allow syntax-error must NOT suppress the syntax error (immune)"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("syntax-error"),
        "syntax-error must still be reported; got {combined}"
    );
}

#[test]
fn repeatable_deny_applies_to_all() {
    // Two --deny flags both apply: a file with an unused binding AND an undefined
    // variable, denying both → non-zero exit, both promoted.
    let p = write_tmp("multi_deny.as", "let x = 1\nprint(y)\n");
    let out = Command::new(bin())
        .arg("check")
        .arg("--deny")
        .arg("unused-binding")
        .arg("--deny")
        .arg("undefined-variable")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "both denied rules must apply and fail the check"
    );
}

// --- CFG-T3: ascript.toml [lint] config -----------------------------------
//
// Discovery walks UP from each checked file's parent directory looking for
// `ascript.toml`. Tests therefore put the `.as` file and `ascript.toml` in the
// SAME unique temp dir and pass the file path. Precedence: inline-ignore >
// CLI flag > toml > rule default.
mod toml_config {
    use std::process::Command;
    fn bin() -> &'static str {
        env!("CARGO_BIN_EXE_ascript")
    }
    // A fresh, unique project dir per test so toml discovery is deterministic and
    // tests don't see each other's `ascript.toml`.
    fn project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ascript_toml_cfg_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn write(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }
    fn combined(out: &std::process::Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    #[test]
    fn toml_deny_promotes_warning_to_error_exit() {
        let dir = project("deny");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(
            &dir,
            "ascript.toml",
            "[lint]\ndeny = [\"unused-binding\"]\n",
        );
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "ascript.toml deny must promote to Error and exit non-zero; out: {}",
            combined(&out)
        );
    }

    #[test]
    fn cli_allow_overrides_toml_deny() {
        // toml denies, CLI allows → CLI wins → exit 0 (CLI > toml).
        let dir = project("cli_over_toml");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(
            &dir,
            "ascript.toml",
            "[lint]\ndeny = [\"unused-binding\"]\n",
        );
        let out = Command::new(bin())
            .arg("check")
            .arg("--allow")
            .arg("unused-binding")
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "CLI --allow must override toml deny (CLI > toml); out: {}",
            combined(&out)
        );
    }

    #[test]
    fn toml_allow_suppresses_rule() {
        let dir = project("allow");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(
            &dir,
            "ascript.toml",
            "[lint]\nallow = [\"unused-binding\"]\n",
        );
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(out.status.success(), "toml allow must exit 0");
        assert!(
            !combined(&out).contains("unused-binding"),
            "toml allow must drop the diagnostic; out: {}",
            combined(&out)
        );
    }

    #[test]
    fn toml_deny_warnings_fails_warning_only_file() {
        let dir = project("denywarn");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(&dir, "ascript.toml", "[lint]\ndeny_warnings = true\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "toml deny_warnings=true must fail a warning-only file; out: {}",
            combined(&out)
        );
    }

    #[test]
    fn malformed_toml_is_a_clear_error() {
        let dir = project("malformed");
        let f = write(&dir, "a.as", "let x = 1\n");
        // `deny` must be an array; a string is a type error in the [lint] table.
        write(&dir, "ascript.toml", "[lint]\ndeny = \"notalist\"\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(!out.status.success(), "malformed toml must exit non-zero");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("ascript.toml"),
            "error must name ascript.toml; stderr: {err}"
        );
    }

    #[test]
    fn broken_toml_syntax_is_a_clear_error() {
        let dir = project("broken");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(&dir, "ascript.toml", "[lint\ndeny = [\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "broken toml syntax must exit non-zero"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("ascript.toml"),
            "error must name ascript.toml; stderr: {err}"
        );
    }

    #[test]
    fn unknown_rule_in_toml_is_a_clear_error() {
        let dir = project("unknown");
        let f = write(&dir, "a.as", "let x = 1\n");
        write(&dir, "ascript.toml", "[lint]\ndeny = [\"bogus\"]\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "unknown rule in toml must exit non-zero"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("ascript.toml") && err.contains("bogus"),
            "error must name ascript.toml and the unknown rule; stderr: {err}"
        );
    }

    #[test]
    fn no_toml_behaves_as_default() {
        // No ascript.toml anywhere in the dir → warning-only file still exits 0.
        let dir = project("none");
        let f = write(&dir, "a.as", "let x = 1\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            out.status.success(),
            "no ascript.toml must behave as default (warning → exit 0); out: {}",
            combined(&out)
        );
    }

    #[test]
    fn toml_discovered_by_walking_up() {
        // ascript.toml in the project root, the .as file in a nested subdir.
        let dir = project("walkup");
        let sub = dir.join("src").join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        let f = write(&sub, "a.as", "let x = 1\n");
        write(
            &dir,
            "ascript.toml",
            "[lint]\ndeny = [\"unused-binding\"]\n",
        );
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "ascript.toml in an ancestor dir must be discovered; out: {}",
            combined(&out)
        );
    }

    #[test]
    fn inline_ignore_beats_toml_deny() {
        // inline ascript-ignore always wins over a toml deny.
        let dir = project("inline");
        let f = write(
            &dir,
            "a.as",
            "let x = 1 // ascript-ignore[unused-binding]\n",
        );
        write(
            &dir,
            "ascript.toml",
            "[lint]\ndeny = [\"unused-binding\"]\n",
        );
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            out.status.success(),
            "inline ascript-ignore must beat a toml deny; out: {}",
            combined(&out)
        );
    }
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

    // SP10 corpus zero-new-diagnostic differential (the SP10 analogue of the VM
    // three-way differential). The whole corpus must produce ZERO
    // `type-mismatch`/`type-error`/`possibly-nil` diagnostics — any new one is a bug
    // in `assignable`/`synth`/narrowing (relax the GUARD, never this assertion).
    //
    // The test runs in WHICHEVER feature config `cargo test` is invoked with, so it
    // gates BOTH `cargo test` (default) and `cargo test --no-default-features`.
    #[test]
    fn type_checker_emits_no_type_diagnostics_on_the_corpus() {
        let type_codes = ["type-mismatch", "type-error", "possibly-nil"];
        let mut offenders = Vec::new();
        for path in corpus() {
            let src = fs::read_to_string(&path).unwrap();
            let hits: Vec<_> = ascript::check::analyze(&src)
                .diagnostics
                .into_iter()
                .filter(|d| type_codes.contains(&d.code.as_str()))
                .map(|d| format!("{}@{}: {}", d.code, d.range.start, d.message))
                .collect();
            if !hits.is_empty() {
                offenders.push(format!("{}:\n  {}", path.display(), hits.join("\n  ")));
            }
        }
        assert!(
            offenders.is_empty(),
            "SP10 type checker emitted type-* diagnostics on the untyped corpus (fix the root cause in assignable/synth/narrowing — default to Unknown/silent — NEVER relax this gate):\n{}",
            offenders.join("\n")
        );
    }
}

// ---------------------------------------------------------------------------
// Workers Spec A: invalid modifier combos must be flagged by `ascript check`.
// ---------------------------------------------------------------------------

#[test]
fn checker_flags_worker_async_fn() {
    // `worker async fn` must produce an Error-severity `worker-capture` diagnostic.
    let p = write_tmp("worker_async.as", "worker async fn g() { return 2 }\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "check must exit non-zero for worker async fn; output: {combined}"
    );
    assert!(
        combined.contains("worker functions cannot be async"),
        "expected worker-modifiers message; output: {combined}"
    );
}

#[test]
fn checker_accepts_worker_generator_fn() {
    // Spec B Task 6: `worker fn*` is a VALID streaming generator — the checker must
    // NOT flag it as an invalid modifier combination (no `worker-capture` error for
    // the modifiers themselves; the body still gets the normal capture checks).
    let p = write_tmp(
        "worker_gen_ok.as",
        "worker fn* h(n) { for i in 1..=n { yield i } }\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("worker functions cannot be"),
        "worker fn* must not be flagged as an invalid modifier combo; output: {combined}"
    );
}
