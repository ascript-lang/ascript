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

#[test]
fn forward_reference_to_interface_is_not_undefined() {
    // IFACE: an interface name late-binds via its def_env (lazy `extends`/conformance
    // resolution), so a reference BEFORE the textual `interface` declaration — including
    // from inside an earlier fn body — must NOT trip `undefined-variable`, exactly like a
    // forward-referenced class/enum/fn. (Holistic-review hardening: the exemption is now
    // explicit on BindingKind::Interface, not implicit via is_global.)
    let p = write_tmp(
        "fwd_iface.as",
        "fn make() {\n  let f = F()\n  return f instanceof Greeter\n}\ninterface Greeter { fn hello() }\nclass F { fn hello() { return \"hi\" } }\nprint(make())\n",
    );
    let out = Command::new(bin())
        .arg("check")
        .arg("--deny")
        .arg("undefined-variable")
        .arg(&p)
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "a forward-referenced interface must not be undefined-variable; got: {combined}"
    );
    assert!(
        !combined.contains("undefined"),
        "no undefined diagnostic expected; got: {combined}"
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

    // TYPE Task 2 — the annotated-slot blocking default is OPT-OUT via ascript.toml.
    // An annotated `type-mismatch` defaults to a BLOCKING `Severity::Error` (Task 1).
    // A project `[lint] warn = ["type-mismatch"]` DOWNGRADES it back to a warning (the
    // explicit opt-out); with NO override the default stays blocking. This composes
    // entirely through `config.effective(code, default)`: the emit severity is the
    // `default` argument, so a `warn` override returns `Some(Warning)` and no override
    // passes the `Error` through — NO code change beyond Task 1 was needed here.

    #[test]
    fn type_default_blocks_annotated_mismatch() {
        // No ascript.toml → the annotated `type-mismatch` stays a blocking Error and
        // the run exits non-zero (the soundness default).
        let dir = project("type_default_blocks");
        let f = write(&dir, "a.as", "let x: number = \"s\"\nprint(x)\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            !out.status.success(),
            "annotated type-mismatch must block by default (exit non-zero); out: {}",
            combined(&out)
        );
    }

    #[test]
    fn toml_warn_downgrades_blocking_type_mismatch() {
        // `[lint] warn = ["type-mismatch"]` downgrades the blocking annotated Error
        // back to a Warning → the run exits 0 (the explicit opt-out).
        let dir = project("type_warn_downgrade");
        let f = write(&dir, "a.as", "let x: number = \"s\"\nprint(x)\n");
        write(&dir, "ascript.toml", "[lint]\nwarn = [\"type-mismatch\"]\n");
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        assert!(
            out.status.success(),
            "[lint] warn = [type-mismatch] must downgrade the blocking Error to a Warning (exit 0); out: {}",
            combined(&out)
        );
        // The diagnostic is still reported (as a warning), just non-blocking.
        let json = Command::new(bin())
            .arg("check")
            .arg("--json")
            .arg(&f)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&json.stdout);
        assert!(
            s.contains("\"code\":\"type-mismatch\"") && s.contains("\"severity\":\"warning\""),
            "downgraded type-mismatch must still report as a warning; out: {s}"
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
fn checker_flags_actor_spawn_inside_workflow() {
    // Workers Spec B Task 12: a cross-isolate actor spawn inside a `workflow.run` body
    // must emit a `workflow-determinism` warning (drive it through an activity instead).
    let src = r#"
import { run } from "std/workflow"
worker class Counter { n: number = 0  fn inc(): number { self.n = self.n + 1; return self.n } }
await run((ctx, input) => {
  let c = Counter.spawn()
  return c
}, 0, { log: "wf.log" })
"#;
    let p = write_tmp("workflow_actor_spawn.as", src);
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("workflow-determinism"),
        "expected a workflow-determinism warning for an actor spawn in a workflow; output: {combined}"
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

// ---------------------------------------------------------------------------
// ADT — exhaustiveness, variant narrowing, binding-shadow, construction synth.
// (Library-level checker tests via `ascript::check::analyze`.)
// ---------------------------------------------------------------------------
mod adt_exhaustiveness {
    use ascript::check::{analyze, Severity};

    fn diags(src: &str) -> Vec<ascript::check::AsDiagnostic> {
        analyze(src).diagnostics
    }
    fn find<'a>(
        ds: &'a [ascript::check::AsDiagnostic],
        code: &str,
    ) -> Option<&'a ascript::check::AsDiagnostic> {
        ds.iter().find(|d| d.code == code)
    }
    fn has(src: &str, code: &str) -> bool {
        diags(src).iter().any(|d| d.code == code)
    }

    const SHAPE: &str =
        "enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }\n";

    #[test]
    fn non_exhaustive_missing_variant_is_error_naming_it() {
        // A match missing `Point` (and `Pair`) with NO catch-all.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Rect(w, h) => w,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        let ds = diags(&src);
        let d = find(&ds, "non-exhaustive-match")
            .unwrap_or_else(|| panic!("expected non-exhaustive-match, got {ds:?}"));
        assert_eq!(d.severity, Severity::Error, "must default to Error");
        assert!(d.message.contains("Shape"), "msg: {}", d.message);
        assert!(d.message.contains("Pair"), "must name missing Pair: {}", d.message);
        assert!(d.message.contains("Point"), "must name missing Point: {}", d.message);
    }

    #[test]
    fn exhaustive_all_variants_is_clean() {
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Pair(a, b) => float(a),\n    Shape.Point => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
    }

    #[test]
    fn wildcard_catch_all_is_exhaustive() {
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    _ => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
    }

    #[test]
    fn bare_binding_catch_all_is_exhaustive() {
        // A bare ident that is NOT a variant binds (Option-C) → catch-all.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    other => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
        // `other` is not a variant name → no shadow warning.
        assert!(!has(&src, "enum-variant-binding-shadow"), "{:?}", diags(&src));
    }

    #[test]
    fn guarded_only_arm_does_not_cover() {
        // `Point` is covered only by a GUARDED arm → still non-exhaustive.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Pair(a, b) => float(a),\n    Shape.Point if true => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        let ds = diags(&src);
        let d = find(&ds, "non-exhaustive-match")
            .unwrap_or_else(|| panic!("guarded-only must not cover; got {ds:?}"));
        assert!(d.message.contains("Point"), "msg: {}", d.message);
    }

    #[test]
    fn guarded_plus_unguarded_covers() {
        // A guarded arm AND an unguarded arm for the same variant → covered.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Pair(a, b) => float(a),\n    Shape.Point if false => 9.0,\n    Shape.Point => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
    }

    #[test]
    fn unknown_subject_is_silent() {
        // The subject `v` has no enum type (untyped param) → gradual silent.
        let src = format!(
            "{SHAPE}fn f(v): float {{\n  return match v {{\n    Circle(r) => r,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
    }

    #[test]
    fn binding_shadow_fires_on_bare_known_variant() {
        // A bare `Point` (a variant of Shape) would BIND → shadow warning + catch-all.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Point => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        let ds = diags(&src);
        let d = find(&ds, "enum-variant-binding-shadow")
            .unwrap_or_else(|| panic!("expected binding-shadow, got {ds:?}"));
        assert_eq!(d.severity, Severity::Warning);
        assert!(d.message.contains("Shape.Point"), "suggest qualified: {}", d.message);
        // The bare bind is a catch-all, so the match is NOT flagged non-exhaustive.
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", ds);
    }

    #[test]
    fn qualified_unit_no_shadow_and_covers() {
        // `Shape.Point` qualified → no shadow warning, counts as covering Point.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Pair(a, b) => float(a),\n    Shape.Point => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "enum-variant-binding-shadow"), "{:?}", diags(&src));
        assert!(!has(&src, "non-exhaustive-match"), "{:?}", diags(&src));
    }

    #[test]
    fn value_equality_arm_does_not_fully_cover() {
        // `Circle(0.0)` is a value-equality sub-pattern → does NOT fully cover Circle.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(0.0) => 1.0,\n    Rect(w, h) => w,\n    Pair(a, b) => float(a),\n    Shape.Point => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        let ds = diags(&src);
        let d = find(&ds, "non-exhaustive-match")
            .unwrap_or_else(|| panic!("value-eq must not cover Circle; got {ds:?}"));
        assert!(d.message.contains("Circle"), "msg: {}", d.message);
    }

    #[test]
    fn unknown_variant_in_pattern_and_ctor() {
        // Constructor call + qualified pattern with a non-existent variant.
        let ctor = format!("{SHAPE}let x = Shape.Nope(1)\nprint(x)\n");
        assert!(has(&ctor, "unknown-enum-variant"), "{:?}", diags(&ctor));
        let pat = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Shape.Nonexist(r) => r,\n    _ => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(has(&pat, "unknown-enum-variant"), "{:?}", diags(&pat));
    }

    #[test]
    fn variant_narrowing_binds_field_type() {
        // In `Circle(r) => …`, `r` is `float`; passing it to a `string` param is a
        // provable mismatch — proving the payload sub-pattern was typed `float`.
        let src = format!(
            "{SHAPE}fn needsStr(x: string): string {{ return x }}\nfn f(s: Shape): string {{\n  return match s {{\n    Circle(r) => needsStr(r),\n    _ => \"x\",\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(has(&src, "type-mismatch"), "{:?}", diags(&src));
    }

    #[test]
    fn variant_narrowing_correct_use_is_clean() {
        // `Circle(r) => r` where `r: float` and the fn returns `float` — no mismatch.
        let src = format!(
            "{SHAPE}fn f(s: Shape): float {{\n  return match s {{\n    Circle(r) => r,\n    _ => 0.0,\n  }}\n}}\nprint(f(Shape.Point))\n"
        );
        assert!(!has(&src, "type-mismatch"), "{:?}", diags(&src));
        assert!(!has(&src, "type-error"), "{:?}", diags(&src));
    }

    #[test]
    fn construction_wrong_arg_type_is_mismatch() {
        // `Shape.Pair("x", 4)` — first positional field is `int`, a string is provably wrong.
        let src = format!("{SHAPE}let p = Shape.Pair(\"x\", 4)\nprint(p)\n");
        assert!(has(&src, "type-mismatch"), "{:?}", diags(&src));
    }

    #[test]
    fn construction_correct_is_clean() {
        let src = format!(
            "{SHAPE}let a = Shape.Pair(3, 4)\nlet b = Shape.Circle(2.0)\nprint(a)\nprint(b)\n"
        );
        assert!(!has(&src, "type-mismatch"), "{:?}", diags(&src));
    }

    // The CST-sibling-gather regression guard: oop.as and all_features.as both rely on
    // a trailing `_` arm; if arm-gathering is wrong, they flood with non-exhaustive.
    #[test]
    fn oop_example_has_zero_exhaustiveness_diagnostics() {
        let src = std::fs::read_to_string("examples/oop.as").unwrap();
        for code in ["non-exhaustive-match", "enum-variant-binding-shadow"] {
            assert!(
                !has(&src, code),
                "examples/oop.as emitted {code}: {:?}",
                diags(&src)
            );
        }
    }

    #[test]
    fn all_features_example_has_zero_exhaustiveness_diagnostics() {
        let src = std::fs::read_to_string("examples/all_features.as").unwrap();
        for code in ["non-exhaustive-match", "enum-variant-binding-shadow"] {
            assert!(
                !has(&src, code),
                "examples/all_features.as emitted {code}: {:?}",
                diags(&src)
            );
        }
    }

    #[test]
    fn enums_adt_example_clean() {
        let src = std::fs::read_to_string("examples/enums_adt.as").unwrap();
        for code in ["non-exhaustive-match", "enum-variant-binding-shadow", "type-mismatch", "type-error"] {
            assert!(!has(&src, code), "examples/enums_adt.as emitted {code}: {:?}", diags(&src));
        }
    }
}

// ---------------------------------------------------------------------------
// TYPE Task 1 — the soundness blocking-severity chokepoint.
//
// A `type-mismatch`/`type-error` against a *syntactically-annotated* slot is a
// BLOCKING `Severity::Error` (it fails the gate); an inferred/uncertain misuse
// (and `possibly-nil`) stays an advisory `Severity::Warning`. There are EXACTLY
// FOUR annotated sites: `let x: T = v`, `fn f(): T { return v }`, an annotated
// param at a call site, and a typed class-field default.
// ---------------------------------------------------------------------------
mod sound_blocking_severity {
    use ascript::check::{analyze, Severity};

    fn diags(src: &str) -> Vec<ascript::check::AsDiagnostic> {
        analyze(src).diagnostics
    }
    fn find<'a>(
        ds: &'a [ascript::check::AsDiagnostic],
        code: &str,
    ) -> Option<&'a ascript::check::AsDiagnostic> {
        ds.iter().find(|d| d.code == code)
    }

    #[test]
    fn annotated_let_mismatch_is_error() {
        // `let x: number = "s"` — the destination slot is annotated → BLOCKING.
        let ds = diags("let x: number = \"s\"\n");
        let d = find(&ds, "type-mismatch")
            .unwrap_or_else(|| panic!("expected type-mismatch, got {ds:?}"));
        assert_eq!(
            d.severity,
            Severity::Error,
            "annotated `let` mismatch must block (Error): {ds:?}"
        );
    }

    #[test]
    fn annotated_param_mismatch_is_error() {
        // `fn f(p: string) {} f(1)` — the call passes `int` to an annotated `string`
        // param → BLOCKING on arg 1.
        let ds = diags("fn f(p: string) {}\nf(1)\n");
        let d = find(&ds, "type-mismatch")
            .unwrap_or_else(|| panic!("expected type-mismatch, got {ds:?}"));
        assert_eq!(
            d.severity,
            Severity::Error,
            "annotated param mismatch must block (Error): {ds:?}"
        );
    }

    #[test]
    fn annotated_return_mismatch_is_error() {
        // `fn f(): number { return "x" }` — the declared return is annotated → BLOCKING.
        let ds = diags("fn f(): number { return \"x\" }\n");
        let d = find(&ds, "type-mismatch")
            .unwrap_or_else(|| panic!("expected type-mismatch, got {ds:?}"));
        assert_eq!(
            d.severity,
            Severity::Error,
            "annotated return mismatch must block (Error): {ds:?}"
        );
    }

    #[test]
    fn typed_field_default_mismatch_is_error() {
        // `class C { n: number = "x" }` — the field's declared type is annotated → BLOCKING.
        let ds = diags("class C { n: number = \"x\" }\n");
        let d = find(&ds, "type-mismatch")
            .unwrap_or_else(|| panic!("expected type-mismatch, got {ds:?}"));
        assert_eq!(
            d.severity,
            Severity::Error,
            "typed field default mismatch must block (Error): {ds:?}"
        );
    }

    #[test]
    fn inferred_misuse_stays_advisory_warning() {
        // No annotation on `x`; the later arithmetic misuse is over an *inferred*
        // string slot → stays advisory `Warning` (the programmer never promised a
        // type). This is the paired counterpart of `annotated_let_mismatch_is_error`.
        let ds = diags("let x = \"s\"\nlet y = x - 1\n");
        let d = find(&ds, "type-error")
            .unwrap_or_else(|| panic!("expected type-error on inferred misuse, got {ds:?}"));
        assert_eq!(
            d.severity,
            Severity::Warning,
            "inferred-slot misuse must stay advisory (Warning): {ds:?}"
        );
    }

    #[test]
    fn paired_annotated_error_vs_inferred_warning() {
        // The SAME provable `No` is Error over an annotated slot and Warning over an
        // inferred slot — asserted side by side.
        let annotated = diags("let x: number = \"s\"\n");
        let ad = find(&annotated, "type-mismatch")
            .unwrap_or_else(|| panic!("annotated: expected type-mismatch, got {annotated:?}"));
        assert_eq!(ad.severity, Severity::Error, "annotated → Error: {annotated:?}");

        let inferred = diags("let x = \"s\"\nlet y = x - 1\n");
        let id = find(&inferred, "type-error")
            .unwrap_or_else(|| panic!("inferred: expected type-error, got {inferred:?}"));
        assert_eq!(id.severity, Severity::Warning, "inferred → Warning: {inferred:?}");
    }

    #[test]
    fn possibly_nil_stays_advisory_warning() {
        // `possibly-nil` flags a *latent* runtime panic, not an annotated-slot type
        // clash — it stays advisory even though `x` is annotated `T?`.
        let ds = diags("fn f(x: number?): number { return x + 1 }\n");
        if let Some(d) = find(&ds, "possibly-nil") {
            assert_eq!(
                d.severity,
                Severity::Warning,
                "possibly-nil must stay advisory (Warning): {ds:?}"
            );
        }
    }
}
