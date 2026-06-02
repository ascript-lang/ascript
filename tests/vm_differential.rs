//! Differential test harness: the bytecode VM must produce byte-identical
//! results to the tree-walking interpreter. This is the differential oracle for
//! the whole VM sub-project.
//!
//! How it works: there is no "return the value" API on the tree-walker — it
//! prints. So for a given expression `e` we run `print(e)` through the
//! tree-walker (captured stdout) and compare it, byte for byte, against the VM's
//! final `Value` formatted exactly the way the `print` builtin formats it:
//! `format!("{value}\n")` (the `print` builtin does `v.to_string()` + `'\n'`,
//! and the VM and tree-walker share the same `Value` `Display`). The whole point
//! is byte-identical agreement, so number formatting (`7` vs `7.0`, `2.5`),
//! string rendering, `true`/`nil`, etc. must all match.
//!
//! V1 covers only the arithmetic / literal subset the VM implements. The
//! comparison helper is intentionally generic: as the VM grows (V2+), this file
//! swaps in full-program stdout comparison and grows the case set toward the
//! whole `examples/` corpus — the gate flips to the entire corpus at V10.
//!
//! NEVER weaken the byte-identical assertion. A divergence here is a real bug in
//! the VM (or the compiler), not something to paper over by trimming or
//! normalizing the comparison.

/// Assert that evaluating `expr_src` on the VM yields the same observable
/// output as `print(expr_src)` on the tree-walker, byte for byte.
async fn assert_vm_matches_treewalker(expr_src: &str) {
    // Tree-walker: print(expr) → captured stdout. `print` appends a single '\n'.
    let tw = ascript::run_source(&format!("print({expr_src})"))
        .await
        .expect("tree-walker ok");

    // VM: eval expr → Value, then format it exactly the way `print` does.
    let v = ascript::vm_eval_source(expr_src).await.expect("vm ok");
    let vm_str = format!("{v}\n");

    assert_eq!(
        tw, vm_str,
        "VM diverged from tree-walker for `{expr_src}`\n  tree-walker: {tw:?}\n  vm:          {vm_str:?}"
    );
}

#[tokio::test]
async fn vm_matches_treewalker_arithmetic_and_literals() {
    let cases = [
        // arithmetic
        "1+2",
        "2*3+4",
        "-(5)",
        "(1+2)*4",
        "10/4",
        "7 % 3",
        "2 ** 10",
        "3.5 + 0.5",
        // bare literals
        "42",
        "true",
        "nil",
        "\"hi\"",
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_matches_treewalker_number_forms() {
    // Every numeric literal form the lexer accepts must const-fold to the
    // identical f64 the tree-walker produces (byte-identical `print`).
    let cases = [
        "0xff",          // hex
        "0XFF",          // upper-case hex prefix + digits
        "0b1010",        // binary
        "0B1111",        // upper-case binary prefix
        "1e3",           // scientific
        "1E3",           // upper-case exponent
        "2.5e-2",        // float + signed exponent
        "1_000",         // underscore digit separators
        "1_000_000",     // more separators
        "0xFF_FF",       // underscores in hex
        "3.14",          // float
        "0.5",           // leading-zero float
        "0",             // bare zero
        "1000000000000", // large integer printed without exponent
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_matches_treewalker_string_escapes() {
    // The full escape set the tree-walker's `escape_char` handles, plus single
    // quotes and lenient passthrough of unknown escapes (AScript has no
    // `\u`/`\x`). Byte-identical via `print`.
    let cases = [
        r#""a\nb""#,            // newline
        r#""tab\there""#,       // tab
        r#""cr\rhere""#,        // carriage return
        r#""quote\"x""#,        // escaped double-quote
        r#""back\\slash""#,     // escaped backslash
        r#""nul\0end""#,        // NUL
        r#"'single'"#,          // single-quoted
        r#"'he said \'hi\''"#,  // escaped single-quote inside single quotes
        r#""unknown\qescape""#, // lenient passthrough: \q -> q
        r#""""#,                // empty string
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_matches_treewalker_templates() {
    // Template interpolation: literal chunks + `${expr}` slots, with the value
    // coercion matching `Value::to_string()` exactly.
    let cases = [
        "`plain`",                   // no interpolation
        "`hi ${1+2}!`",              // arithmetic interpolation
        "`n=${42}`",                 // number coercion
        "`b=${true}`",               // bool coercion
        "`nothing=${nil}`",          // nil coercion
        "`s=${\"inner\"}`",          // nested string literal
        "`${1} and ${2} and ${3}`",  // multiple interpolations
        "`${`${1}`}`",               // nested template
        "`leading${1}`",             // leading literal chunk only
        "`${1}trailing`",            // trailing literal chunk only
        "`${1}${2}`",                // adjacent interpolations, empty middle chunk
        "`tab\tin\ttemplate`",       // escapes inside a template chunk
        "`escaped \\` backtick`",    // escaped backtick
        "`escaped \\${not} interp`", // escaped ${ (literal, not interpolation)
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

/// Assert that running `src` (a full program, for side effects) on the VM
/// produces byte-identical stdout to running it on the tree-walker. This is the
/// V2 output-path differential: `print(...)` must agree exactly.
async fn assert_vm_run_matches_treewalker(src: &str) {
    let tw = ascript::run_source(src).await.expect("tree-walker ok");
    let (vm_out, code) = ascript::vm_run_source(src).await.expect("vm ok");
    assert_eq!(code, None, "no exit code expected for `{src}`");
    assert_eq!(
        tw, vm_out,
        "VM stdout diverged from tree-walker for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm_out:?}"
    );
}

#[tokio::test]
async fn vm_run_print_exact_output() {
    // Byte-exact against the spec's stated outputs.
    assert_eq!(
        ascript::vm_run_source("print(1 + 2)").await.expect("ok"),
        ("3\n".to_string(), None)
    );
    assert_eq!(
        ascript::vm_run_source("print(\"hi\")").await.expect("ok"),
        ("hi\n".to_string(), None)
    );
}

#[tokio::test]
async fn vm_run_print_matches_treewalker() {
    let cases = [
        "print(1+2)",
        "print(\"hi\")",
        "print(42)",
        "print(true)",
        "print(nil)",
        "print(2 * 3 + 4)",
        "print(10 / 4)",
        // multiple leading print statements + a trailing expression.
        "print(1)\nprint(2)\nprint(3)\n4",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_run_locals_match_treewalker() {
    let cases = [
        // let + local read + arithmetic
        "let x = 1\nlet y = x + 1\nprint(y)",
        // reassignment
        "let x = 1\nx = x + 5\nprint(x)",
        // block shadowing: inner x gets a distinct slot; outer is unchanged
        "let x = 1\n{ let x = 2\n print(x) }\nprint(x)",
        // const binds like let at runtime
        "const c = 10\nprint(c * 2)",
        // assignment is a statement that stores; the next read sees the new value.
        // (Assignment-as-expression yielding its value — e.g. `print(x = 5)` — is
        // exercised in the compiler unit tests via the trailing-value path; the
        // CST front-end does not currently parse an assignment inside call args,
        // a pre-existing parser limitation unrelated to locals, so it is not run
        // through the `print(...)`-based differential harness here.)
        "let x = 1\nx = 5\nprint(x)",
        // let with no initializer binds nil
        "let x\nprint(x)",
        // string locals + template interpolation through a local
        "let name = \"world\"\nprint(`hi ${name}!`)",
        // multiple locals interacting
        "let a = 2\nlet b = 3\nlet c = a * b + 1\nprint(c)",
        // reassign then read inside and after a block (shared slot)
        "let n = 0\nn = n + 10\n{ n = n + 5\n print(n) }\nprint(n)",
        // block that declares a local used only within the block
        "{ let z = 7\n print(z * z) }\nprint(\"done\")",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ----- V2-T4: bare builtins through GET_GLOBAL → CALL -------------------------

#[tokio::test]
async fn vm_run_bare_builtins_match_treewalker() {
    // Every bare builtin reachable as a call in the V2 expression subset must run
    // through the VM's GET_GLOBAL → CALL path byte-identically to the tree-walker.
    // (Array/object-literal arguments await the container-literal compiler slice;
    // these all use number/string/nested-call arguments that compile today.)
    let cases = [
        // len over a string and over range's array result.
        "print(len(\"hello\"))",     // 5
        "print(len(range(5)))",       // range(5) -> [0,1,2,3,4]; len -> 5
        // type strings (must match the tree-walker's exact spellings).
        "print(type(1))",             // number
        "print(type(\"x\"))",         // string
        "print(type(true))",          // bool
        "print(type(nil))",           // nil
        "print(type(range(2)))",      // array
        // Ok / Err formatting ([value, nil] / [nil, {message}]).
        "print(Ok(1))",
        "print(Err(\"e\"))",
        // assert(true) is a no-op (no output, no panic); a trailing print proves
        // the program continued.
        "assert(true)\nprint(\"ok\")",
        // a nested builtin chain.
        "print(len(range(len(\"abc\"))))", // len("abc")=3 -> range(3) -> len 3
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_eval_first_class_builtin_reference_matches_treewalker() {
    // A bare builtin name used as a *value* (not called) is the `Value::Builtin`
    // itself; printing it must render identically on both engines. This exercises
    // the compiler's `compile_name_ref` GET_GLOBAL path (first-class builtins).
    for name in ["print", "len", "type", "range", "assert", "Ok", "Err"] {
        assert_vm_matches_treewalker(name).await;
    }
}

// ----- V2-T5: complete arithmetic / comparison / equality / range / errors ----

#[tokio::test]
async fn vm_string_concat_matches_treewalker() {
    // `+` on two strings concatenates; the result renders identically on both
    // engines (no surrounding quotes — `print` uses the raw string `Display`).
    let cases = [
        "\"a\" + \"b\"",
        "\"foo\" + \"\"",
        "\"\" + \"bar\"",
        "\"x\" + \"y\" + \"z\"",
        "`a${1}` + `b${2}`", // template results are strings → concat
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_range_value_matches_treewalker() {
    // `a..b` is an eager half-open `array<number>`; the printed array form and
    // `len(...)` over it must agree byte-for-byte with the tree-walker.
    let cases = [
        "0..5",          // [0, 1, 2, 3, 4]
        "0..0",          // [] (empty)
        "3..3",          // [] (empty, non-zero bound)
        "2..5",          // [2, 3, 4]
        "len(0..5)",     // 5
        "len(0..0)",     // 0
        "1..1",          // []
        "(1 + 1)..5",    // additive binds tighter: (2)..5 → [2,3,4]
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_comparisons_match_treewalker() {
    let cases = [
        "1 < 2", "2 < 1", "2 >= 2", "3 >= 4", "1 <= 1", "5 > 2", "2 > 5", "2 <= 1",
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_equality_matches_treewalker() {
    let cases = [
        "1 == 1",
        "1 == 2",
        "1 != 2",
        "1 != 1",
        "\"a\" == \"a\"",
        "\"a\" == \"b\"",
        "\"a\" != \"b\"",
        "true == true",
        "true == false",
        "nil == nil",
        "nil != nil",
        // Container equality is pointer identity on both engines (a fresh literal
        // is never equal to another): `range(1) == range(1)` is `false`. We use
        // `range(...)` rather than an `[...]` array literal because array-literal
        // compilation is a separate VM slice (V2-T4b); the equality *semantics*
        // (identity, via `apply_binop`'s `Value` `==`) are what this exercises.
        "range(1) == range(1)",
        "range(1) != range(1)",
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

/// Assert both engines FAIL identically for `expr_src` (run as `print(expr)`):
/// same Tier-2 panic message AND the same source span. A divergence here is a
/// real diagnostics-parity bug, not something to normalize away.
async fn assert_vm_error_matches_treewalker(expr_src: &str) {
    let src = format!("print({expr_src})");
    let tw = ascript::run_source(&src).await;
    let vm = ascript::vm_run_source(&src).await;
    match (tw, vm) {
        (Err(tw_err), Err(vm_err)) => {
            assert_eq!(
                tw_err.message, vm_err.message,
                "panic message diverged for `{expr_src}`\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, vm_err.message
            );
            assert_eq!(
                tw_err.span, vm_err.span,
                "panic span diverged for `{expr_src}` (msg {:?})\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, tw_err.span, vm_err.span
            );
        }
        (tw, vm) => panic!(
            "expected BOTH engines to error for `{expr_src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
        ),
    }
}

#[tokio::test]
async fn vm_operator_errors_match_treewalker() {
    // Each must produce the SAME message and SAME span on both engines.
    let cases = [
        "-(true)",      // cannot negate a non-number
        "true + 1",     // operator requires two numbers ...
        "1 < \"x\"",    // operator requires two numbers ... (ordering)
        "\"a\" - 1",    // string is not concat for `-`
        "nil * 2",      // non-number operand
        "true < false", // ordering on bools
    ];
    for expr in cases {
        assert_vm_error_matches_treewalker(expr).await;
    }
}

// ---- short-circuit `&&` / `||` / `??` (V2-T6) ---------------------------

#[tokio::test]
async fn vm_short_circuit_result_values_match_treewalker() {
    // The tree-walker returns the actual OPERAND (JS-like), not a coerced bool.
    // `&&`: truthy left -> right; falsy left -> the (falsy) left value.
    // `||`: truthy left -> left;  falsy left -> right.
    // `??`: nil left   -> right;  non-nil left -> left.
    // AScript truthiness: only `nil` and `false` are falsy; `0` and `""` are
    // TRUTHY (so `0 || 9` is `0`, `"" || "x"` is `""`).
    let cases = [
        "true && 5",
        "false && 5",
        "true || 5",
        "false || 5",
        "nil ?? 7",
        "5 ?? 7",
        "0 || 9",      // 0 is truthy -> 0
        "0 && 9",      // 0 is truthy -> 9
        "\"\" || \"x\"", // "" is truthy -> ""
        "\"\" && \"x\"", // "" is truthy -> "x"
        "nil && 1",    // nil is falsy -> nil
        "nil || 1",    // nil is falsy -> 1
        "false ?? 7",  // false is non-nil -> false
        "nil ?? nil",  // -> nil
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_short_circuit_chained_match_treewalker() {
    let cases = [
        "true && true && 3",
        "true && false && 3",
        "false || false || 7",
        "false || true || 7",
        "nil ?? nil ?? 3",
        "nil ?? 2 ?? 3",
        // mixed precedence / interaction with other operators
        "1 + 2 == 3 && 9",
        "true && 1 + 2",
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_short_circuit_does_not_evaluate_rhs() {
    // PROVE the RHS is not evaluated when the operator short-circuits: the RHS
    // has an observable side effect (a `print`). Both engines must print ONLY
    // `done`, never `ran`. The short-circuit expression is bound with `let` so
    // it begins its own statement (a bare leading `(` would otherwise glue onto
    // the previous line as a call — an ASI quirk unrelated to this feature).
    let programs = [
        // `&&`: left is falsy -> RHS print skipped.
        "let cond = false\nlet r = cond && print(\"ran\")\nprint(\"done\")",
        // `||`: left is truthy -> RHS print skipped.
        "let cond = true\nlet r = cond || print(\"ran\")\nprint(\"done\")",
        // `??`: left is non-nil -> RHS print skipped.
        "let r = 5 ?? print(\"ran\")\nprint(\"done\")",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ----- V2-T4b: array/object literals + index/member read ----------------------

#[tokio::test]
async fn vm_array_object_literals_match_treewalker() {
    // Literals render byte-identically (the `print` of a container uses the same
    // `Value::Display`). Objects sit in expression position (a top-level `{...}`
    // parses as a block) — printed through `print(...)`.
    let cases = [
        "print([1, 2, 3])",
        "print([1, \"a\", true])",
        "print([[1], [2]])",     // nested arrays
        "print([])",             // empty array
        "print({a: 1, b: 2})",
        "print({\"k\": 5})",     // string key
        "print({})",             // empty object
        "print({a: 1, b: [2, 3], c: {d: 4}})", // nested
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_index_read_matches_treewalker() {
    let cases = [
        "print([10, 20, 30][1])",        // 20
        "print({a: 1}[\"a\"])",          // 1 (object index by string key)
        "print({a: 1}[\"missing\"])",    // nil (missing object key → nil)
        "let a = [1, 2, 3]\nprint(a[0])\nprint(a[2])", // local-array index
        "let o = {x: 9}\nprint(o[\"x\"])",
        "print([[1, 2], [3, 4]][1][0])", // nested index → 3
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_member_read_matches_treewalker() {
    let cases = [
        "let o = {a: 1}\nprint(o.a)",          // 1
        "print({a: 1, b: 2}.b)",               // 2
        "let o = {a: 1}\nprint(o.missing)",    // nil (missing object key → nil)
        "let o = {nested: {deep: 7}}\nprint(o.nested.deep)", // chained member
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_opt_member_read_matches_treewalker() {
    let cases = [
        "let o = nil\nprint(o?.a)",     // nil receiver → nil
        "let o = {a: 1}\nprint(o?.a)",  // 1
        "let o = {a: 1}\nprint(o?.missing)", // nil (present receiver, missing key)
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_short_circuit_does_evaluate_rhs_when_needed() {
    // The complementary case: when NOT short-circuited the RHS print runs, so
    // both engines print `ran` then `done`. Guards against a lowering that
    // wrongly skips the RHS.
    let programs = [
        // `&&`: left truthy -> RHS runs.
        "let cond = true\nlet r = cond && print(\"ran\")\nprint(\"done\")",
        // `||`: left falsy -> RHS runs.
        "let cond = false\nlet r = cond || print(\"ran\")\nprint(\"done\")",
        // `??`: left nil -> RHS runs.
        "let r = nil ?? print(\"ran\")\nprint(\"done\")",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}
