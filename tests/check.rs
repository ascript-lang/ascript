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

// --- TYPE Task 12: generic inference end-to-end (CLI gate) ----------------

#[test]
fn generic_mismatch_fails_the_gate() {
    // A generic fn call whose inferred return is provably wrong for an ANNOTATED slot
    // is a BLOCKING error → non-zero exit.
    let p = write_tmp(
        "gen_bad.as",
        "fn id<T>(x: T): T { return x }\nlet s: string = id(5)\nprint(s)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a provable generic mismatch must fail the gate; got: {combined}"
    );
    assert!(
        combined.contains("type-mismatch"),
        "expected a type-mismatch; got: {combined}"
    );
}

#[test]
fn clean_generic_code_passes_the_gate() {
    // Inference + an explicit type arg + a method-return instantiation, all clean.
    let p = write_tmp(
        "gen_ok.as",
        "class Box<T> { value: T\n fn init(v: T) { self.value = v }\n fn get(): T { return self.value } }\nlet b = Box<int>(5)\nlet n: int = b.get()\nprint(n)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "clean generic code must pass the gate; got: {combined}"
    );
}

#[test]
fn empty_array_generic_call_is_silent() {
    // The pinned invariant: map([], f) leaves the element type gradual → no diagnostic.
    let p = write_tmp(
        "gen_empty.as",
        "fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> {\n  let out: array<B> = []\n  return out\n}\nlet r = map([], (x) => x)\nprint(len(r))\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && !combined.contains("type-"),
        "empty-array generic must be gradual-silent; got: {combined}"
    );
}

#[test]
fn same_typed_params_mixed_numerics_is_silent() {
    // Regression (TYPE Unit-C review B1): a generic with two same-typed params called
    // with mixed numeric subtypes (`max(1, 2.0)`) must be SILENT — the type var widens
    // to `number` (the join), not stale-bound to `int` (which manufactured a false
    // blocking `type-mismatch` on code that runs fine, since T is erased).
    let p = write_tmp(
        "gen_mixed_num.as",
        "fn max<T>(a: T, b: T): T { return a }\nlet r = max(1, 2.0)\nlet s = max(2.0, 1)\nprint(r)\nprint(s)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && !combined.contains("type-"),
        "same-T mixed-numeric generic call must be gradual-silent; got: {combined}"
    );
}

#[test]
fn generic_subclass_field_construction_is_gradual_silent() {
    // Regression (TYPE Unit-D review): a no-`init` GENERIC subclass with inherited
    // fields auto-derives its positional constructor over the base-FIRST merged field
    // schema, so arg 0 binds to the first BASE field at runtime. The checker's own-only
    // field order would misalign arg 0 to the first OWN field and manufacture a FALSE
    // blocking `type-mismatch`. `Sub(1, "x")` below RUNS fine and must check clean.
    let p = write_tmp(
        "gen_subclass.as",
        "class Base { a: number }\nclass Sub<T> extends Base { b: string }\nlet s = Sub(1, \"x\")\nprint(s.a)\nprint(s.b)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && !combined.contains("type-"),
        "a generic subclass construction (base-first merged fields) must be gradual-silent; got: {combined}"
    );
}

#[test]
fn baseless_generic_construction_still_precise() {
    // The subclass gradual-drop must NOT weaken base-less generic inference: an explicit
    // type arg conflicting with the constructor value is still a blocking error.
    let p = write_tmp(
        "gen_baseless.as",
        "class Box<T> { v: T }\nlet b: Box<string> = Box(5)\nprint(b)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("type-mismatch"),
        "a base-less Box<string> = Box(5) conflict must still be caught; got: {combined}"
    );
}

#[test]
fn same_typed_params_incompatible_types_still_caught() {
    // The numeric-join rescue must NOT swallow a genuine conflict: `pair(1, "s")` binds
    // T to two non-numeric-incompatible concretes — still a blocking `type-mismatch`.
    let p = write_tmp(
        "gen_conflict.as",
        "fn pair<T>(a: T, b: T): T { return a }\nlet x = pair(1, \"s\")\nprint(x)\n",
    );
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("type-mismatch"),
        "a genuine same-T conflict (int vs string) must still be caught; got: {combined}"
    );
}

#[test]
fn array_element_diagnostic_is_emitted_exactly_once() {
    // Regression: `synth_array` used to synthesize every element TWICE (a discarded
    // first pass + the folding pass), so any diagnostic on an array element was emitted
    // twice. The element `x + 1` (x: int?) trips `possibly-nil` — it must appear once.
    let p = write_tmp(
        "arr_dup.as",
        "let x: int? = nil\nlet a = [x + 1]\nprint(a)\n",
    );
    let out = Command::new(bin())
        .arg("check")
        .arg("--json")
        .arg(&p)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let count = stdout.matches("\"code\":\"possibly-nil\"").count();
    assert_eq!(
        count, 1,
        "the array-element possibly-nil diagnostic must be emitted exactly once; got {count} in: {stdout}"
    );
}

#[test]
fn member_call_receiver_diagnostic_is_emitted_exactly_once() {
    // Regression (blast-radius of the synth_array dedupe): a `MemberExpr`-callee call
    // used to synthesize the receiver TWICE (once in `synth_variant_construction`, again
    // in `synth_member_call`), duplicating any receiver sub-diagnostic. `(x + 1)` with
    // `x: int?` trips `possibly-nil` — it must appear once.
    let p = write_tmp(
        "memcall_dup.as",
        "let x: int? = nil\nlet r = (x + 1).foo()\nprint(r)\n",
    );
    let out = Command::new(bin())
        .arg("check")
        .arg("--json")
        .arg(&p)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let count = stdout.matches("\"code\":\"possibly-nil\"").count();
    assert_eq!(
        count, 1,
        "the member-call receiver possibly-nil diagnostic must be emitted exactly once; got {count} in: {stdout}"
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
    // The full Gate-5 diagnostic set: the three SP10 advisory codes PLUS the
    // static-exhaustiveness / ADT codes (`non-exhaustive-match`,
    // `enum-variant-binding-shadow`). This mirrors the CI grep
    // (`type-mismatch|type-error|possibly-nil|non-exhaustive|enum-variant`) so the
    // test and the shell gate cannot drift.
    const GATE5_CODES: &[&str] = &[
        "type-mismatch",
        "type-error",
        "possibly-nil",
        "non-exhaustive-match",
        "enum-variant-binding-shadow",
    ];

    #[test]
    fn type_checker_emits_no_type_diagnostics_on_the_corpus() {
        let mut offenders = Vec::new();
        for path in corpus() {
            let src = fs::read_to_string(&path).unwrap();
            let hits: Vec<_> = ascript::check::analyze(&src)
                .diagnostics
                .into_iter()
                .filter(|d| GATE5_CODES.contains(&d.code.as_str()))
                .map(|d| format!("{}@{}: {}", d.code, d.range.start, d.message))
                .collect();
            if !hits.is_empty() {
                offenders.push(format!("{}:\n  {}", path.display(), hits.join("\n  ")));
            }
        }
        assert!(
            offenders.is_empty(),
            "SP10 type checker emitted Gate-5 diagnostics on the corpus (fix the root cause in assignable/synth/unify/narrowing — default to Unknown/silent — NEVER relax this gate):\n{}",
            offenders.join("\n")
        );
    }

    // TYPE Task 14 — the zero-false-positive PROPERTY battery. Untyped, `any`-typed,
    // and partially-typed programs flowing through generics must emit NO blocking
    // diagnostic. The cardinal invariant is "an unsolved/unbounded `Var` → Unknown,
    // never No": these are the exact shapes that would trip if unification or the
    // invariant `assignable` arm ever manufactured a `No`. The B1 mixed-numeric
    // combinator (`max(1, 2.0)` over `<T>(a: T, b: T)`) is included as the standing
    // proof of the numeric-join rescue. Runs in whichever feature config invokes
    // `cargo test`, so it gates BOTH configs.
    #[test]
    fn generics_property_battery_emits_no_blocking_diagnostic() {
        let programs: &[&str] = &[
            // Identity over an untyped value.
            "fn id<T>(x: T): T { return x }\nprint(id(5))\nprint(id(\"s\"))\n",
            // Same-typed params with MIXED numerics (the B1 false-positive shape).
            "fn pick<T>(a: T, b: T): T { return a }\nprint(pick(1, 2.0))\n",
            "fn maxOf<T>(a: T, b: T): T { if (a > b) { return a }\n  return b }\nprint(maxOf(1, 2.0))\n",
            // Empty-array generic call: `A` stays unsolved → array<any> → silent.
            "fn mapAll<A, B>(xs: array<A>, f: fn(A) -> B): array<B> { return [] }\nprint(mapAll([], fn(x) { return x }))\n",
            // `any`-typed argument flowing into a generic param.
            "fn wrap<T>(x: T): array<T> { return [x] }\nfn use2(v: any) { return wrap(v) }\nprint(use2(3))\n",
            // A generic class round-trip with inferred + explicit type args.
            "class Box<T> { value: T\n  fn get(): T { return self.value } }\nlet a = Box(5)\nprint(a.get())\n",
            // A generic enum payload.
            "enum Opt<T> { Some(value: T), None }\nlet o = Opt.Some(3)\nprint(o)\n",
            // Partially-typed: a `number`-annotated arg into a `<T>` slot.
            "fn echo<T>(x: T): T { return x }\nfn caller(n: number) { return echo(n) }\nprint(caller(7))\n",
        ];
        let mut offenders = Vec::new();
        for src in programs {
            let blocking: Vec<_> = ascript::check::analyze(src)
                .diagnostics
                .into_iter()
                .filter(|d| GATE5_CODES.contains(&d.code.as_str()))
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect();
            if !blocking.is_empty() {
                offenders.push(format!("{src:?}\n  {}", blocking.join("\n  ")));
            }
        }
        assert!(
            offenders.is_empty(),
            "generics emitted a blocking diagnostic on a gradual program (the Var→Unknown invariant is broken):\n{}",
            offenders.join("\n\n")
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

// ---------------------------------------------------------------------------
// DX D4 §5.2 — "did you mean" suggestions on unresolved names + std imports.
// ---------------------------------------------------------------------------
mod did_you_mean {
    use ascript::check::analyze;

    fn message(src: &str, code: &str) -> String {
        analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == code)
            .map(|d| d.message)
            .unwrap_or_default()
    }

    #[test]
    fn typo_of_builtin_suggests_it() {
        // `prnt` → builtin `print` (distance 1).
        let m = message("prnt([1])\n", "undefined-variable");
        assert!(m.contains("did you mean `print`?"), "got: {m}");
    }

    #[test]
    fn typo_of_local_binding_suggests_it() {
        let m = message("let length = 5\nprint(lenght)\n", "undefined-variable");
        assert!(m.contains("did you mean `length`?"), "got: {m}");
    }

    #[test]
    fn name_beyond_distance_gets_no_suggestion() {
        // `zzzzzz` is far from every binding/builtin — no nonsense suggestion.
        let m = message("print(zzzzzz)\n", "undefined-variable");
        assert!(m.contains("is not defined"), "got: {m}");
        assert!(!m.contains("did you mean"), "should not suggest: {m}");
    }

    #[test]
    fn typo_std_module_suggests_closest() {
        let m = message(
            "import { abs } from \"std/maths\"\nprint(1)\n",
            "unresolved-import",
        );
        assert!(m.contains("did you mean `std/math`?"), "got: {m}");
    }

    #[test]
    fn far_std_module_gets_no_suggestion() {
        let m = message(
            "import * as x from \"std/doesnotexist\"\nprint(1)\n",
            "unresolved-import",
        );
        assert!(!m.contains("did you mean"), "should not suggest: {m}");
    }

    #[test]
    fn suggestion_carries_a_fix_for_lsp() {
        // The unresolved-name suggestion carries a Fix the LSP turns into a quickfix.
        let fix = analyze("let length = 5\nprint(lenght)\n")
            .diagnostics
            .into_iter()
            .find(|d| d.code == "undefined-variable")
            .and_then(|d| d.fix)
            .expect("a did-you-mean fix");
        assert_eq!(fix.edits.len(), 1);
        assert_eq!(fix.edits[0].replacement, "length");
    }
}

// ---------------------------------------------------------------------------
// DEFER §6.1 — `defer-in-loop` lint (Warning, default-on)
// ---------------------------------------------------------------------------
mod defer_in_loop {
    use super::bin;
    use ascript::check::analyze;
    use std::process::Command;

    fn count(src: &str) -> usize {
        analyze(src)
            .diagnostics
            .iter()
            .filter(|d| d.code == "defer-in-loop")
            .count()
    }

    fn has(src: &str) -> bool {
        count(src) > 0
    }

    // --- fires cases ---

    #[test]
    fn fires_inside_while_loop() {
        let src = "fn f() { while (true) { defer print(1) } }\n";
        assert!(has(src), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_range() {
        let src = "fn f() { for (i in 1..10) { defer print(i) } }\n";
        assert!(has(src), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_of() {
        let src = "fn f(xs) { for (x of xs) { defer print(x) } }\n";
        assert!(has(src), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_await() {
        // for-await uses the same ForStmt CST node.
        let src = "async fn f(g) { for await (x of g) { defer print(x) } }\n";
        assert!(has(src), "{:?}", analyze(src).diagnostics);
    }

    // --- no-fire cases ---

    #[test]
    fn no_fire_outside_any_loop() {
        let src = "fn f() { defer print(1) }\n";
        assert!(!has(src), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn no_fire_nested_fn_inside_loop() {
        // The nested fn body resets the walk; its defer is per-call of the inner fn.
        let src = "fn outer() { while (true) { fn inner() { defer print(1) } inner() } }\n";
        assert!(
            !has(src),
            "nested fn inside loop should NOT fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_nested_arrow_inside_loop() {
        // Arrow body also resets the walk.
        let src = "fn outer() { while (true) { let f = () => { defer print(1) } f() } }\n";
        assert!(
            !has(src),
            "nested arrow inside loop should NOT fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn severity_is_warning() {
        use ascript::check::Severity;
        let src = "fn f() { while (true) { defer print(1) } }\n";
        let diag = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-in-loop")
            .unwrap();
        assert!(
            matches!(diag.severity, Severity::Warning),
            "severity should be Warning, got {:?}",
            diag.severity
        );
    }

    #[test]
    fn suppression_via_toml_allow() {
        // `ascript.toml [lint] allow = ["defer-in-loop"]` must suppress the warning.
        use std::fs;
        let dir = std::env::temp_dir().join("ascript_defer_in_loop_toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("loop.as");
        fs::write(&f, "fn f() { while (true) { defer print(1) } }\n").unwrap();
        fs::write(
            dir.join("ascript.toml"),
            "[lint]\nallow = [\"defer-in-loop\"]\n",
        )
        .unwrap();
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "toml allow must suppress defer-in-loop; out: {combined}"
        );
        assert!(
            !combined.contains("defer-in-loop"),
            "toml allow must drop the diagnostic; out: {combined}"
        );
    }
}

// ---------------------------------------------------------------------------
// DEFER §6.2 — `defer-async-call` lint (Warning, default-on)
// ---------------------------------------------------------------------------
mod defer_async_call {
    use super::bin;
    use ascript::check::analyze;
    use std::process::Command;

    fn count(src: &str) -> usize {
        analyze(src)
            .diagnostics
            .iter()
            .filter(|d| d.code == "defer-async-call")
            .count()
    }

    fn has(src: &str) -> bool {
        count(src) > 0
    }

    // --- fires cases ---

    #[test]
    fn fires_on_bare_defer_to_async_fn() {
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        assert!(has(src), "{:?}", analyze(src).diagnostics);
    }

    // --- no-fire cases ---

    #[test]
    fn no_fire_on_defer_await_async_fn() {
        // `defer await` is the correct form.
        let src = "async fn teardown() { }\nasync fn main() { defer await teardown() }\n";
        assert!(
            !has(src),
            "defer await should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_on_member_callee() {
        // Out-of-scope: member callees.
        let src = "fn main() { let r = {} defer r.teardown() }\n";
        assert!(
            !has(src),
            "member callee should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_on_non_async_fn() {
        let src = "fn cleanup() { }\nfn main() { defer cleanup() }\n";
        assert!(
            !has(src),
            "non-async fn should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_when_name_shadows_async_fn() {
        // Two bindings for the same name → ambiguous → zero-FP: silent.
        let src = "async fn teardown() { }\nfn main() { fn teardown() { } defer teardown() }\n";
        assert!(
            !has(src),
            "shadowed name should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn severity_is_warning() {
        use ascript::check::Severity;
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        let diag = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-async-call")
            .unwrap();
        assert!(
            matches!(diag.severity, Severity::Warning),
            "severity should be Warning, got {:?}",
            diag.severity
        );
    }

    #[test]
    fn message_is_verbatim() {
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        let diag = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-async-call")
            .unwrap();
        assert_eq!(
            diag.message,
            "deferred call to async fn 'teardown' will panic at runtime — use 'defer await teardown(…)'"
        );
    }

    #[test]
    fn suppression_via_toml_allow() {
        use std::fs;
        let dir = std::env::temp_dir().join("ascript_defer_async_call_toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("asyncdefer.as");
        fs::write(
            &f,
            "async fn teardown() { }\nfn main() { defer teardown() }\n",
        )
        .unwrap();
        fs::write(
            dir.join("ascript.toml"),
            "[lint]\nallow = [\"defer-async-call\"]\n",
        )
        .unwrap();
        let out = Command::new(bin()).arg("check").arg(&f).output().unwrap();
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "toml allow must suppress defer-async-call; out: {combined}"
        );
        assert!(
            !combined.contains("defer-async-call"),
            "toml allow must drop the diagnostic; out: {combined}"
        );
    }
}

// ── ELIDE Task 0.3: collector cost envelope measurement ──────────────────────
//
// Run with:
//   cargo test --test check --release -- elide_collector_cost_envelope --ignored --nocapture
//
// This harness measures the wall time of the full checker pipeline
// (parse → tree_builder::build_tree → resolve::resolve → Table::build → pass::run)
// which is the cost CEILING for the ELIDE ElisionSet collector (same walk + cheap
// set inserts). Results are printed to stdout for capture into bench/ELIDE_RESULTS.md.
//
// The harness is kept IGNORED so it never runs in normal CI (it's a one-shot
// measurement tool, not a correctness gate).
#[test]
#[ignore]
fn elide_collector_cost_envelope() {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    // ── collect corpus files ─────────────────────────────────────────────────
    fn walk_as(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk_as(&p, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("as") {
                    out.push(p);
                }
            }
        }
    }
    let mut corpus: Vec<PathBuf> = Vec::new();
    walk_as(Path::new("examples"), &mut corpus);
    corpus.sort();
    let corpus_len = corpus.len();

    // ── per-file checker pipeline timing ────────────────────────────────────
    // Warm up the allocator with one throw-away pass.
    let _ = ascript::check::analyze("let _ = 1\n");

    const REPS: u32 = 10; // repeats per file to get a stable sample

    let mut per_file: Vec<(String, u64 /* µs median */, usize /* lines */)> = Vec::new();

    for path in &corpus {
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let lines = src.lines().count();
        let name = path.display().to_string();

        let mut times: Vec<Duration> = Vec::with_capacity(REPS as usize);
        for _ in 0..REPS {
            let t0 = Instant::now();
            let _ = ascript::check::analyze(&src);
            times.push(t0.elapsed());
        }
        times.sort();
        let median_us = times[REPS as usize / 2].as_micros() as u64;
        per_file.push((name, median_us, lines));
    }

    // ── synthetic ~5 k-line module ───────────────────────────────────────────
    // Build a big source by duplicating the all_features content with different
    // top-level wrapper fn names until we reach ≥5000 lines.
    let base = fs::read_to_string("examples/all_features.as").unwrap_or_default();
    let base_lines = base.lines().count();
    let needed = 5000usize.saturating_sub(base_lines).div_ceil(base_lines);
    let mut big_src = String::with_capacity(base.len() * (needed + 1));
    big_src.push_str(&base);
    for i in 0..needed {
        // Wrap each copy in a unique fn so top-level names don't clash.
        big_src.push_str(&format!(
            "\nfn _big_wrap_{}() {{\n{}\n}}\n",
            i,
            base.replace('\n', "\n  ")
        ));
    }
    let big_lines = big_src.lines().count();
    let mut big_times: Vec<Duration> = Vec::with_capacity(REPS as usize);
    for _ in 0..REPS {
        let t0 = Instant::now();
        let _ = ascript::check::analyze(&big_src);
        big_times.push(t0.elapsed());
    }
    big_times.sort();
    let big_median_us = big_times[REPS as usize / 2].as_micros() as u64;

    // ── statistics ───────────────────────────────────────────────────────────
    let mut all_us: Vec<u64> = per_file.iter().map(|(_, us, _)| *us).collect();
    all_us.sort();
    let min_us = *all_us.first().unwrap_or(&0);
    let max_us = *all_us.last().unwrap_or(&0);
    let median_us = all_us[all_us.len() / 2];

    // ── end-to-end ascript run timing (20 iterations over the corpus) ────────
    // We only time files that are known-runnable (skip server/worker/db etc).
    // Use the same EXAMPLE_SKIPS approach as the conformance tests: just run them
    // all and accept that some will fail — we care about wall time, not output.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let run_iters = 20u32;

    // Pick a representative subset: the largest files from examples/ (not advanced/)
    // that are likely to run quickly. Use all_features.as as the canonical file.
    let representative_files: Vec<&Path> = corpus
        .iter()
        .filter(|p| {
            let s = p.display().to_string();
            // Skip net/db/worker/tui/ai examples that block or need services
            !s.contains("net")
                && !s.contains("postgres")
                && !s.contains("redis")
                && !s.contains("sqlite")
                && !s.contains("tui")
                && !s.contains("ai")
                && !s.contains("server")
                && !s.contains("sse")
                && !s.contains("ws")
                && !s.contains("advanced")
                && !s.contains("app/")
                && !s.contains("workers_")
        })
        .map(|p| p.as_path())
        .collect();

    let mut e2e_results: Vec<(String, u64 /* ms */,  usize /* lines */)> = Vec::new();

    for path in &representative_files {
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let lines = src.lines().count();
        let name = path.display().to_string();

        let mut run_times: Vec<u64> = Vec::with_capacity(run_iters as usize);
        for _ in 0..run_iters {
            let t0 = Instant::now();
            let _ = Command::new(bin)
                .arg("run")
                .arg(path)
                .output();
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            run_times.push(elapsed_ms);
        }
        run_times.sort();
        let median_ms = run_times[run_times.len() / 2];
        e2e_results.push((name, median_ms, lines));
    }

    // Compute projected regression %:
    // checker_median_us / (e2e_median_ms * 1000) * 100
    // We use the grand median of checker times vs the grand median of e2e times
    // for the representative files. Match by file.
    let mut regression_pcts: Vec<f64> = Vec::new();
    for (e2e_name, e2e_ms, _) in &e2e_results {
        if let Some((_, check_us, _)) = per_file.iter().find(|(n, _, _)| n == e2e_name) {
            if *e2e_ms > 0 {
                let check_ms = *check_us as f64 / 1000.0;
                let pct = check_ms / *e2e_ms as f64 * 100.0;
                regression_pcts.push(pct);
            }
        }
    }
    regression_pcts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_regression_pct = if regression_pcts.is_empty() {
        f64::NAN
    } else {
        regression_pcts[regression_pcts.len() / 2]
    };

    // ── print results ────────────────────────────────────────────────────────
    println!();
    println!("=== ELIDE Task 0.3: Collector cost envelope (release build) ===");
    println!("Profile: release  |  Reps per file: {REPS}  |  Corpus size: {corpus_len} files");
    println!();
    println!("--- Checker pipeline wall time (parse+resolve+table+pass) ---");
    println!("  min    = {:.2} ms ({:.0} µs)", min_us as f64 / 1000.0, min_us as f64);
    println!("  median = {:.2} ms ({:.0} µs)", median_us as f64 / 1000.0, median_us as f64);
    println!("  max    = {:.2} ms ({:.0} µs)", max_us as f64 / 1000.0, max_us as f64);
    println!();
    println!("--- Synthetic ~{big_lines}-line module ---");
    println!("  median checker time = {:.2} ms ({big_median_us} µs)", big_median_us as f64 / 1000.0);
    println!();
    println!("--- Per-file checker pipeline times (median over {REPS} reps) ---");
    println!("{:<60} {:>6}  {:>6}", "file", "lines", "µs");
    println!("{}", "-".repeat(76));
    for (name, us, lines) in &per_file {
        println!("{:<60} {:>6}  {:>6}", name, lines, us);
    }
    println!();
    println!("--- End-to-end `ascript run` times (median over {run_iters} iterations) ---");
    println!("{:<60} {:>6}  {:>8}", "file", "lines", "e2e ms");
    println!("{}", "-".repeat(76));
    for (name, ms, lines) in &e2e_results {
        // Find checker time for this file
        let check_us = per_file.iter().find(|(n,_,_)| n==name).map(|(_,u,_)| *u).unwrap_or(0);
        let pct = if *ms > 0 { check_us as f64 / 1000.0 / *ms as f64 * 100.0 } else { 0.0 };
        println!("{:<60} {:>6}  {:>8}  (checker {:.2} ms = {:.1}% of e2e)", name, lines, ms, check_us as f64/1000.0, pct);
    }
    println!();
    println!("--- Projected regression summary ---");
    println!("  Median checker/e2e ratio = {:.2}%", median_regression_pct);
    println!("  Spec §5.1 budget: ≤ 2% corpus geomean AND ≤ 1 ms for ≤500-line module");
    println!("  500-line module ceiling: {:.2} ms (budget: ≤ 1.00 ms)", median_us as f64 / 1000.0);
    println!();
    println!("NOTE: 'collector ceiling' = checker pipeline time (collector = same walk + set inserts).");
    println!("Task 4.1 decides whether option (a) (always-on) ships based on these numbers.");
}
