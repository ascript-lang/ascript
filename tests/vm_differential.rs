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
async fn vm_matches_treewalker_bitwise_and_wrapping() {
    // NUM §3.2: bitwise / shift / wrapping operators must be byte-identical across
    // the tree-walker and the VM (results AND the precedence shape).
    let cases = [
        "0xFF & 0b1010",
        "12 | 10",
        "12 ^ 10",
        "~0",
        "~5",
        "1 << 3",
        "(1 << 16) | 256",
        "1 >> 0",
        "256 >> 4",
        "(0 - 8) >> 1",     // arithmetic sign-extension
        "(0 - 1) << 1",     // -1 << 1 == -2 (bit-loss, no trap)
        "1 << 63",          // i64::MIN (defined)
        "5 +% 3",
        "5 -% 8",
        "6 *% 7",
        "9223372036854775807 +% 1", // wraps to i64::MIN
        "6 & 2 == 2",               // Go precedence: (6&2)==2
        "1 | 2 == 3",               // (1|2)==3
        "1 + 1 << 2",               // 1 + (1<<2)
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_matches_treewalker_instanceof_reserved_types() {
    // NUM §6: `x instanceof int|float|number|string|bool` must be byte-identical
    // across the tree-walker and the VM (the VM uses a dedicated `INSTANCE_OF_TYPE`
    // opcode; the tree-walker intercepts the reserved-name RHS).
    let cases = [
        "5 instanceof int",
        "5 instanceof float",
        "5 instanceof number",
        "5.0 instanceof int",
        "5.0 instanceof float",
        "5.0 instanceof number",
        "\"hi\" instanceof string",
        "true instanceof bool",
        "5 instanceof string",
        "\"x\" instanceof int",
        "(1 + 2) instanceof int",
        "(1.0 + 2) instanceof float",
        "(7 / 2) instanceof int",
    ];
    for expr in cases {
        assert_vm_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_matches_treewalker_bitwise_panics() {
    // The Tier-2 panics (shift-amount out of range, bitwise-on-float) must be
    // byte-identical across both engines. Run as full programs and compare the
    // captured panic output.
    for src in [
        "print(1 << 64)",
        "print(1 << (0 - 1))",
        "print(1 & 2.0)",
        "print(~1.0)",
        "print(1 +% 2.0)",
    ] {
        // Both engines must FAIL with the same panic message (Control::Panic →
        // AsError). `run_source` / `vm_run_source` surface it as `Err(AsError)`.
        let tw_err = ascript::run_source(src)
            .await
            .expect_err("tree-walker should panic")
            .to_string();
        let vm_err = ascript::vm_run_source(src)
            .await
            .expect_err("vm should panic")
            .to_string();
        assert_eq!(
            tw_err, vm_err,
            "panic message diverged for `{src}`\n  tw: {tw_err:?}\n  vm: {vm_err:?}"
        );
    }
}

#[tokio::test]
async fn vm_matches_treewalker_number_forms() {
    // Every numeric literal form the lexer accepts must const-fold to the
    // identical f64 the tree-walker produces (byte-identical `print`).
    let cases = [
        "0xff",          // hex
        "0XFF",          // upper-case hex prefix + digits
        "0X1F",          // upper-case hex, mixed digits/letters
        "0b1010",        // binary
        "0B1111",        // upper-case binary prefix
        "0B11",          // short upper-case binary
        "1e3",           // scientific
        "1E3",           // upper-case exponent
        "1.5e-3",        // float + signed negative exponent
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
        r#""\t\r\0""#,          // adjacent control escapes (tab/CR/NUL)
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
        "`price \\$5`",              // escaped bare $ (\$ -> $)
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
async fn vm_adaptive_specialization_matches_treewalker() {
    // V11-T4 PEP-659 adaptive specialization must stay byte-identical to the
    // tree-walker on the four scenarios that drive the warmup / specialize / deopt
    // machinery hard: a hot numeric loop (specializes to ADD_NUMBER), a loop that
    // alternates number+string adds (constant deopt churn → never specializes), a
    // decimal-arithmetic loop (ADD_DECIMAL), and global-heavy code that hammers a
    // GET_GLOBAL site (GET_GLOBAL_CACHED). All four must agree exactly.
    let cases = [
        // Hot numeric loop: the `acc = acc + i` ADD site specializes to Number.
        "let acc = 0\nfor (i in range(0, 50)) { acc = acc + i }\nprint(acc)",
        // Numeric *, /, -, % in a hot loop (extends to the other numeric binops).
        "let p = 1\nfor (i in range(1, 12)) { p = p * 2 }\nprint(p)\n\
         let s = 1000\nfor (i in range(0, 20)) { s = s - i }\nprint(s)",
        // Polymorphic ADD: alternating number+string adds keep deopting; the result
        // must still be byte-identical (numbers sum, strings concat).
        "let n = 0\nlet t = \"\"\n\
         for (i in range(0, 20)) { if (i % 2 == 0) { n = n + i } else { t = t + \"x\" } }\n\
         print(n)\nprint(t)",
        // (Decimal-arithmetic specialization — ADD_DECIMAL — is covered by the
        // run-loop unit test `add_specializes_to_decimal`; it can't run through a
        // source-level differential here because constructing a `Value::Decimal`
        // needs `import { from } from "std/decimal"`, and the VM does not compile
        // `import` until V12. The whole-corpus differential exercises the decimal
        // examples once that lands.)
        // Global-heavy: a tight loop calling the `print` builtin via the same
        // GET_GLOBAL site each iteration (GET_GLOBAL_CACHED hits every time).
        "for (i in range(0, 12)) { print(i * i) }",
        // Specialize then DEOPT mid-program: warm a number ADD site hot, then feed
        // it strings so it deopts and concatenates — both engines agree.
        "fn add(a, b) { return a + b }\n\
         let r = 0\nfor (i in range(0, 30)) { r = add(r, 1) }\nprint(r)\n\
         print(add(\"foo\", \"bar\"))",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_run_closures_match_treewalker() {
    // V4-T2: functions/arrows compile to a nested FnProto + CLOSURE. Calling is
    // V4-T3, but the closure VALUE is observable via `type(...)` (== "function")
    // and a fn declaration binds its name. These must be byte-identical to the
    // tree-walker.
    let cases = [
        // An arrow expression's value is a function.
        "let f = (x) => x\nprint(type(f))",
        // A multi-statement-body arrow likewise.
        "let g = (a, b) => { return a + b }\nprint(type(g))",
        // A fn declaration binds a function value to its name.
        "fn greet() { return 1 }\nprint(type(greet))",
        // A fn that uses only its params + builtins (no captures) compiles.
        "fn add(a, b) { return a + b }\nprint(type(add))",
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
        "print(len(\"hello\"))", // 5
        "print(len(range(5)))",  // range(5) -> [0,1,2,3,4]; len -> 5
        // type strings (must match the tree-walker's exact spellings).
        "print(type(1))",        // number
        "print(type(\"x\"))",    // string
        "print(type(true))",     // bool
        "print(type(nil))",      // nil
        "print(type(range(2)))", // array
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
        "0..5",       // [0, 1, 2, 3, 4]
        "0..0",       // [] (empty)
        "3..3",       // [] (empty, non-zero bound)
        "2..5",       // [2, 3, 4]
        "len(0..5)",  // 5
        "len(0..0)",  // 0
        "1..1",       // []
        "(1 + 1)..5", // additive binds tighter: (2)..5 → [2,3,4]
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

/// Assert both engines FAIL identically for `expr_src`: same Tier-2 panic message
/// AND the same source span. A divergence here is a real diagnostics-parity bug,
/// not something to normalize away.
///
/// The expression is run BARE (its own statement) — NOT wrapped in `print(...)`.
/// Previously these were wrapped only to keep the panicking sub-expression off the
/// statement start (a leading bare expression's CST node carries leading-newline
/// trivia, which used to push the VM's span one byte early — the #132 off-by-one).
/// Now that the compiler anchors every panicking op at the trivia-trimmed code
/// span, the wrapper is unnecessary, so the bare form is the stronger test.
async fn assert_vm_error_matches_treewalker(expr_src: &str) {
    let src = expr_src.to_string();
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
        "0 || 9",        // 0 is truthy -> 0
        "0 && 9",        // 0 is truthy -> 9
        "\"\" || \"x\"", // "" is truthy -> ""
        "\"\" && \"x\"", // "" is truthy -> "x"
        "nil && 1",      // nil is falsy -> nil
        "nil || 1",      // nil is falsy -> 1
        "false ?? 7",    // false is non-nil -> false
        "nil ?? nil",    // -> nil
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

// ----- V10-T1: destructuring `let` (array + object + rest) ---------------------
//
// `let [a, b, ...r] = arr` and `let {a, b as local, "k" as v, ...rest} = obj`.
// The compiler lowers the RHS into a temp slot, validates its type ONCE (a panic
// at the RHS span on a non-array / non-object), then binds each position/key
// (missing → nil) and the optional `...rest` collector (array tail / leftover
// object keys). Every case is byte-identical to the tree-walker's
// `Stmt::LetDestructure` / `Stmt::LetDestructureObject`.

#[tokio::test]
async fn vm_destructure_array_matches_treewalker() {
    let programs = [
        // Basic positional binding.
        "let [a, b, c] = [1, 2, 3]\nprint(a)\nprint(b)\nprint(c)",
        // Rest collects the TAIL into a new array.
        "let [first, ...rest] = [1, 2, 3, 4]\nprint(first)\nprint(rest)",
        // Fewer names than elements (no rest) — extra elements are simply dropped.
        "let [x] = [10, 20, 30]\nprint(x)",
        // More names than elements — missing positions bind nil.
        "let [a, b, c] = [1]\nprint(a)\nprint(b)\nprint(c)",
        // Rest over an exhausted array yields an empty array.
        "let [a, b, ...rest] = [1, 2]\nprint(rest)",
        // `const` destructuring behaves identically at runtime.
        "const [a, b] = [7, 8]\nprint(a + b)",
        // Destructure from an expression RHS (evaluated once).
        "let pair = [3, 4]\nlet [a, b] = pair\nprint(a * b)",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_destructure_object_matches_treewalker() {
    let programs = [
        // Shorthand keys.
        "let {x, y} = {x: 10, y: 20}\nprint(x)\nprint(y)",
        // Rename with `as` and a quoted key.
        "let {a as A, \"k\" as v} = {a: 1, k: 2}\nprint(A)\nprint(v)",
        // Missing key binds nil.
        "let {a, missing} = {a: 5}\nprint(a)\nprint(missing)",
        // Rest collects the LEFTOVER keys (excluding bound source keys), in order.
        "let {a, ...rest} = {a: 1, b: 2, c: 3}\nprint(rest)",
        // Rest after multiple bound keys.
        "let {a, b, ...rest} = {a: 1, b: 2, c: 3, d: 4}\nprint(rest)",
        // Object destructuring of a class instance reads its FIELDS (not methods).
        "class P {\n  x: number\n  y: number\n}\nlet p = P.from({x: 3, y: 4})\nlet {x, y} = p\nprint(x + y)",
        // `const` object destructuring.
        "const {a, b} = {a: 1, b: 2}\nprint(a + b)",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_destructure_captured_in_closure_matches_treewalker() {
    // A destructured binding captured by a closure must use the cell-backed slot
    // exactly like a plain `let`, so the closure observes the bound value.
    let programs = [
        "let [a, b] = [1, 2]\nlet f = () => a + b\nprint(f())",
        "let {x, y} = {x: 5, y: 6}\nfn g() {\n  return x * y\n}\nprint(g())",
        // Rest binding captured.
        "let [head, ...tail] = [10, 20, 30]\nlet f = () => tail\nprint(f())",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_destructure_type_errors_match_treewalker() {
    // Destructuring a non-array / non-object RHS is a Tier-2 panic anchored at the
    // RHS expression's span, with a message that names the value's type. Both
    // engines must agree on message AND span.
    let cases = [
        "let [a, b] = 5",   // cannot destructure a non-array value of type number
        "let [a] = \"hi\"", // ... of type string
        "let [a] = {x: 1}", // ... of type object (an object is not an array)
        "let [a] = nil",    // ... of type nil
        "let {a, b} = 5",   // cannot destructure a non-object value of type number
        "let {a} = [1, 2]", // ... of type array (an array is not an object)
        "let {a} = \"hi\"", // ... of type string
        "let {a} = nil",    // ... of type nil
    ];
    for expr in cases {
        assert_vm_error_matches_treewalker(expr).await;
    }
}

#[tokio::test]
async fn vm_match_guard_ending_in_ident_matches_treewalker() {
    // A match guard ending in a bare identifier right before `=>` (`n if n == lim
    // => ...`) must not be parsed as an arrow that swallows the arm body. Closes
    // the V10 differential blind spot: guards were previously only tested ending
    // in literals. Both engines must produce byte-identical output.
    let programs = [
        // The exact repro from the bug report.
        "fn d(v, lim) {\n  return match v {\n    n if n == lim => \"eq\",\n    other => \"o\",\n  }\n}\nprint(d(2, 2))\nprint(d(5, 2))",
        // Guard with another ident-ending comparison (`x == n`).
        "fn d(x, n) { return match x { y if y == n => \"hit\", _ => \"miss\" } }\nprint(d(3, 3))\nprint(d(3, 4))",
        // Guard with `&&` ending in an identifier.
        "fn d(v, lim) { return match v { n if n > 0 && n == lim => \"a\", other => \"o\" } }\nprint(d(2, 2))\nprint(d(-1, 2))",
        // Guard containing a parenthesized arrow (parens disambiguate; must work).
        "fn d(v) { return match v { x if (() => true)() => \"y\", _ => \"n\" } }\nprint(d(1))",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_closure_into_stdlib_hof_matches_treewalker() {
    // V12 #176: a VM-compiled script function is a `Value::Closure`. Stdlib native
    // HOFs that take a CALLBACK argument used to enumerate the callable `Value`
    // variants WITHOUT `Closure`, so a VM program passing an arrow/`fn` to e.g.
    // `stream.filter`/`stream.map`/`array.sort`'s comparator panicked "expects a
    // function, got function". Both engines must now run byte-identically.
    let programs = [
        // stream.filter + stream.map with arrow closures (the streams_and_testing repro).
        "import * as stream from \"std/stream\"\nlet s = stream.map(stream.filter(stream.from([1,2,3,4,5,6]), x => x % 2 == 0), x => x * 10)\nprint(await stream.collect(s))",
        // stream.reduce with a 2-arg closure.
        "import * as stream from \"std/stream\"\nprint(await stream.reduce(stream.from([1,2,3,4]), (a, x) => a + x, 0))",
        // stream.forEach + stream.find with closures.
        "import * as stream from \"std/stream\"\nawait stream.forEach(stream.from([\"a\",\"b\"]), x => print(x))\nprint(await stream.find(stream.from([1,2,3,4]), x => x > 2))",
        // A NAMED `fn` (also a Closure on the VM) passed to a stdlib HOF.
        "import * as stream from \"std/stream\"\nfn keep(x) { return x > 1 }\nprint(await stream.collect(stream.filter(stream.from([0,1,2,3]), keep)))",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_method_call_spread_matches_treewalker() {
    // V12 #177: spread in a member-method call `recv.m(...args)`. The VM lowers this
    // to `CALL_METHOD_SPREAD name` — evaluate the receiver, build the flattened
    // runtime args-array (the SAME machinery as free-call `CALL_SPREAD`), then
    // dispatch EXACTLY like `CALL_METHOD` (schema hook → IC compiled-method →
    // `read_member`→`call_value`). Every form must be byte-identical to the
    // tree-walker.
    let programs = [
        // (1) Object closure field called with a fully-spread arg list.
        "let o = { add: (a, b, c) => a + b + c }\nlet args = [1, 2, 3]\nprint(o.add(...args))",
        // (2) Instance method (compiled, IC fast-path) with a fully-spread list.
        "class Calc {\n  fn sum(a, b, c) { return a + b + c }\n}\n\
         let c = Calc()\nlet args = [10, 20, 30]\nprint(c.sum(...args))",
        // (3) Mixed positional + spread: `inst.m(1, ...[2, 3])`.
        "class Calc {\n  fn sum(a, b, c) { return a + b + c }\n}\n\
         let c = Calc()\nprint(c.sum(1, ...[2, 3]))",
        // (4) Mixed with the spread NOT last (a trailing positional after a spread).
        "let o = { f: (a, b, c, d) => a * 1000 + b * 100 + c * 10 + d }\n\
         print(o.f(1, ...[2, 3], 4))",
        // (5) Two spreads in one call.
        "let o = { f: (a, b, c, d) => `${a}${b}${c}${d}` }\n\
         print(o.f(...[1, 2], ...[3, 4]))",
        // (6) Empty spread (a zero-element array) — argc resolves to 0 dynamically.
        "let o = { f: () => 42 }\nprint(o.f(...[]))",
        // (7) Inherited method via the chain (defining-class dispatch) + spread.
        "class Base {\n  fn combine(a, b) { return a - b }\n}\n\
         class Derived extends Base {}\n\
         let d = Derived()\nprint(d.combine(...[9, 4]))",
        // (8) Stdlib module-namespace method with spread (the stdlib_completeness repro).
        "import * as math from \"std/math\"\nprint(math.min(...[3, 1, 2]))\nprint(math.max(...[3, 1, 2]))",
        // (9) Receiver is itself a call result (receiver evaluated once, then spread).
        "fn mk() { return { g: (a, b) => a + b } }\nprint(mk().g(...[5, 7]))",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_method_call_spread_errors_match_treewalker() {
    // A non-array operand spread into a METHOD call is the SAME Tier-2 panic as a
    // free call: `can only spread an array as call arguments, got {type}`, anchored
    // at the operand span. Both engines must agree on message AND span.
    let cases = [
        "let o = { add: (a, b) => a + b }\no.add(...42)",
        "let o = { add: (a, b) => a + b }\no.add(...\"hi\")",
        "let o = { add: (a, b) => a + b }\no.add(1, ...nil)",
    ];
    for expr in cases {
        assert_vm_error_matches_treewalker(expr).await;
    }
}

// ----- V2-T7: widen the differential gate -------------------------------------
//
// Two complementary widenings of the byte-identical gate:
//
//  1. SYNC MULTI-FEATURE SNIPPETS (`vm_run_sync_multi_feature_programs`): realistic
//     multi-line programs that COMBINE the full V1+V2 feature set in one run —
//     locals + reassignment + arithmetic + array/object literals + index/member
//     reads + templates + short-circuit (`&&`/`||`/`??`) + `print`. The single-
//     feature tests above prove each construct in isolation; these prove they
//     compose correctly through the compiler + VM, byte-for-byte against the
//     tree-walker. This is the V2 sync-subset gate.
//
//  2. WHOLE-CORPUS OPT-OUT GATE (`vm_run_whole_corpus_matches_treewalker`): the
//     CENTRAL VM correctness proof (oracle #1). At V10 the VM is language-complete,
//     so the old curated allow-list is RETIRED and the gate FLIPS to an opt-OUT
//     model: it enumerates EVERY `examples/*.as` AND `examples/advanced/*.as` file,
//     runs each through BOTH engines, and asserts byte-identical stdout+exit —
//     EXCEPT for an explicit, per-file-documented SKIP list (`EXAMPLE_SKIPS`). The
//     goal is the MAXIMUM set the VM supports running byte-identically, with the
//     skip list shrinking toward zero as V12 (import) lands. NEVER relax the byte-
//     identical assertion or add a skip without a real, documented reason: a
//     divergence is a real VM/compiler bug, not something to paper over.

#[tokio::test]
async fn vm_run_sync_multi_feature_programs() {
    // Each program is a realistic SYNC snippet combining several V1+V2 features.
    // All must be byte-identical to the tree-walker.
    let programs = [
        // (a) build an array via a local, index into it, print via a template.
        "let xs = [10, 20, 30]\nlet i = 1\nprint(`xs[${i}] = ${xs[i]}`)",
        // (b) object construction + member reads + arithmetic on fields.
        "let p = {x: 3, y: 4}\nprint(p.x * p.x + p.y * p.y)",
        // (c) nested data + short-circuit `??` defaulting on a missing key + print.
        "let cfg = {name: \"svc\"}\nlet port = cfg.port ?? 8080\nprint(`${cfg.name}:${port}`)",
        // (d) string building: templates + `+` concatenation across locals.
        "let first = \"ada\"\nlet last = \"lovelace\"\nlet full = first + \" \" + last\nprint(`name=${full} len=${len(full)}`)",
        // (e) let-chains with reassignment + computed prints.
        "let total = 0\ntotal = total + 5\ntotal = total * 3\nprint(total)\nprint(total - 1)",
        // (f) array of objects, index then member, arithmetic.
        "let users = [{age: 30}, {age: 12}]\nprint(users[0].age + users[1].age)",
        // (g) nested object/array, chained index+member, template render.
        "let data = {items: [{n: \"a\"}, {n: \"b\"}]}\nprint(`first=${data.items[0].n} second=${data.items[1].n}`)",
        // (h) short-circuit selecting an operand, fed into arithmetic.
        "let a = 0\nlet b = 7\nlet pick = a || b\nprint(pick + 1)",
        // (i) block scoping + outer reassignment + post-block read.
        "let acc = 1\n{ let acc = 100\n print(acc) }\nacc = acc + 2\nprint(acc)",
        // (j) equality/comparison results threaded through `&&`/`||` and printed.
        "let n = 5\nprint(n > 0 && n < 10)\nprint(n == 5 || n == 6)",
        // (k) computed index from arithmetic + range array.
        "let r = 0..5\nlet idx = 1 + 2\nprint(r[idx])",
        // (l) object with array field + len + template, multiple prints.
        "let inv = {tags: [\"x\", \"y\", \"z\"]}\nprint(len(inv.tags))\nprint(`tags: ${inv.tags[0]}, ${inv.tags[2]}`)",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

/// Why a file is SKIPPED by the whole-corpus opt-out gate. Every skip carries a
/// one-line, machine-checkable reason; the gate ENFORCES that the skip is still
/// justified (see `vm_whole_corpus_skips_are_still_justified`) so a skip cannot
/// silently outlive its reason — as each tracked VM gap lands, the corresponding
/// skips start FAILING the guard and must be deleted (V12-T1 compiled stdlib
/// `import`, which deleted the whole `V12Import` category and unskipped 24 files).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    /// The example's output is inherently NON-DETERMINISTIC across two runs (an
    /// ephemeral UDP port, `crypto.randomBytes`, `time.sleep` measured ms / a live
    /// `now` timestamp, or a live network event stream). The VM is byte-identical
    /// in the DETERMINISTIC portion of the output, but a stdout-equality oracle
    /// over two separate runs (tree-walker run vs VM run) cannot match the random/
    /// time/network-dependent bytes — it is not a VM divergence. Documented-only
    /// (also makes live network calls), so it is not executed in the gate.
    Nondeterministic,
    /// The example mutates a FIXED shared external resource (a hard-coded `/tmp`
    /// directory tree) and is NOT self-isolating. The multiple corpus oracles (the
    /// whole-corpus gate, the recorded-goldens checker, the three-way gate) run in
    /// PARALLEL under `cargo test` and each runs every example twice (tree-walker
    /// then VM); concurrent runs of such a file clobber each other's files, so the
    /// tree-walker can observe a half-written/half-deleted tree (a flaky
    /// `stat failed` race). It passes deterministically in ISOLATION and is still
    /// covered once by the CLI/conformance suites — only the parallel multi-oracle
    /// corpus run can't run it safely. Documented-only (not executed here).
    SharedExternalState,
    /// A network-peer / long-running SERVER example: it calls a forever-blocking
    /// `serve`/accept loop that needs a separate client process (and never
    /// terminates on its own). It cannot run headless in a unit test — it hangs
    /// EVEN the tree-walker oracle — so it is excluded the same way the CLI/
    /// conformance suites leave the server/peer examples out of their run set.
    LongRunningServer,
}

/// The explicit, per-file SKIP list for the whole-corpus opt-out gate. EVERY entry
/// has a one-line reason. The list is the documented complement of "every example
/// the VM runs byte-identically TODAY"; it shrinks toward ~zero as V12 (import +
/// `.from` defaults) lands. Adding an entry requires a real reason — NEVER skip a
/// file just to make the gate green.
const EXAMPLE_SKIPS: &[(&str, SkipReason)] = &[
    // V12-T1 landed stdlib `import`, so the bulk of the corpus now runs byte-
    // identically and has been UNSKIPPED. What remains here are files that, once
    // the import binds, hit a DIFFERENT documented VM gap (or are inherently
    // nondeterministic / server-blocking). Each is tracked precisely below.
    //
    // ---- `.from` defaults / nested-class coercion / typed-parse (task #157) ----
    // NOW FIXED. `typed_parse.as` and `shape_validation.as` validated into classes
    // with field defaults + nested-class fields + an Object→`map<K,Class>` boundary
    // coercion — the `.from`/typed-parse divergence. With the VM now populating
    // `FieldSchema.default` (lowered default expr) and `Class.def_env` (a shared
    // module env holding the class/enum bindings), the SHARED `validate_into` runs
    // byte-identically, so both have been UNSKIPPED into the whole-corpus gate.
    // `advanced/typed_http.as` (a self-contained client+server whose route does a
    // typed-parse with a defaulted field) ALSO runs byte-identically now and is
    // UNSKIPPED — its in-process round-trip completes and the output is deterministic
    // (it binds an ephemeral port but, unlike `typed_api.as`, never prints it).
    // ---- stdlib callable-accept gap: Value::Closure (V12 #176) — NOW FIXED -----
    // `streams_and_testing.as` was here (it passes arrow/`fn` closures to
    // `stream.filter`/`map`/`reduce`/…). With native HOFs now accepting
    // `Value::Closure` it runs byte-identically on the VM and has been UNSKIPPED
    // into the whole-corpus gate. `typed_api.as` / `typed_http.as` (server route +
    // middleware closures) ALSO run on the VM now, but each stays skipped for a
    // DIFFERENT, now-surfaced reason — see below.
    //
    // ---- member-method-call spread `recv.m(...args)` (V12 #177) — NOW FIXED -----
    // `stdlib_completeness.as` used `math.min(...[..])` / `math.max(...[..])` — a
    // spread in a module-namespace member call. The VM now lowers method-call spread
    // to `CALL_METHOD_SPREAD` (shared dispatch with `CALL_METHOD`), so the file runs
    // byte-identically and has been UNSKIPPED into the whole-corpus gate (its golden
    // is recorded).
    //
    // ---- Inherently nondeterministic output (NOT a VM divergence) -------------
    // Deterministic output is byte-identical; the differing bytes are an ephemeral
    // UDP port / `crypto.randomBytes` / sleep-ms + live `now` / a live network
    // event stream — unmatchable by a two-run stdout-equality oracle.
    ("examples/host_info.as", SkipReason::Nondeterministic),
    (
        "examples/advanced/crypto_and_compress.as",
        SkipReason::Nondeterministic,
    ),
    (
        "examples/advanced/datetime_intl.as",
        SkipReason::Nondeterministic,
    ),
    (
        "examples/advanced/sse_client.as",
        SkipReason::Nondeterministic,
    ),
    // `ws_client.as` connects to `ws://127.0.0.1:8788` (the `ws_server.as` peer).
    // Its output is environment-dependent on TWO axes: (1) whether anything is
    // listening on that port — with the server up it completes an echo round-trip,
    // with it down it prints a connection-refused line; and (2) the OS errno text
    // in that line is platform-specific (`os error 61` on macOS, `111` on Linux).
    // A two-run stdout-equality oracle matches on a single machine (both runs hit
    // the same errno) but a golden frozen on one OS cannot reproduce on another —
    // it is not a VM divergence (the live two-run + three-way gates still prove
    // byte-identity on whatever platform runs them). Same class as `sse_client.as`.
    ("examples/advanced/ws_client.as", SkipReason::Nondeterministic),
    // SP11 std/ai examples: their output is environment-dependent — with no provider
    // key they print Tier-1 "no credential" messages, but the OpenAI-compatible local
    // line ("local model unavailable: <reqwest error>") depends on whether a local
    // Ollama is listening, and a real key would print a live model response. So the
    // output is not a stable two-run constant. They are exercised by the dedicated
    // fixture-replay suite (`tests/ai.rs`), not this stdout-equality corpus oracle.
    ("examples/advanced/ai_chat.as", SkipReason::Nondeterministic),
    ("examples/advanced/ai_tools.as", SkipReason::Nondeterministic),
    // `typed_api.as` is byte-identical on the VM EXCEPT for the ephemeral bound
    // port it prints (`server bound on http://127.0.0.1:<port>`): the OS assigns a
    // fresh port to each of the two separate runs (tree-walker then VM), so the
    // stdout-equality oracle cannot match that one line. Every other byte (the
    // route/middleware closures now run on the VM via the #176 fix, plus the
    // POST/GET round-trips and 400 rejections) is identical. Not a VM divergence.
    (
        "examples/advanced/typed_api.as",
        SkipReason::Nondeterministic,
    ),
    // `http_client.as` makes live HTTP calls to `http://127.0.0.1:8787`, the
    // companion `http_server.as` (which is NOT running under tests — it is itself
    // skipped as `LongRunningServer`). With the server absent, every call fails with
    // a connection-refused / timeout error whose message and retry timing are
    // non-deterministic across runs (OS errno text, elapsed-ms counters). The two
    // separate tree-walker and VM oracle runs therefore produce differing byte
    // sequences — it is not a VM divergence; the VM is byte-identical in the
    // deterministic portion. Covered end-to-end by the CLI suite when the server
    // is available; excluded here because the two-run stdout-equality oracle cannot
    // match live-network output.
    (
        "examples/advanced/http_client.as",
        SkipReason::Nondeterministic,
    ),
    // ---- Mutates a fixed shared /tmp tree (races across parallel oracles) ------
    ("examples/system.as", SkipReason::SharedExternalState),
    (
        "examples/advanced/fs_toolkit.as",
        SkipReason::SharedExternalState,
    ),
    // ---- Network-peer / long-running servers (cannot run headless) ------------
    // Forever-serving HTTP API: blocks on `serve` awaiting a client in a separate
    // process; it does not terminate on its own and hangs even the tree-walker.
    (
        "examples/advanced/http_server.as",
        SkipReason::LongRunningServer,
    ),
    // Forever-running WebSocket echo server: blocks on an `accept` loop awaiting a
    // peer; same headless-impossible / hangs-the-oracle situation.
    (
        "examples/advanced/ws_server.as",
        SkipReason::LongRunningServer,
    ),
];

/// Enumerate EVERY `examples/*.as` and `examples/advanced/*.as` file, paths
/// relative to the crate root, sorted for deterministic ordering.
fn all_corpus_examples() -> Vec<String> {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut out = Vec::new();
    for dir in ["examples", "examples/advanced"] {
        let p = std::path::Path::new(root).join(dir);
        let rd = std::fs::read_dir(&p).unwrap_or_else(|e| panic!("read_dir {dir}: {e}"));
        for entry in rd {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|x| x.to_str()) == Some("as") {
                out.push(format!(
                    "{dir}/{}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    out.sort();
    out
}

fn skip_reason(rel: &str) -> Option<SkipReason> {
    EXAMPLE_SKIPS
        .iter()
        .find(|(p, _)| *p == rel)
        .map(|(_, r)| *r)
}

/// Whether an example imports a stdlib module that is NOT available in the CURRENT
/// Cargo feature configuration (e.g. `std/csv` / `std/sql` under
/// `--no-default-features`). Such an example errors identically on BOTH engines
/// (`unknown standard library module '…'`), so there is no byte-identical
/// reference output to compare and it is excluded from the corpus oracles for THIS
/// build only (it still runs under the full-feature build). Detected by actually
/// running the tree-walker and matching the feature-gated module error — this is
/// config-driven, not a hard-coded list, so the set adjusts automatically per
/// feature config.
async fn feature_unavailable_in_this_build(src: &str) -> bool {
    match ascript::run_source_exit(src).await {
        Err(e) => e.message.starts_with("unknown standard library module"),
        Ok(_) => false,
    }
}

#[tokio::test]
async fn vm_run_whole_corpus_matches_treewalker() {
    // THE CENTRAL VM CORRECTNESS PROOF (oracle #1). For EVERY corpus example that
    // is not on the documented `EXAMPLE_SKIPS` list, the VM's stdout+exit must be
    // byte-identical to the tree-walker's over the SAME file contents. A divergence
    // here is a real VM/compiler bug — NEVER relax this assertion or skip a file to
    // make it pass.
    let root = env!("CARGO_MANIFEST_DIR");
    let mut ran = 0usize;
    let mut skipped = 0usize;
    let mut feature_skipped = 0usize;
    for rel in all_corpus_examples() {
        if skip_reason(&rel).is_some() {
            skipped += 1;
            continue;
        }
        let path = std::path::Path::new(root).join(&rel);
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read example {rel}: {e}"));
        // A byte-identical example that imports a module unavailable in THIS Cargo
        // feature config (e.g. `std/csv` under `--no-default-features`): both
        // engines error identically with the feature-gated module message, so it
        // has no byte-identical reference here. Assert the VM errors the SAME way
        // and skip it for this build (it runs under the full-feature build).
        if feature_unavailable_in_this_build(&src).await {
            let tw = ascript::run_source_exit(&src).await;
            let vm = ascript::vm_run_source(&src).await;
            match (tw, vm) {
                (Err(a), Err(b)) => assert_eq!(
                    a.message, b.message,
                    "feature-gated `{rel}` errored differently on the two engines"
                ),
                (tw, vm) => panic!(
                    "feature-gated `{rel}` did not error on both engines: tw={tw:?} vm={vm:?}"
                ),
            }
            feature_skipped += 1;
            continue;
        }
        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on non-skipped {rel}: {e:?}"));
        let vm = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("VM failed on non-skipped {rel}: {e:?}"));
        assert_eq!(
            tw, vm,
            "VM diverged from tree-walker for example `{rel}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
        );
        ran += 1;
    }
    // Sanity: the gate must actually exercise the bulk of the corpus, and the
    // arithmetic must add up (no file silently missing from either set).
    assert_eq!(
        ran + skipped + feature_skipped,
        all_corpus_examples().len(),
        "every corpus example must be either run, explicitly skipped, or feature-skipped"
    );
    // Floor is config-aware: the full-feature build runs the bulk of the corpus;
    // `--no-default-features` strips most stdlib modules, so far fewer examples are
    // runnable (only the bare-language ones) — the differential still PROVES
    // byte-identity for whatever DOES run.
    let floor = if cfg!(feature = "data") { 38 } else { 1 };
    assert!(
        ran >= floor,
        "expected the VM to run >={floor} examples byte-identically, ran {ran}"
    );
    eprintln!(
        "whole-corpus gate: {ran} examples byte-identical, {skipped} skipped, \
         {feature_skipped} feature-skipped (modules unavailable in this build)"
    );
}

#[tokio::test]
async fn vm_whole_corpus_skips_are_still_justified() {
    // Guard that keeps the skip list HONEST so it can only shrink, never rot:
    //  - As each tracked VM gap lands (V12-T1 `import`, #176 stdlib-Closure, #157
    //    `.from` defaults, #177 method-call spread), the file runs byte-identically
    //    and its skip is DELETED, moving the file into the must-run whole-corpus
    //    gate. (`stdlib_completeness.as` left the skip list when #177 landed.)
    //  - Nondeterministic / SharedExternalState / LongRunningServer: documented-only
    //    (random/time/network bytes, a shared-/tmp race, or a forever-blocking
    //    server), so they are not executed; we only assert the file exists so a
    //    rename is caught.
    //  - Every skip entry must name a real corpus file (no stale paths).
    let root = env!("CARGO_MANIFEST_DIR");
    let corpus = all_corpus_examples();
    for (rel, reason) in EXAMPLE_SKIPS {
        assert!(
            corpus.iter().any(|c| c == rel),
            "skip-list entry `{rel}` is not a real corpus example (stale path?)"
        );
        let _src = std::fs::read_to_string(std::path::Path::new(root).join(rel))
            .unwrap_or_else(|e| panic!("read skipped example {rel}: {e}"));
        match reason {
            SkipReason::Nondeterministic
            | SkipReason::SharedExternalState
            | SkipReason::LongRunningServer => {
                // Documented-only; not executed (random/time/network bytes can't
                // match across two runs, a shared-/tmp file races across the
                // parallel oracles, or a server blocks forever). Existence is
                // already asserted above.
            }
        }
    }
}

// ----- V10-T5: test-suite differential (a spread of import-free constructs) ----
//
// The whole-corpus gate above is the CORE of the V10 differential, but most of the
// corpus is currently `import`-gated (V12). This suite adds breadth at the level
// the VM fully supports: a representative spread of LANGUAGE constructs in one
// self-contained `.as` program each (no `import`), run VM-vs-tree-walker
// byte-identically. It complements the corpus gate (whole programs) and the
// snippet tests (single features) with realistic combinations of the language-
// complete feature set: closures, recursion, error model, async, generators,
// classes/enums/super/`.from`, destructuring/spread, and `match`.

#[tokio::test]
async fn vm_test_suite_differential_constructs() {
    let programs = [
        // (1) recursion + closures + higher-order functions.
        "fn make_adder(n) { return (x) => x + n }\n\
         let add10 = make_adder(10)\n\
         fn fib(n) { if (n < 2) { return n } return fib(n - 1) + fib(n - 2) }\n\
         print(add10(5))\nprint(fib(10))",
        // (2) error model: `?` propagation + `!` unwrap + recover.
        "fn parse(ok) { if (ok) { return Ok(42) } return Err(\"bad\") }\n\
         fn use_it(ok) { let v = parse(ok)?\n return v + 1 }\n\
         print(use_it(true))\n\
         let r = recover(() => parse(false)!)\n\
         print(r[1].message)",
        // (3) async/await: a chain of awaited async fns.
        "async fn double(n) { return n * 2 }\n\
         async fn run() { let a = await double(5)\n let b = await double(a)\n return b }\n\
         async fn main() { print(await run()) }\n\
         await main()",
        // (4) generators: `fn*` + `for await` consumption + `.next()`.
        "fn* counter(n) { let i = 0\n while (i < n) { yield i * i\n i = i + 1 } }\n\
         let total = 0\n for await (v in counter(4)) { total = total + v }\n print(total)\n\
         let g = counter(2)\n print(g.next())\n print(g.next())\n print(g.next())",
        // (5) classes + inheritance + super + method dispatch.
        "class Animal {\n  name: string\n  fn init(n) { self.name = n }\n  fn speak() { return `${self.name} makes a sound` }\n}\n\
         class Dog extends Animal {\n  fn speak() { return `${super.speak()} (woof)` }\n}\n\
         let d = Dog(\"Rex\")\n print(d.speak())\n print(d.name)",
        // (6) enums + match with variant patterns, plus a guard on a bound ident.
        "enum Shape { Circle, Square, Other }\n\
         fn classify(s) { return match s {\n  Shape.Circle => \"circle\",\n  Shape.Square => \"square\",\n  _ => \"other\"\n } }\n\
         fn bucket(n) { return match n {\n  x if x < 0 => \"neg\",\n  0 => \"zero\",\n  _ => \"pos\"\n } }\n\
         print(classify(Shape.Circle))\n print(classify(Shape.Other))\n print(bucket(-3))\n print(bucket(0))\n print(bucket(9))",
        // (7) destructuring (array + object + rest) + spread (literals + calls).
        "let [a, b, ...rest] = [1, 2, 3, 4, 5]\n\
         let {x, y as why} = {x: 10, y: 20}\n\
         let merged = [...rest, a, b]\n\
         fn sum(...ns) { let t = 0\n for (n of ns) { t = t + n } return t }\n\
         print(a + b)\n print(x + why)\n print(merged)\n print(sum(...merged))",
        // (8) `.from` typed-parse on a simple class + field reads.
        "class Point {\n  x: number\n  y: number\n}\n\
         let p = Point.from({x: 3, y: 4})\n print(p.x * p.x + p.y * p.y)",
        // (9) match on array/object patterns with binding + nesting.
        "fn head(xs) { return match xs {\n  [] => \"empty\",\n  [only] => `one:${only}`,\n  [first, ...tail] => `first:${first} rest:${len(tail)}`\n } }\n\
         print(head([]))\n print(head([7]))\n print(head([1, 2, 3]))",
        // (10) ternary + short-circuit + compound assignment in a loop.
        "let acc = 0\n for (i in 0..6) { acc += i % 2 == 0 ? i : 0 }\n\
         let flag = acc > 0 && acc < 100\n print(acc)\n print(flag)",
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
        "print([[1], [2]])", // nested arrays
        "print([])",         // empty array
        "print({a: 1, b: 2})",
        "print({\"k\": 5})",                   // string key
        "print({})",                           // empty object
        "print({a: 1, b: [2, 3], c: {d: 4}})", // nested
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_index_read_matches_treewalker() {
    let cases = [
        "print([10, 20, 30][1])",                      // 20
        "print({a: 1}[\"a\"])",                        // 1 (object index by string key)
        "print({a: 1}[\"missing\"])",                  // nil (missing object key → nil)
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
        "let o = {a: 1}\nprint(o.a)",                        // 1
        "print({a: 1, b: 2}.b)",                             // 2
        "let o = {a: 1}\nprint(o.missing)",                  // nil (missing object key → nil)
        "let o = {nested: {deep: 7}}\nprint(o.nested.deep)", // chained member
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_opt_member_read_matches_treewalker() {
    let cases = [
        "let o = nil\nprint(o?.a)",          // nil receiver → nil
        "let o = {a: 1}\nprint(o?.a)",       // 1
        "let o = {a: 1}\nprint(o?.missing)", // nil (present receiver, missing key)
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ----- SP1 Phase A: `a?.m(args)` optional method call -------------------------
//
// The VM used to REJECT `a?.m(...)` at compile (`optional method calls
// (a?.m(...)) not yet supported (V9)`); the tree-walker runs it. These cases pin
// the VM to the tree-walker's exact semantics, three-way (tree-walker ==
// specialized-VM == generic-VM), byte-for-byte: a nil receiver yields `nil`
// WITHOUT evaluating the arguments and short-circuits the rest of the postfix
// chain; a non-nil receiver is an ordinary bound method call; `?.` guards ONLY
// the nil receiver (a non-nil receiver with a missing method still panics
// `value is not callable`).

/// Three-way OK assertion: `tree-walker == specialized-VM == generic-VM`,
/// byte-identical stdout, no exit code.
async fn assert_opt_call_ok_three_way(src: &str) {
    let tw = ascript::run_source(src).await.expect("tree-walker ok");
    let (vm, vm_code) = ascript::vm_run_source(src).await.expect("specialized vm ok");
    let (gen, gen_code) = ascript::vm_run_source_generic(src)
        .await
        .expect("generic vm ok");
    assert_eq!(vm_code, None, "no exit code expected for `{src}`");
    assert_eq!(gen_code, None, "no exit code expected for `{src}`");
    assert_eq!(
        tw, vm,
        "specialized VM diverged from tree-walker for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
    );
    assert_eq!(
        tw, gen,
        "generic VM diverged from tree-walker for `{src}`\n  tw: {tw:?}\n  gen: {gen:?}"
    );
}

/// Three-way ERROR assertion: all three engines panic with identical message +
/// span.
async fn assert_opt_call_error_three_way(src: &str) {
    let tw = ascript::run_source(src).await;
    let vm = ascript::vm_run_source(src).await;
    let gen = ascript::vm_run_source_generic(src).await;
    match (tw, vm, gen) {
        (Err(tw_err), Err(vm_err), Err(gen_err)) => {
            assert_eq!(
                tw_err.message, vm_err.message,
                "specialized-VM panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, vm_err.message
            );
            assert_eq!(
                tw_err.span, vm_err.span,
                "specialized-VM panic span diverged for `{src}` (msg {:?})\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, tw_err.span, vm_err.span
            );
            assert_eq!(
                tw_err.message, gen_err.message,
                "generic-VM panic message diverged for `{src}`\n  tw: {:?}\n  gen: {:?}",
                tw_err.message, gen_err.message
            );
            assert_eq!(
                tw_err.span, gen_err.span,
                "generic-VM panic span diverged for `{src}` (msg {:?})\n  tw: {:?}\n  gen: {:?}",
                tw_err.message, tw_err.span, gen_err.span
            );
        }
        (tw, vm, gen) => panic!(
            "expected ALL THREE engines to error for `{src}`\n  tree-walker: {tw:?}\n  vm: {vm:?}\n  gen: {gen:?}"
        ),
    }
}

#[tokio::test]
async fn vm_opt_call_nil_skips_args() {
    // nil receiver: result nil AND the argument's side effect must NOT run.
    assert_opt_call_ok_three_way(
        "fn se() { print(\"ARG\")\n  return 1 }\nlet a = nil\nprint(a?.m(se()))\n",
    )
    .await;
}

#[tokio::test]
async fn vm_opt_call_nonnil() {
    // non-nil receiver: ordinary bound method call.
    assert_opt_call_ok_three_way(
        "class C { fn m(x) { return x + 1 } }\nlet c = C()\nprint(c?.m(10))\n",
    )
    .await;
}

#[tokio::test]
async fn vm_opt_call_chain_nil() {
    // chained: the whole postfix chain short-circuits when the receiver is nil.
    assert_opt_call_ok_three_way("let a = nil\nprint(a?.m().n().o)\n").await;
}

#[tokio::test]
async fn vm_opt_call_missing_method() {
    // non-nil receiver, missing method -> all engines panic "value is not
    // callable", identical span (`?.` does NOT guard a missing method).
    assert_opt_call_error_three_way("class C {}\nlet c = C()\nprint(c?.nope(1))\n").await;
}

#[tokio::test]
async fn vm_opt_call_mixed() {
    // mixed optional member + optional call in one chain.
    assert_opt_call_ok_three_way(
        "class C { fn m() { return 5 } }\nlet c = C()\nprint(c?.m())\nlet a = nil\nprint(a?.b?.m())\n",
    )
    .await;
}

// ----- V3-T1: if / else if / else ---------------------------------------------

#[tokio::test]
async fn vm_if_statement_matches_treewalker() {
    // The tree-walker (the differential oracle) requires parenthesized
    // conditions: `if (cond) { ... }`. The CST front-end accepts the same form
    // (the cond is a ParenExpr the compiler transparently unwraps).
    let cases = [
        // bare if, condition true → body runs.
        "if (true) { print(\"yes\") }",
        // bare if, condition false → body skipped, no output.
        "if (false) { print(\"no\") }",
        // if/else, then-branch taken.
        "let x = 5\nif (x > 3) { print(\"big\") } else { print(\"small\") }",
        // if/else, else-branch taken.
        "let x = 1\nif (x > 3) { print(\"big\") } else { print(\"small\") }",
        // else if chain → middle arm taken.
        "let x = 2\nif (x == 1) { print(\"one\") } else if (x == 2) { print(\"two\") } else { print(\"other\") }",
        // else if chain → first arm taken.
        "let x = 1\nif (x == 1) { print(\"one\") } else if (x == 2) { print(\"two\") } else { print(\"other\") }",
        // else if chain → final else taken.
        "let x = 9\nif (x == 1) { print(\"one\") } else if (x == 2) { print(\"two\") } else { print(\"other\") }",
        // longer else-if chain (multiple `else if`).
        "let x = 3\nif (x == 1) { print(\"a\") } else if (x == 2) { print(\"b\") } else if (x == 3) { print(\"c\") } else { print(\"d\") }",
        // if between other statements: stack must stay balanced.
        "print(\"a\")\nif (true) { print(\"b\") }\nprint(\"c\")",
        // if with a block-scoped let inside the body.
        "if (true) { let y = 10\n print(y) }",
        // nested ifs.
        "let x = 5\nif (x > 0) { if (x > 3) { print(\"both\") } else { print(\"only-positive\") } }",
        // nested ifs, inner false.
        "let x = 2\nif (x > 0) { if (x > 3) { print(\"both\") } else { print(\"only-positive\") } }",
        // truthiness: a non-bool condition follows is_truthy (0 is truthy, only
        // nil/false are falsy in AScript).
        "if (0) { print(\"zero-truthy\") }",
        "if (\"\") { print(\"empty-str-truthy\") }",
        "if (nil) { print(\"nil\") } else { print(\"nil-falsy\") }",
        // if statement followed by a trailing expression value (program value).
        "let x = 7\nif (x > 5) { print(\"hi\") }\nx",
        // else-only side-effect, with a trailing read proving locals survive.
        "let n = 0\nif (false) { n = 1 } else { n = 2 }\nprint(n)",
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

// ----- V3-T2: while + break / continue ----------------------------------------

#[tokio::test]
async fn vm_while_loop_matches_treewalker() {
    // The tree-walker (the oracle) requires parenthesized conditions:
    // `while (cond) { ... }`. The CST front-end accepts the same form (the cond
    // is a ParenExpr the compiler transparently unwraps).
    let cases = [
        // Counting loop: prints 0,1,2 then stops.
        "let i = 0\nwhile (i < 3) { print(i)\n i = i + 1 }",
        // Loop that never runs (condition false up front): no output.
        "let i = 5\nwhile (i < 3) { print(i)\n i = i + 1 }",
        // `break` from an infinite loop: prints 0,1 then breaks at i == 2.
        "let i = 0\nwhile (true) { if (i == 2) { break }\n print(i)\n i = i + 1 }",
        // `continue`: increment first, skip the print when i == 3 -> 1,2,4,5.
        "let i = 0\nwhile (i < 5) { i = i + 1\n if (i == 3) { continue }\n print(i) }",
        // `break` on the very first iteration: no output.
        "let i = 0\nwhile (true) { break\n print(i) }",
        // Loop with a block-scoped let in the body (fresh slot each iteration).
        "let i = 0\nwhile (i < 3) { let sq = i * i\n print(sq)\n i = i + 1 }",
        // Loop between other statements: the stack must stay balanced.
        "print(\"start\")\nlet i = 0\nwhile (i < 2) { print(i)\n i = i + 1 }\nprint(\"end\")",
        // Trailing expression value after a loop (proves locals survive).
        "let i = 0\nwhile (i < 4) { i = i + 1 }\ni",
        // Non-bool truthy condition (a non-empty string is truthy; break ends it).
        "let i = 0\nwhile (\"go\") { if (i == 2) { break }\n print(i)\n i = i + 1 }",
        // `break` nested inside an `if` inside the loop (the if pushes no ctx, so
        // the break still targets the loop).
        "let i = 0\nwhile (i < 10) { if (i >= 3) { break }\n print(i)\n i = i + 1 }",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_nested_while_break_inner_only_matches_treewalker() {
    // `break`/`continue` target the INNERMOST loop. The inner loop breaks at j ==
    // 2, but the outer loop keeps running, so both engines emit the same grid of
    // (i, j) pairs. Guards against a loop-context stack that targets the wrong
    // loop.
    let cases = [
        // Inner break: for each outer i, inner prints j=0,1 then breaks.
        "let i = 0\nwhile (i < 3) { let j = 0\n while (j < 5) { if (j == 2) { break }\n print(i * 10 + j)\n j = j + 1 }\n i = i + 1 }",
        // Inner continue: inner skips j == 1 but runs to completion (0,2).
        "let i = 0\nwhile (i < 2) { let j = 0\n while (j < 3) { j = j + 1\n if (j == 1) { continue }\n print(i * 10 + j) }\n i = i + 1 }",
        // Outer break after the inner loop completes: outer stops at i == 1.
        "let i = 0\nwhile (i < 5) { if (i == 1) { break }\n let j = 0\n while (j < 2) { print(j)\n j = j + 1 }\n i = i + 1 }",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_break_continue_outside_loop_match_treewalker() {
    // `break`/`continue` with no enclosing loop is rejected by BOTH engines with
    // the identical message. The tree-walker raises it at runtime; the VM rejects
    // it at compile time — but both surface as an `Err(AsError)` from the public
    // entry points, so the differential check compares the error message.
    for (src, expected) in [
        ("break", "'break' outside of a loop"),
        ("continue", "'continue' outside of a loop"),
        // Inside an `if` (which does NOT open a loop context) still counts as
        // outside a loop.
        ("if (true) { break }", "'break' outside of a loop"),
        ("if (true) { continue }", "'continue' outside of a loop"),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        let tw_err = tw.expect_err(&format!("tree-walker should reject `{src}`"));
        let vm_err = vm.expect_err(&format!("VM should reject `{src}`"));
        assert!(
            tw_err.message.contains(expected),
            "tree-walker message for `{src}` was {:?}, expected to contain {expected:?}",
            tw_err.message
        );
        assert_eq!(
            tw_err.message, vm_err.message,
            "VM and tree-walker error messages diverge for `{src}`"
        );
    }
}

// ----- V3-T3: for-range loop (+ compiler scratch slots) -----------------------

#[tokio::test]
async fn vm_for_range_matches_treewalker() {
    // `for (i in start..end)` is half-open (EXCLUSIVE) on both engines: `i` runs
    // from `start` while `i < end`, the loop var rebinds each iteration, `break`
    // exits, `continue` runs the increment then re-tests. All byte-identical.
    let cases = [
        // basic ascending count: 0,1,2.
        "for (i in 0..3) { print(i) }",
        // empty when start == end.
        "for (i in 5..5) { print(i) }\nprint(\"done\")",
        // empty when start > end (i < end false up front).
        "for (i in 5..3) { print(i) }\nprint(\"done\")",
        // accumulate into an outer local: 1+2+3+4 = 10.
        "let sum = 0\nfor (i in 1..5) { sum = sum + i }\nprint(sum)",
        // break at i == 3 → 0,1,2.
        "for (i in 0..10) { if (i == 3) { break }\n print(i) }",
        // continue skips i == 2 (the increment still runs) → 0,1,3,4.
        "for (i in 0..5) { if (i == 2) { continue }\n print(i) }",
        // non-zero start.
        "for (i in 2..6) { print(i) }",
        // single iteration.
        "for (i in 7..8) { print(i) }",
        // bounds from local + arithmetic expressions (end evaluated once).
        "let lo = 1\nlet hi = 4\nfor (i in lo..hi) { print(i) }",
        // body with a block-scoped local (fresh slot each iteration).
        "for (i in 0..3) { let sq = i * i\n print(sq) }",
        // for between other statements: the stack must stay balanced.
        "print(\"start\")\nfor (i in 0..2) { print(i) }\nprint(\"end\")",
        // nested for-range: prints a grid of i*10 + j.
        "for (i in 0..2) { for (j in 0..3) { print(i * 10 + j) } }",
        // nested with inner break: inner stops at j == 2 each outer iter.
        "for (i in 0..2) { for (j in 0..5) { if (j == 2) { break }\n print(i * 10 + j) } }",
        // nested with inner continue: inner skips j == 1.
        "for (i in 0..2) { for (j in 0..3) { if (j == 1) { continue }\n print(i * 10 + j) } }",
        // trailing expression value after a loop (proves locals survive).
        "let last = 0\nfor (i in 0..4) { last = i }\nlast",
        // loop var reused as a read inside arithmetic.
        "for (i in 0..3) { print(i + 100) }",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_for_range_bounds_error_matches_treewalker() {
    // Non-number bounds raise the SAME Tier-2 panic (`for-range bounds must be
    // numbers`) at the SAME span (the START bound) on both engines. The VM emits a
    // CHECK_NUMBERS guard anchored at `start.span`, byte-identical to the
    // tree-walker's `Stmt::ForRange` eval check.
    for src in [
        // non-number END bound (start is a number → guard fires on end).
        "for (i in 0..\"x\") { print(i) }",
        // non-number START bound.
        "for (i in \"x\"..3) { print(i) }",
        // both non-number.
        "for (i in true..false) { print(i) }",
        // nil end.
        "for (i in 0..nil) { print(i) }",
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        match (tw, vm) {
            (Err(tw_err), Err(vm_err)) => {
                assert_eq!(
                    tw_err.message, vm_err.message,
                    "for-range bounds panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.message, vm_err.message
                );
                assert_eq!(
                    tw_err.message, "for-range bounds must be numbers",
                    "unexpected message for `{src}`: {:?}",
                    tw_err.message
                );
                assert_eq!(
                    tw_err.span, vm_err.span,
                    "for-range bounds panic span diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.span, vm_err.span
                );
            }
            (tw, vm) => panic!(
                "expected BOTH engines to error for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
            ),
        }
    }
}

#[tokio::test]
async fn both_engines_inclusive_range_agree() {
    // RANGES FEATURE, Phase 2 (+ Phase 4 direction): inclusive `..=` ranges
    // EVALUATE on BOTH engines in for-range and value position; with `step`
    // omitted the direction is inferred from the bounds, so `lo > hi` counts
    // down. Both engines must SUCCEED and produce byte-identical output — a real
    // both-accept parity assertion.
    for (src, expected) in [
        // for-range, inclusive: 1,2,3,4,5.
        ("for (i in 1..=5) { print(i) }", "1\n2\n3\n4\n5\n"),
        // value range, inclusive.
        ("print(1..=5)", "[1, 2, 3, 4, 5]\n"),
        // single-element inclusive (`5..=5` → `[5]`; cf. exclusive `5..5` → `[]`).
        ("print(5..=5)", "[5]\n"),
        ("print(5..5)", "[]\n"),
        // Phase 4: descending inclusive counts down (direction inferred).
        ("print(10..=1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]\n"),
        // exclusive `..` is unchanged.
        ("print(1..5)", "[1, 2, 3, 4]\n"),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(tw.is_ok(), "tree-walker should accept `{src}`, got {tw:?}");
        assert!(vm.is_ok(), "VM should accept `{src}`, got {vm:?}");
        assert_eq!(
            tw.as_deref().ok(),
            Some(expected),
            "tree-walker output wrong for `{src}`"
        );
        assert_eq!(
            tw.as_deref().ok(),
            vm.as_ref().map(|(out, _)| out.as_str()).ok(),
            "inclusive-range output diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn both_engines_step_iteration_agree() {
    // RANGES FEATURE, Phase 3: `step` iteration now EVALUATES on BOTH engines, with
    // the step's sign honored as the direction. Both engines must SUCCEED and
    // produce byte-identical output (a real both-accept parity assertion). Omitted
    // step stays ascending-only this phase (direction inference is Phase 4).
    for (src, expected) in [
        // ascending stepped for-range.
        ("for (i in 1..10 step 2) { print(i) }", "1\n3\n5\n7\n9\n"),
        // descending stepped for-range (negative sign drives the direction).
        ("for (i in 10..1 step -2) { print(i) }", "10\n8\n6\n4\n2\n"),
        // stepped value ranges (exclusive + inclusive).
        ("print(1..10 step 2)", "[1, 3, 5, 7, 9]\n"),
        ("print(1..=10 step 2)", "[1, 3, 5, 7, 9]\n"),
        // float step.
        ("print(0..=1 step 0.25)", "[0.0, 0.25, 0.5, 0.75, 1.0]\n"),
        // overshoot simply stops.
        ("print(1..10 step 100)", "[1]\n"),
        // Phase 4: omitted-step descending counts down (direction inferred).
        ("print(10..1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2]\n"),
        ("print(10..=1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]\n"),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(tw.is_ok(), "tree-walker should accept `{src}`, got {tw:?}");
        assert!(vm.is_ok(), "VM should accept `{src}`, got {vm:?}");
        assert_eq!(
            tw.as_deref().ok(),
            Some(expected),
            "tree-walker output wrong for `{src}`"
        );
        assert_eq!(
            tw.as_deref().ok(),
            vm.as_ref().map(|(out, _)| out.as_str()).ok(),
            "step-iteration output diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn both_engines_descending_bare_range_counts_down() {
    // RANGES FEATURE, Phase 4: the ONE changed existing behavior. With `step`
    // OMITTED the direction is inferred from the bounds, so a bare descending
    // range counts DOWN (was empty). Asserted byte-identical across engines, in
    // both for-range (lazy) and value (materialized) position, with the §3.5
    // truth-table guards (ascending unchanged, equal bounds empty/single).
    for (src, expected) in [
        // descending bare ranges count down (THE behavior change).
        ("for (i in 5..1) { print(i) }", "5\n4\n3\n2\n"),
        ("for (i in 5..=1) { print(i) }", "5\n4\n3\n2\n1\n"),
        ("print(10..1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2]\n"),
        ("print(10..=1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]\n"),
        // UNCHANGED: ascending bare ranges.
        ("print(1..5)", "[1, 2, 3, 4]\n"),
        ("print(1..=5)", "[1, 2, 3, 4, 5]\n"),
        // UNCHANGED: equal bounds.
        ("print(5..5)", "[]\n"),
        ("print(5..=5)", "[5]\n"),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(tw.is_ok(), "tree-walker should accept `{src}`, got {tw:?}");
        assert!(vm.is_ok(), "VM should accept `{src}`, got {vm:?}");
        assert_eq!(
            tw.as_deref().ok(),
            Some(expected),
            "tree-walker output wrong for `{src}`"
        );
        assert_eq!(
            tw.as_deref().ok(),
            vm.as_ref().map(|(out, _)| out.as_str()).ok(),
            "count-down output diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn both_engines_step_validation_panics_agree() {
    // RANGES FEATURE, Phase 3: a zero/non-finite step or a direction mismatch is a
    // Tier-2 panic. BOTH engines must panic AND produce the EXACT same message
    // (the interpolated `{step}`/`{end}` use the canonical number formatting both
    // engines share, so the strings are byte-identical).
    for (src, expected_msg) in [
        (
            "for (i in 1..10 step 0) {}",
            "step must be a finite, non-zero number",
        ),
        (
            "for (i in 1..10 step -2) {}",
            "step -2.0 moves away from end (10.0); range can never progress",
        ),
        (
            "for (i in 10..1 step 2) {}",
            "step 2.0 moves away from end (1.0); range can never progress",
        ),
        // value-position validation panics identically.
        (
            "print(1..10 step 0)",
            "step must be a finite, non-zero number",
        ),
        (
            "print(1..10 step -2)",
            "step -2.0 moves away from end (10.0); range can never progress",
        ),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        match (tw, vm) {
            (Err(tw_err), Err(vm_err)) => {
                assert_eq!(
                    tw_err.message, vm_err.message,
                    "step-validation panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.message, vm_err.message
                );
                assert_eq!(
                    tw_err.message, expected_msg,
                    "unexpected message for `{src}`: {:?}",
                    tw_err.message
                );
            }
            (tw, vm) => panic!(
                "expected BOTH engines to panic for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
            ),
        }
    }
}

#[tokio::test]
async fn vm_inclusive_match_pattern_matches_treewalker() {
    // Inclusive `..=` in a MATCH PATTERN is PRE-EXISTING, working functionality
    // (distinct from value/for ranges). Both engines must accept it and agree —
    // the Phase-1 step-rejection guard must NOT over-reject inclusive patterns.
    for src in [
        "let n = 5\nprint(match n { 1..=10 => \"in\", _ => \"out\" })",
        "let n = 15\nprint(match n { 1..=10 => \"in\", _ => \"out\" })",
        "let n = 10\nprint(match n { 1..=10 => \"in\", _ => \"out\" })",
        "let n = 5\nprint(match n { 1..10 => \"in\", _ => \"out\" })",
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(tw.is_ok(), "tree-walker should accept `{src}`, got {tw:?}");
        assert_eq!(
            tw.as_deref().ok(),
            vm.as_ref().map(|(out, _)| out.as_str()).ok(),
            "inclusive match-pattern output diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn both_engines_pattern_step_agree() {
    // RANGES FEATURE, Phase 5: `step` in a MATCH-RANGE pattern = strided membership
    // (spec §3.7), byte-identical on the VM and the tree-walker. Anchor is `start`,
    // so parity/offset depends on where the range begins. A plain (no-step) pattern
    // keeps its pre-existing in-bounds-only behavior (incl. a fractional subject).
    for (src, expected) in [
        // 3 ∈ {1,3,5,7,9}; 4 ∉.
        (
            "print(match 3 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        (
            "print(match 4 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // anchor 0 → even membership.
        (
            "print(match 4 { 0..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // out of bounds.
        (
            "print(match 11 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // exclusive vs inclusive end at the stride endpoint.
        (
            "print(match 10 { 0..10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        (
            "print(match 10 { 0..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // descending stepped pattern.
        (
            "print(match 8 { 10..=2 step -2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        (
            "print(match 9 { 10..=2 step -2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // float step membership (exact-equality, spec §3.8).
        (
            "print(match 0.5 { 0..=1 step 0.25 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        (
            "print(match 0.3 { 0..=1 step 0.25 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // PLAIN ASCENDING (no-step) patterns unchanged — incl. a FRACTIONAL
        // subject, which must still match purely on bounds (no stride test).
        ("print(match 5 { 1..=10 => \"in\", _ => \"out\" })", "in\n"),
        (
            "print(match 2.5 { 1..=10 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        (
            "print(match 15 { 1..=10 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // PLAIN DESCENDING (no-step) pattern — direction is now INFERRED from the
        // bounds (spec §3.1), so `10..=1` is the descending sequence range and
        // matches `[1,10]`. (Old behavior: `n>=lo && n<=hi` = `n>=10 && n<=1` =
        // NEVER, i.e. empty on BOTH engines. The change is engine-symmetric: VM and
        // tree-walker agreed before and agree now.)
        ("print(match 5 { 10..=1 => \"in\", _ => \"out\" })", "in\n"),
        ("print(match 1 { 10..=1 => \"in\", _ => \"out\" })", "in\n"),
        ("print(match 1 { 10..1 => \"in\", _ => \"out\" })", "out\n"), // exclusive low end
        (
            "print(match 11 { 10..=1 => \"in\", _ => \"out\" })",
            "out\n",
        ),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(tw.is_ok(), "tree-walker should accept `{src}`, got {tw:?}");
        assert!(vm.is_ok(), "VM should accept `{src}`, got {vm:?}");
        assert_eq!(
            tw.as_deref().ok(),
            Some(expected),
            "tree-walker output wrong for `{src}`"
        );
        assert_eq!(
            tw.as_deref().ok(),
            vm.as_ref().map(|(out, _)| out.as_str()).ok(),
            "pattern-step output diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn both_engines_pattern_step_validation_panics_agree() {
    // RANGES FEATURE, Phase 5: a stepped pattern runs the SAME shared validator as
    // iteration, so a `step 0` / direction-mismatch pattern PANICS with the EXACT
    // same message on both engines (byte-identical). This also proves the Phase-3
    // pattern-step REJECTION divergence (VM "empty expression statement" vs the
    // tree-walker guard) is GONE — both now ACCEPT and validate uniformly.
    for (src, expected_msg) in [
        (
            "print(match 5 { 1..=10 step 0 => 1, _ => 0 })",
            "step must be a finite, non-zero number",
        ),
        (
            "print(match 5 { 1..=10 step -2 => 1, _ => 0 })",
            "step -2.0 moves away from end (10.0); range can never progress",
        ),
        (
            "print(match 5 { 10..=1 step 2 => 1, _ => 0 })",
            "step 2.0 moves away from end (1.0); range can never progress",
        ),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        match (tw, vm) {
            (Err(tw_err), Err(vm_err)) => {
                assert_eq!(
                    tw_err.message, vm_err.message,
                    "pattern-step panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.message, vm_err.message
                );
                assert_eq!(
                    tw_err.message, expected_msg,
                    "unexpected message for `{src}`: {:?}",
                    tw_err.message
                );
            }
            (tw, vm) => panic!(
                "expected BOTH engines to panic for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
            ),
        }
    }
}

// ----- V3-T4: sync for-of (Array + Str snapshot iteration) --------------------

#[tokio::test]
async fn vm_for_of_matches_treewalker() {
    // `for (x of iterable)` SYNC for-of iterates a SNAPSHOT of an Array (its
    // elements) or a Str (its chars, each a 1-char string). The loop var rebinds
    // each iteration, `break` exits, `continue` advances to the next item. All
    // byte-identical to the tree-walker's `Stmt::ForOf` (for_await = false).
    let cases = [
        // array of numbers: 10,20,30.
        "for (x of [10, 20, 30]) { print(x) }",
        // string: each char as a 1-char string -> a,b,c.
        "for (c of \"abc\") { print(c) }",
        // RangeExpr iterable: builds the range ARRAY then iterates it -> 0,1,2.
        "for (x of 0..3) { print(x) }",
        // empty array: no output.
        "for (x of []) { print(x) }\nprint(\"done\")",
        // empty string: no output.
        "for (c of \"\") { print(c) }\nprint(\"done\")",
        // single-element array.
        "for (x of [42]) { print(x) }",
        // iterate over a NAME bound to an array (not just a literal).
        "let xs = [1, 2, 3]\nfor (x of xs) { print(x) }",
        // mixed-type elements render via the same Display as the tree-walker.
        "for (x of [1, \"two\", true, nil]) { print(x) }",
        // break at the second element -> 10.
        "for (x of [10, 20, 30]) { if (x == 20) { break }\n print(x) }",
        // continue skips x == 20 -> 10,30.
        "for (x of [10, 20, 30]) { if (x == 20) { continue }\n print(x) }",
        // accumulate into an outer local: 1+2+3+4 = 10.
        "let sum = 0\nfor (x of [1, 2, 3, 4]) { sum = sum + x }\nprint(sum)",
        // body with a block-scoped local (fresh slot each iteration).
        "for (x of [1, 2, 3]) { let sq = x * x\n print(sq) }",
        // for-of between other statements: the stack must stay balanced.
        "print(\"start\")\nfor (x of [1, 2]) { print(x) }\nprint(\"end\")",
        // nested for-of: a grid of i*10 + j over two arrays.
        "for (i of [0, 1]) { for (j of [0, 1, 2]) { print(i * 10 + j) } }",
        // nested with inner break: inner stops at j == 1 each outer iter.
        "for (i of [0, 1]) { for (j of [0, 1, 2]) { if (j == 1) { break }\n print(i * 10 + j) } }",
        // nested with inner continue: inner skips j == 1.
        "for (i of [0, 1]) { for (j of [0, 1, 2]) { if (j == 1) { continue }\n print(i * 10 + j) } }",
        // for-of over a string nested in a for-of over an array.
        "for (s of [\"ab\", \"cd\"]) { for (c of s) { print(c) } }",
        // trailing expression value after a loop (proves locals survive).
        "let last = 0\nfor (x of [5, 6, 7]) { last = x }\nlast",
        // loop var read inside arithmetic.
        "for (x of [1, 2, 3]) { print(x + 100) }",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_for_of_not_iterable_error_matches_treewalker() {
    // A non-Array, non-Str iterable raises the SAME Tier-2 panic (`value of type
    // {t} is not iterable`) at the SAME span (the iterable expression) on both
    // engines. CRUCIALLY this includes object/map/set, which are NOT iterable in
    // sync for-of (they hit the "not iterable" path, NOT element iteration) —
    // byte-identical to the tree-walker's `Stmt::ForOf`.
    for (src, expected) in [
        (
            "for (x of 5) { print(x) }",
            "value of type int is not iterable",
        ),
        (
            "for (x of true) { print(x) }",
            "value of type bool is not iterable",
        ),
        (
            "for (x of nil) { print(x) }",
            "value of type nil is not iterable",
        ),
        // An OBJECT is not iterable in sync for-of.
        (
            "for (x of {a: 1}) { print(x) }",
            "value of type object is not iterable",
        ),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        match (tw, vm) {
            (Err(tw_err), Err(vm_err)) => {
                assert_eq!(
                    tw_err.message, vm_err.message,
                    "for-of not-iterable panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.message, vm_err.message
                );
                assert_eq!(
                    tw_err.message, expected,
                    "unexpected message for `{src}`: {:?}",
                    tw_err.message
                );
                assert_eq!(
                    tw_err.span, vm_err.span,
                    "for-of not-iterable panic span diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.span, vm_err.span
                );
            }
            (tw, vm) => panic!(
                "expected BOTH engines to error for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
            ),
        }
    }
}

// ----- V3-T4b: `for (i in value)` routes to for-of (legacy `in` overload) ------

#[tokio::test]
async fn vm_for_in_over_value_matches_treewalker() {
    // The legacy parser OVERLOADS `for ... in ...`: `in` + a LITERAL `a..b` range
    // uses the lazy range loop; `in` over any OTHER value falls back to ForOf and
    // iterates the resulting value (src/parser.rs `Tok::In` arm). The VM must mirror
    // that overload: `in` + non-`RangeExpr` is a sync for-of, byte-identical to the
    // tree-walker. `..` itself produces a `Value::Array`, so `for (i in r)` where `r`
    // is a range VALUE iterates that array's elements.
    let cases = [
        // `in` over a NAME bound to a range VALUE -> iterate elements 0,1,2.
        "let r = 0..3\nfor (i in r) { print(i) }",
        // `in` over a NAME bound to an array literal -> 10,20.
        "let xs = [10, 20]\nfor (i in xs) { print(i) }",
        // `in` over a NAME bound to a string -> chars a,b,c.
        "let s = \"abc\"\nfor (c in s) { print(c) }",
        // `in` directly over an array literal (non-range expr) -> 1,2,3.
        "for (i in [1, 2, 3]) { print(i) }",
        // accumulate over a range value: 0+1+2+3+4 = 10 (mirrors examples/ranges.as).
        "let r = 0..5\nlet total = 0\nfor (i in r) { total = total + i }\nprint(total)",
        // REGRESSION: `in` + a LITERAL range still uses the lazy range loop -> 0,1,2.
        "for (i in 0..3) { print(i) }",
        // REGRESSION: empty range value -> no output.
        "let r = 0..0\nfor (i in r) { print(i) }\nprint(\"done\")",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_for_in_over_non_iterable_value_matches_treewalker() {
    // `in` over a non-`RangeExpr`, non-iterable value goes through the for-of path
    // and raises the SAME Tier-2 panic (`value of type {t} is not iterable`) at the
    // SAME span on both engines.
    for (src, expected) in [
        (
            "for (i in 5) { print(i) }",
            "value of type int is not iterable",
        ),
        (
            "let n = 5\nfor (i in n) { print(i) }",
            "value of type int is not iterable",
        ),
        (
            "for (i in true) { print(i) }",
            "value of type bool is not iterable",
        ),
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        match (tw, vm) {
            (Err(tw_err), Err(vm_err)) => {
                assert_eq!(
                    tw_err.message, vm_err.message,
                    "for-in not-iterable panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.message, vm_err.message
                );
                assert_eq!(
                    tw_err.message, expected,
                    "unexpected message for `{src}`: {:?}",
                    tw_err.message
                );
                assert_eq!(
                    tw_err.span, vm_err.span,
                    "for-in not-iterable panic span diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                    tw_err.span, vm_err.span
                );
            }
            (tw, vm) => panic!(
                "expected BOTH engines to error for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
            ),
        }
    }
}

// ----- V3-T5: ternary expression ---------------------------------------------

#[tokio::test]
async fn vm_ternary_matches_treewalker() {
    let cases = [
        // literal conditions select the correct branch.
        "print(true ? \"a\" : \"b\")",
        "print(false ? \"a\" : \"b\")",
        // computed condition.
        "let x = 5\nprint(x > 3 ? \"big\" : \"small\")",
        // nested / right-associative: `n == 1 ? "one" : (n == 2 ? "two" : "other")`.
        "let n = 2\nprint(n == 1 ? \"one\" : n == 2 ? \"two\" : \"other\")",
        // precedence form `a ? -b : c` — the `-1` is the then-branch.
        "let f = true\nprint(f ? -1 : 2)",
        // ternary as a sub-expression: the chosen value participates in `+`.
        "print((true ? 1 : 2) + 10)",
        // truthiness follows is_truthy (0 and "" are truthy; only nil/false falsy).
        "print(0 ? \"t\" : \"f\")",
        "print(nil ? \"t\" : \"f\")",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ----- V3-T6: control-flow multi-feature sync programs ------------------------
//
// These COMBINE the full V3 control-flow set (`if`/`else if`/`else`, `while` with
// `break`/`continue`, `for (i in a..b)`, `for (x of …)`, ternary) with the V1+V2
// base (locals, reassignment, arithmetic, arrays/objects, index/member, templates,
// short-circuit, `print`). The single-feature tests above prove each construct in
// isolation; these prove they compose correctly through the compiler + VM, byte-
// for-byte against the tree-walker. This is the V3 sync-subset gate.

#[tokio::test]
async fn vm_run_control_flow_multi_feature_programs() {
    let programs = [
        // (a) sum the squares of a range with a for-range loop, then re-sum the
        // same source array with a for-of loop — both must agree.
        "let squares = 0\nfor (i in 0..5) { squares = squares + i * i }\nlet src = 0..5\nlet again = 0\nfor (x of src) { again = again + x }\nprint(squares)\nprint(again)\nprint(len(src))",
        // (b) fizzbuzz-style: if / else if / else inside a for-range, with print.
        "for (i in 1..16) { if (i % 15 == 0) { print(\"fizzbuzz\") } else if (i % 3 == 0) { print(\"fizz\") } else if (i % 5 == 0) { print(\"buzz\") } else { print(i) } }",
        // (c) nested for-range building a flattened coordinate list via templates.
        "for (i in 0..3) { for (j in 0..3) { if (i == j) { print(`diag ${i},${j}`) } } }",
        // (d) while with break + continue accumulating into a local.
        "let i = 0\nlet acc = 0\nwhile (true) { i = i + 1\n if (i > 10) { break }\n if (i % 2 == 0) { continue }\n acc = acc + i }\nprint(acc)",
        // (e) ternary inside a loop selecting between two computed branches.
        "for (n of [1, 2, 3, 4, 5]) { print(n % 2 == 0 ? `${n} even` : `${n} odd`) }",
        // (f) for-of over an array of objects, member reads + arithmetic + a running max.
        "let users = [{name: \"a\", age: 30}, {name: \"b\", age: 12}, {name: \"c\", age: 45}]\nlet oldest = users[0]\nfor (u of users) { if (u.age > oldest.age) { oldest = u } }\nprint(oldest.name)\nprint(oldest.age)",
        // (g) nested if inside for-of with short-circuit guard + template.
        "let words = [\"hi\", \"\", \"world\", \"\"]\nlet shown = 0\nfor (w of words) { if (len(w) > 0 && shown < 5) { shown = shown + 1\n print(`#${shown}: ${w}`) } }\nprint(`total ${shown}`)",
        // (h) while loop computing a factorial, then an if on the result.
        "let n = 6\nlet f = 1\nlet k = 1\nwhile (k <= n) { f = f * k\n k = k + 1 }\nif (f > 100) { print(`${n}! = ${f} (big)`) } else { print(`${n}! = ${f}`) }",
        // (i) for-range over computed bounds with a ternary-driven step decision.
        "let lo = 2\nlet hi = lo + 5\nlet evens = 0\nlet odds = 0\nfor (i in lo..hi) { if (i % 2 == 0) { evens = evens + 1 } else { odds = odds + 1 } }\nprint(`evens=${evens} odds=${odds}`)",
        // (j) for-of over a string counting a class of chars, ternary in the print.
        "let s = \"banana\"\nlet count = 0\nfor (c of s) { if (c == \"a\") { count = count + 1 } }\nprint(count > 0 ? `found ${count}` : \"none\")",
        // (k) nested data: for-of over array of objects whose field is an array,
        // inner for-of over that array, index/member throughout.
        "let groups = [{tag: \"x\", vals: [1, 2]}, {tag: \"y\", vals: [3, 4, 5]}]\nfor (g of groups) { let s = 0\n for (v of g.vals) { s = s + v }\n print(`${g.tag}: ${s}`) }",
        // (l) while loop computing a running total + iteration count, then a
        // conditional summary via ternary on the count.
        "let total = 0\nlet i = 0\nwhile (i < 4) { total = total + i * 10\n i = i + 1 }\nprint(total)\nprint(i == 4 ? \"ok\" : \"bad\")",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_ternary_only_chosen_branch_evaluates() {
    // Side-effect proof: only the taken branch's `print` runs (the untaken branch
    // must NOT evaluate). Both engines print exactly the chosen branch's effect.
    let cases = [
        // condition false → only the else-branch print runs (`n`).
        "let x = 0\nlet r = x > 0 ? print(\"y\") : print(\"n\")",
        // condition true → only the then-branch print runs (`y`).
        "let x = 1\nlet r = x > 0 ? print(\"y\") : print(\"n\")",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ---- functions: CALL/RETURN + multi-frame + by-ref capture (V4-T3) -------

#[tokio::test]
async fn vm_recursion_factorial() {
    // Self-recursion: `fac` references its own name (a captured cell in the file
    // frame, an upvalue inside the body). 10! = 3628800.
    assert_vm_run_matches_treewalker(
        "fn fac(n) { if (n < 2) { return 1 }\n return n * fac(n - 1) }\nprint(fac(10))",
    )
    .await;
}

#[tokio::test]
async fn vm_forward_inter_fn_call() {
    // `a` calls `b` declared LATER: the late-binding cell (allocated nil at frame
    // entry, filled when `b`'s declaration runs) makes the forward reference work.
    assert_vm_run_matches_treewalker("fn a() { return b() + 1 }\nfn b() { return 7 }\nprint(a())")
        .await;
}

#[tokio::test]
async fn vm_mutual_recursion_even_odd() {
    assert_vm_run_matches_treewalker(
        "fn isEven(n) { if (n == 0) { return true }\n return isOdd(n - 1) }\n\
         fn isOdd(n) { if (n == 0) { return false }\n return isEven(n - 1) }\n\
         print(isEven(10))",
    )
    .await;
}

#[tokio::test]
async fn vm_capturing_closure_read() {
    // The arrow captures `x` from `make`'s frame BY REFERENCE (a cell); calling it
    // after `make` returned still reads the filled value.
    assert_vm_run_matches_treewalker(
        "fn make() { let x = 42\n return () => x }\nlet f = make()\nprint(f())",
    )
    .await;
}

#[tokio::test]
async fn vm_capturing_closure_mutate_counter() {
    // Mutable capture via the shared cell: each call mutates `c` through
    // SET_UPVALUE; the counter advances 1, 2, 3.
    assert_vm_run_matches_treewalker(
        "fn counter() { let c = 0\n return () => { c = c + 1\n return c } }\n\
         let inc = counter()\nprint(inc())\nprint(inc())\nprint(inc())",
    )
    .await;
}

#[tokio::test]
async fn vm_nested_calls_args_and_return_values() {
    // Nested calls, multiple args, return values composed.
    assert_vm_run_matches_treewalker(
        "fn add(a, b) { return a + b }\nfn mul(a, b) { return a * b }\n\
         print(add(mul(2, 3), add(4, 5)))",
    )
    .await;
}

#[tokio::test]
async fn vm_function_as_value_and_arg() {
    // A function passed as a value/argument and invoked through the parameter.
    assert_vm_run_matches_treewalker(
        "fn apply(f, x) { return f(x) }\nfn dbl(n) { return n * 2 }\nprint(apply(dbl, 21))",
    )
    .await;
}

#[tokio::test]
async fn vm_deep_recursion_matches_treewalker_at_modest_depth() {
    // Differential at a depth the TREE-WALKER survives even under the debug test
    // thread's small (~2 MiB) stack. The tree-walker recurses on the Rust stack
    // (`#[async_recursion]` eval, a large per-frame future), so it overflows at a
    // modest depth in debug tests (its documented "robust unbounded deep recursion"
    // non-goal) — and a stack overflow aborts the WHOLE test process, so the depth
    // is kept conservatively safe (20). sum(20) = 20 * 21 / 2 = 210; this still
    // drives 21 nested CALL/RETURN frames through the VM. The heap-bounded proof at
    // real depth is `vm_deep_recursion_is_heap_bounded` (VM-only, 50_000 deep).
    assert_vm_run_matches_treewalker(
        "fn sum(n) { if (n == 0) { return 0 }\n return n + sum(n - 1) }\nprint(sum(20))",
    )
    .await;
}

#[tokio::test]
async fn vm_deep_recursion_is_heap_bounded() {
    // VM-only: each CALL pushes a HEAP `CallFrame` and the Rust `run` loop stays
    // flat, so recursion is heap-bounded — the VM reaches a depth the tree-walker
    // (native-stack recursion) cannot. SP3 §B now caps logical recursion at
    // `MAX_CALL_DEPTH` (a clean catchable panic, NOT a SIGABRT). Prove heap-bounded
    // depth JUST UNDER the cap completes on the small (~2 MiB) test-thread stack —
    // 2900 nested CALL/RETURN frames the tree-walker could never reach there.
    // sum(2900) = 2900 * 2901 / 2 = 4_206_450.
    let src = "fn sum(n) { if (n == 0) { return 0 }\n return n + sum(n - 1) }\nprint(sum(2900))";
    let (vm_out, code) = ascript::vm_run_source(src).await.expect("vm ok");
    assert_eq!(code, None);
    assert_eq!(vm_out, "4206450\n", "deep VM recursion result (just under the cap)");
}

// ---- V4-T4: parameters — arity, rest, and type contracts ------------------
//
// These assert the VM enforces exact arity, rest collection, per-parameter type
// contracts, the rest element type, and the return-type contract BYTE-IDENTICALLY
// to the tree-walker — both the value path (matching stdout) AND the error path
// (matching the Tier-2 panic message AND span). The shared `check_call_args` /
// `check_type` / `contract_panic` core is the single source of truth, so a
// divergence here would be a real frame-setup bug in the VM CALL/RETURN.

/// Assert both engines FAIL identically for a FULL source program (not wrapped):
/// same Tier-2 panic message AND the same source span.
async fn assert_vm_run_error_matches_treewalker(src: &str) {
    let tw = ascript::run_source(src).await;
    let vm = ascript::vm_run_source(src).await;
    match (tw, vm) {
        (Err(tw_err), Err(vm_err)) => {
            assert_eq!(
                tw_err.message, vm_err.message,
                "panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, vm_err.message
            );
            assert_eq!(
                tw_err.span, vm_err.span,
                "panic span diverged for `{src}` (msg {:?})\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, tw_err.span, vm_err.span
            );
        }
        (tw, vm) => panic!(
            "expected BOTH engines to error for `{src}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
        ),
    }
}

#[tokio::test]
async fn vm_param_arity_ok_matches_treewalker() {
    assert_vm_run_matches_treewalker("fn f(a, b) { return a + b }\nprint(f(1, 2))").await;
    assert_vm_run_matches_treewalker("fn f() { return 42 }\nprint(f())").await;
    assert_vm_run_matches_treewalker("fn id(x) { return x }\nprint(id(\"hi\"))").await;
}

#[tokio::test]
async fn vm_param_arity_errors_match_treewalker() {
    // Too few / too many arguments — exact-arity panic, identical message + span.
    assert_vm_run_error_matches_treewalker("fn f(a, b) { return a + b }\nprint(f(1))").await;
    assert_vm_run_error_matches_treewalker("fn f(a, b) { return a + b }\nprint(f(1, 2, 3))").await;
    assert_vm_run_error_matches_treewalker("fn f() { return 1 }\nprint(f(1))").await;
    // The callee description in the message is the fn's name ("f"), not "function".
    assert_vm_run_error_matches_treewalker("fn one(a) { return a }\nprint(one())").await;
}

#[tokio::test]
async fn vm_rest_param_matches_treewalker() {
    // Rest collects the trailing args into an array (empty when none are passed).
    assert_vm_run_matches_treewalker("fn f(a, ...rest) { return a + len(rest) }\nprint(f(1))")
        .await;
    assert_vm_run_matches_treewalker(
        "fn f(a, ...rest) { return a + len(rest) }\nprint(f(1, 2, 3))",
    )
    .await;
    // A pure-rest function collects everything.
    assert_vm_run_matches_treewalker("fn f(...xs) { return len(xs) }\nprint(f())").await;
    assert_vm_run_matches_treewalker("fn f(...xs) { return len(xs) }\nprint(f(9, 8, 7))").await;
    // The rest binding is a real array value.
    assert_vm_run_matches_treewalker("fn f(...xs) { return xs }\nprint(f(1, 2, 3))").await;
}

#[tokio::test]
async fn vm_rest_param_too_few_fixed_args_matches_treewalker() {
    // Fewer than the fixed-param count → "expected at least N argument(s)".
    assert_vm_run_error_matches_treewalker("fn f(a, b, ...rest) { return a }\nprint(f(1))").await;
}

#[tokio::test]
async fn vm_typed_param_contracts_match_treewalker() {
    // A satisfied contract passes; a violated one panics identically (msg + span).
    assert_vm_run_matches_treewalker("fn f(n: number) { return n }\nprint(f(5))").await;
    assert_vm_run_error_matches_treewalker("fn f(n: number) { return n }\nprint(f(\"x\"))").await;
    // String / bool / optional contracts.
    assert_vm_run_matches_treewalker("fn g(s: string) { return s }\nprint(g(\"hi\"))").await;
    assert_vm_run_error_matches_treewalker("fn g(s: string) { return s }\nprint(g(1))").await;
    assert_vm_run_matches_treewalker("fn h(x: number?) { return x }\nprint(h(nil))").await;
    assert_vm_run_matches_treewalker("fn h(x: number?) { return x }\nprint(h(3))").await;
    assert_vm_run_error_matches_treewalker("fn h(x: number?) { return x }\nprint(h(\"x\"))").await;
}

#[tokio::test]
async fn vm_typed_rest_element_contract_matches_treewalker() {
    // `...xs: array<number>` per-element checks each trailing arg; a wrong element
    // raises the SAME contract panic on both engines.
    assert_vm_run_matches_treewalker(
        "fn f(...xs: array<number>) { return len(xs) }\nprint(f(1, 2, 3))",
    )
    .await;
    assert_vm_run_error_matches_treewalker(
        "fn f(...xs: array<number>) { return len(xs) }\nprint(f(1, \"x\"))",
    )
    .await;
}

#[tokio::test]
async fn vm_return_type_contract_matches_treewalker() {
    // A satisfied return contract passes; a violated one panics identically — and
    // crucially at the CALL-site span (not the `return` statement's span).
    assert_vm_run_matches_treewalker("fn f(): number { return 1 }\nprint(f())").await;
    assert_vm_run_error_matches_treewalker("fn f(): number { return \"x\" }\nprint(f())").await;
    // Falling off the end returns nil; a `: number` contract then fails
    // identically — and crucially the panic is anchored at the CALL-site span
    // (`f()`), exactly like the tree-walker's `run_body`, not the body's span.
    assert_vm_run_error_matches_treewalker("fn f(): number { let x = 1 }\nprint(f())").await;
    // A union return type accepts either arm.
    assert_vm_run_matches_treewalker(
        "fn f(b): number | string { if (b) { return 1 }\n return \"x\" }\nprint(f(true))",
    )
    .await;
    assert_vm_run_matches_treewalker(
        "fn f(b): number | string { if (b) { return 1 }\n return \"x\" }\nprint(f(false))",
    )
    .await;
    // `: nil` return type — falling off the end yields nil and is accepted
    // (prints "hi" on both engines); explicitly returning a non-nil value
    // (`return 5`) is contract-rejected identically (same panic msg + span).
    // (Calls are wrapped in `print(...)` so the panic anchors at the wrapper
    // call-site span on both engines, matching the other return-type cases.)
    assert_vm_run_matches_treewalker("fn f(): nil { print(\"hi\") }\nprint(f())").await;
    assert_vm_run_error_matches_treewalker("fn f(): nil { return 5 }\nprint(f())").await;
    // `: nil` as a union member is also accepted.
    assert_vm_run_matches_treewalker("fn g(): number | nil { return nil }\nprint(g())").await;
}

#[tokio::test]
async fn vm_fn_type_param_contract_matches_treewalker() {
    // `: fn` parameter contract — a closure passed to a `: fn` param satisfies the
    // contract (exercising the check_type Closure-accepts-`fn` fix end-to-end), and
    // the result is byte-identical to the tree-walker. This is the proof that the
    // CST type parser now lowers `fn` to Type::Fn AND that the VM contract check
    // accepts a Closure for it.
    assert_vm_run_matches_treewalker(
        "fn apply(g: fn, x) { return g(x) }\nprint(apply((n) => n * 2, 5))",
    )
    .await;
    // A NON-function passed to a `: fn` param is contract-rejected identically
    // (same Tier-2 panic message + span on both engines).
    assert_vm_run_error_matches_treewalker(
        "fn apply(g: fn, x) { return g(x) }\nprint(apply(7, 5))",
    )
    .await;
}

// ---- call_value bridge: native code invokes a VM closure (V4-T5) ----------
//
// `recover(fn)` is the testable end-to-end surface for the `native → VM` bridge:
// it is a BARE builtin (so the VM compiler can emit the call without `import`,
// which lands in a later VM slice) and its native implementation invokes the
// closure argument through `Interp::call_value`, which routes a `Value::Closure`
// back into `Vm::call_value` to run it on a fresh Fiber. The whole point is that
// the closure runs on the VM and produces byte-identical observable output to the
// tree-walker. The HOF callers (`array.map`/`filter`/sort comparator/middleware)
// exercise the IDENTICAL `call_value` primitive; their end-to-end differential
// gate lands once the VM compiler grows `import` (module-namespaced calls), and
// the primitive itself is pinned directly by the `Vm::call_value` unit tests in
// `src/vm/run.rs`.

#[tokio::test]
async fn vm_recover_invokes_vm_closure_success_matches_treewalker() {
    // recover(() => v) → [v, nil] on both engines: the closure runs on the VM via
    // the bridge and its result is wrapped identically.
    assert_vm_run_matches_treewalker("print(recover(() => 1))").await;
    assert_vm_run_matches_treewalker("print(recover(() => 1 + 2))").await;
    assert_vm_run_matches_treewalker("print(recover(() => \"ok\"))").await;
    assert_vm_run_matches_treewalker("print(recover(() => nil))").await;
}

#[tokio::test]
async fn vm_recover_closure_captures_outer_var_matches_treewalker() {
    // A closure that captures an outer `k` is invoked by native `recover`; the
    // captured upvalue cell travels with the closure value, so the bridge sees it.
    assert_vm_run_matches_treewalker("let k = 10\nprint(recover(() => k + 5))").await;
}

#[tokio::test]
async fn vm_recover_catches_closure_panic_matches_treewalker() {
    // A panic raised inside the VM-run closure surfaces back through the bridge as
    // a `Control::Panic`, which `recover` converts into `[nil, err]` IDENTICALLY
    // on both engines (same error message round-tripped). Here the closure indexes
    // a 1-element array out of bounds (a runtime Tier-2 panic).
    assert_vm_run_matches_treewalker("let a = [1]\nprint(recover(() => a[9]))").await;
}

#[tokio::test]
async fn vm_recover_closure_calls_another_fn_matches_treewalker() {
    // The closure passed to recover calls a user `fn`; that call (closure → VM
    // function) and the recover bridge (native → VM closure) compose, and the
    // result is byte-identical.
    assert_vm_run_matches_treewalker("fn dbl(x) { return x * 2 }\nprint(recover(() => dbl(21)))")
        .await;
}

#[tokio::test]
async fn vm_panic_through_closure_escapes_recover_message_matches_treewalker() {
    // A panic inside a VM closure that is NOT wrapped by recover propagates all the
    // way out and aborts the program with the SAME diagnostic MESSAGE on both
    // engines. We assert message-equality (not span) here on purpose: the panic
    // originates from an index-out-of-bounds *inside a closure body*, whose span
    // is subject to the orthogonal VM span-table off-by-one tracked separately (the
    // "VM diagnostic spans must use trivia-trimmed code spans" audit) — it is NOT a
    // call_value-bridge concern. The bridge's job, verified here, is that the panic
    // SURFACES out of the VM closure to the top level identically.
    let src = "let bad = (x) => x[9]\nprint(bad([0]))";
    let tw = ascript::run_source(src)
        .await
        .expect_err("tree-walker errors");
    let vm = ascript::vm_run_source(src).await.expect_err("vm errors");
    assert_eq!(
        tw.message, vm.message,
        "panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
        tw.message, vm.message
    );
}

// ----- V4-T6: function-heavy multi-feature sync programs -----------------------
//
// These COMBINE the full V4 function set (definitions, calls, recursion,
// mutual/forward refs, closures capturing + mutating state, functions returning
// functions, rest params, typed params + contracts, the native→VM `recover`
// bridge) with the V1..V3 base (locals, arithmetic, arrays/objects, index/member,
// templates, short-circuit/`??`, `if`/`for`/`while`, ternary, `print`). The
// single-feature function tests above prove each construct in isolation; these
// prove they compose correctly through the compiler + VM, byte-for-byte against
// the tree-walker. This is the V4 sync-subset gate. NEVER weaken the byte-
// identical assertion: a divergence here is a real VM/compiler bug.
//
// NOTE on recursion depth: the tree-walker (the differential oracle) recurses on
// the Rust stack and overflows at modest depth under the debug test thread's small
// stack — a stack overflow aborts the WHOLE test process — so any recursion here
// stays well within what the tree-walker survives (e.g. ackermann is kept to
// `ack(2, 2)`; the heap-bounded deep-recursion proof is the VM-only test above).

#[tokio::test]
async fn vm_run_function_heavy_multi_feature_programs() {
    let programs = [
        // (a) classic recursion: fibonacci computed for 0..10, each printed.
        "fn fib(n) { if (n < 2) { return n }\n return fib(n - 1) + fib(n - 2) }\n\
         for (i in 0..10) { print(fib(i)) }",
        // (b) recursion via the Euclidean algorithm (gcd), two cases.
        "fn gcd(a, b) { if (b == 0) { return a }\n return gcd(b, a % b) }\n\
         print(gcd(48, 36))\nprint(gcd(17, 5))",
        // (c) ackermann — nested non-trivial recursion (kept SMALL: ack(2,2)=7 is
        // well within the tree-walker's stack).
        "fn ack(m, n) { if (m == 0) { return n + 1 }\n if (n == 0) { return ack(m - 1, 1) }\n\
         return ack(m - 1, ack(m, n - 1)) }\nprint(ack(2, 2))",
        // (d) mutual recursion (isEven/isOdd), printed via a template over a range.
        "fn isEven(n) { if (n == 0) { return true }\n return isOdd(n - 1) }\n\
         fn isOdd(n) { if (n == 0) { return false }\n return isEven(n - 1) }\n\
         for (i in 0..6) { print(`${i} even=${isEven(i)}`) }",
        // (e) higher-order via the native→VM `recover` bridge + closures calling
        // closures (a closure returned by `make` invoked through another closure).
        "fn make(base) { return (x) => base + x }\nlet add10 = make(10)\nlet add100 = make(100)\n\
         print(recover(() => add10(5))[0])\nprint(add100(add10(1)))",
        // (f) closure capturing + MUTATING shared state (an accumulator): each call
        // mutates the captured `total` through the shared cell.
        "fn acc() { let total = 0\n return (n) => { total = total + n\n return total } }\n\
         let a = acc()\nprint(a(5))\nprint(a(10))\nprint(a(-3))",
        // (g) function returning a function (adder factory), two independent closures.
        "fn adder(x) { return (y) => x + y }\nlet inc = adder(1)\nlet plus5 = adder(5)\n\
         print(inc(41))\nprint(plus5(plus5(0)))",
        // (h) a fn computing over an array with a loop, returning an object summary.
        "fn stats(xs) { let sum = 0\n let max = xs[0]\n for (x of xs) { sum = sum + x\n\
         if (x > max) { max = x } }\n return { sum: sum, max: max, n: len(xs) } }\n\
         let s = stats([3, 7, 2, 9, 4])\nprint(`sum=${s.sum} max=${s.max} n=${s.n}`)",
        // (i) a fn computing over OBJECTS (member reads), called in a for-of loop.
        "fn pointNorm(p) { return p.x * p.x + p.y * p.y }\n\
         let pts = [{x: 3, y: 4}, {x: 1, y: 1}]\nfor (p of pts) { print(pointNorm(p)) }",
        // (j) deeply NESTED closures (three levels), fully applied in one chain.
        "fn outer(a) { return (b) => { return (c) => a + b + c } }\nprint(outer(1)(2)(3))",
        // (k) a fn with REST params aggregating its trailing args via a loop.
        "fn sumAll(...xs) { let t = 0\n for (x of xs) { t = t + x }\n return t }\n\
         print(sumAll())\nprint(sumAll(1, 2, 3, 4, 5))",
        // (l) TYPED params + an optional param with a `??` default (default-ish).
        "fn greet(name: string, greeting: string?) { let g = greeting ?? \"Hello\"\n\
         return `${g}, ${name}!` }\nprint(greet(\"Ada\", nil))\nprint(greet(\"Lin\", \"Hi\"))",
        // (m) higher-order: functions passed as arguments and COMPOSED (f∘g), order
        // matters, so both orders are printed.
        "fn apply(f, g, x) { return f(g(x)) }\nfn dbl(n) { return n * 2 }\nfn inc(n) { return n + 1 }\n\
         print(apply(dbl, inc, 10))\nprint(apply(inc, dbl, 10))",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ---------------------------------------------------------------------------
// V5-T1: per-iteration loop-var / loop-body cell freshness.
//
// The tree-walker creates a FRESH binding per loop iteration, so a closure that
// captures the loop variable (or a `let` declared in the loop body) sees THAT
// iteration's value. The VM must allocate a fresh cell per iteration for each
// captured cell-slot so the byte-identical differential gate holds.
// ---------------------------------------------------------------------------

/// The confirmed divergence target: a closure capturing the for-RANGE loop var.
/// Tree-walker prints 0,1,2; the pre-fix shared-cell VM printed 1,2,2.
#[tokio::test]
async fn vm_for_range_loop_var_capture_is_per_iteration() {
    let src = "let prev = nil\n\
               for (i in 0..3) {\n  if (prev != nil) { print(prev()) }\n  prev = () => i\n}\n\
               print(prev())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("0\n1\n2\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// for-OF variant: a closure capturing the for-of loop var.
#[tokio::test]
async fn vm_for_of_loop_var_capture_is_per_iteration() {
    let src = "let prev = nil\n\
               for (x of [10, 20, 30]) {\n  if (prev != nil) { print(prev()) }\n  prev = () => x\n}\n\
               print(prev())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("10\n20\n30\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// while variant: a captured `let` declared INSIDE the loop body.
#[tokio::test]
async fn vm_while_body_let_capture_is_per_iteration() {
    let src = "let prev = nil\nlet i = 0\n\
               while (i < 3) {\n  let j = i\n  if (prev != nil) { print(prev()) }\n  \
               prev = () => j\n  i = i + 1\n}\n\
               print(prev())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("0\n1\n2\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// A captured `let` declared inside a for-range BODY (not just the loop var).
#[tokio::test]
async fn vm_for_range_body_let_capture_is_per_iteration() {
    let src = "let prev = nil\n\
               for (i in 0..3) {\n  let doubled = i * 2\n  if (prev != nil) { print(prev()) }\n  \
               prev = () => doubled\n}\n\
               print(prev())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("0\n2\n4\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// Nested loops capturing BOTH loop vars: the inner closure created in the
/// PREVIOUS (a, b) iteration is invoked at the START of the next one, so it must
/// report the exact (outer, inner) pair from the iteration that created it. With
/// shared cells the inner `b` (and `a` across the inner-loop boundary) would be
/// stale; per-iteration fresh cells make each closure pin its own pair.
#[tokio::test]
async fn vm_nested_loop_capture_both_vars() {
    let src = "let prev = nil\n\
               for (a in 0..2) {\n  for (b in 0..2) {\n    \
               if (prev != nil) { print(prev()) }\n    prev = () => a * 10 + b\n  }\n}\n\
               print(prev())";
    assert_vm_run_matches_treewalker(src).await;
}

/// Regression: a NON-captured for-range loop var (no closure) still iterates
/// correctly — no behavior change for the common case.
#[tokio::test]
async fn vm_non_captured_loop_var_unchanged() {
    let src = "let total = 0\nfor (i in 0..5) { total = total + i }\nprint(total)";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("10\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// Regression: a closure capturing an OUTER (non-loop) variable still sees that
/// variable's CURRENT value (shared cell across iterations), as in V4. Only
/// loop-local cells are refreshed per iteration.
#[tokio::test]
async fn vm_outer_capture_still_shared() {
    let src = "let count = 0\nlet bump = nil\n\
               for (i in 0..3) {\n  count = count + 1\n  bump = () => count\n}\n\
               print(bump())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("3\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

// ---- V6-T1: the `?` propagate operator (PROPAGATE opcode) ----------------

/// `expr?` on a success pair (`err == nil`) yields the `value`, and the
/// enclosing function returns normally. Result pairs render as `[value, err]`
/// (e.g. `[6, nil]`), shared with the tree-walker's `Value::Display`.
#[tokio::test]
async fn vm_propagate_success_matches_treewalker() {
    let src = "fn g(): Result<number> { return [5, nil] }\n\
               fn f(): Result<number> { let v = g()?\n return [v + 1, nil] }\n\
               print(f())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("[6, nil]\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// `expr?` on a failure pair (`err != nil`) early-returns `[nil, err]` from the
/// enclosing function — that propagated pair becomes `f`'s return value, printed
/// as `[nil, "boom"]`. (Untyped `f` so the propagated pair is not subject to a
/// return-type contract — the contract path's call-site span is a separate audit,
/// task #132; this isolates PROPAGATE's own unwind.)
#[tokio::test]
async fn vm_propagate_failure_returns_pair_matches_treewalker() {
    let src = "fn g() { return [nil, \"boom\"] }\n\
               fn f() { let v = g()?\n return [v, nil] }\n\
               print(f())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("[nil, \"boom\"]\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// A failing `?` short-circuits the REST of the function: a `print` after the
/// `?` does NOT run (the function early-returned the `[nil, err]` pair).
#[tokio::test]
async fn vm_propagate_short_circuits_rest_of_function_matches_treewalker() {
    let src = "fn g() { return [nil, \"stop\"] }\n\
               fn f() {\n  let a = g()?\n  print(\"not reached\")\n  return [a, nil]\n}\n\
               print(f())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("[nil, \"stop\"]\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// Multiple `?` in a row: the first failing one short-circuits, so a later `?`
/// (and any code after it) never runs.
#[tokio::test]
async fn vm_propagate_chain_matches_treewalker() {
    let src = "fn a() { return [1, nil] }\n\
               fn b() { return [nil, \"e2\"] }\n\
               fn c() { return [3, nil] }\n\
               fn f() {\n  let x = a()?\n  let y = b()?\n  let z = c()?\n\
               \n  return [x + y + z, nil]\n}\n\
               print(f())";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("[nil, \"e2\"]\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// A top-level `?` on a failure pair ends the PROGRAM (the root frame is the
/// function boundary): a `print` after it does NOT run, mirroring the
/// tree-walker's top-level `Control::Propagate => Ok` (the pair is discarded).
#[tokio::test]
async fn vm_propagate_top_level_ends_program_matches_treewalker() {
    let src = "let v = [nil, \"e\"]?\nprint(\"after\")";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        (String::new(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// A top-level `?` on a SUCCESS pair binds the value and execution continues.
#[tokio::test]
async fn vm_propagate_top_level_success_matches_treewalker() {
    let src = "let v = [42, nil]?\nprint(v)";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("42\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// `expr?` where `expr` is NOT a 2-element `[value, err]` array is a Tier-2 panic
/// with the exact message + span identical to the tree-walker's `ExprKind::Try`.
#[tokio::test]
async fn vm_propagate_non_pair_panic_matches_treewalker() {
    assert_vm_run_error_matches_treewalker("let x = 5?").await;
    // A 3-element array is not a Result pair either.
    assert_vm_run_error_matches_treewalker("let x = [1, 2, 3]?").await;
    // A string is not an array.
    assert_vm_run_error_matches_treewalker("let x = \"nope\"?").await;
}

// ---- V6-T2: the `!` force-unwrap operator (UNWRAP opcode) ----------------
//
// `expr!` mirrors the tree-walker's `ExprKind::Unwrap` exactly: the operand must
// be a 2-element `[value, err]` Result pair. If `err == nil` the result is the
// `value`; otherwise it raises a RECOVERABLE `Control::Panic` carrying the
// original error's message (via `error_message`) anchored at the `!` expr span.

/// Success: a `[value, nil]` pair unwraps to `value`.
#[tokio::test]
async fn vm_unwrap_success_matches_treewalker() {
    let src = "fn g() { return [42, nil] }\nprint(g()!)";
    assert_eq!(
        ascript::vm_run_source(src).await.expect("vm ok"),
        ("42\n".to_string(), None)
    );
    assert_vm_run_matches_treewalker(src).await;
}

/// Failure with a STRING error: the recoverable panic carries `error_message` of
/// the string (`"boom"` → `boom`, no quotes), at the `!` expr span. Both engines
/// must produce the identical panic message + span.
#[tokio::test]
async fn vm_unwrap_failure_string_err_panic_matches_treewalker() {
    assert_vm_run_error_matches_treewalker("fn g() { return [nil, \"boom\"] }\nprint(g()!)").await;
}

/// Failure with an ERROR-OBJECT error: `error_message` reads the object's
/// `message` field (`{message: "x"}` → `x`). Identical panic on both engines.
#[tokio::test]
async fn vm_unwrap_failure_error_object_panic_matches_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "fn g() { return [nil, {message: \"x\"}] }\nprint(g()!)",
    )
    .await;
}

/// `expr!` where `expr` is NOT a 2-element `[value, err]` array is a Tier-2 panic
/// ("the ! operator requires a Result pair [value, err]") with the exact message
/// + span identical to the tree-walker's `ExprKind::Unwrap`.
#[tokio::test]
async fn vm_unwrap_non_pair_panic_matches_treewalker() {
    assert_vm_run_error_matches_treewalker("let x = 5!").await;
    assert_vm_run_error_matches_treewalker("let x = [1, 2, 3]!").await;
    assert_vm_run_error_matches_treewalker("let x = \"nope\"!").await;
}

/// `!` inside `recover`: the unwrap raises a RECOVERABLE panic, so `recover`
/// catches it and yields `[nil, err]` — byte-identical on both engines.
#[tokio::test]
async fn vm_unwrap_inside_recover_matches_treewalker() {
    let src = "fn g() { return [nil, \"boom\"] }\nprint(recover(() => g()!))";
    assert_vm_run_matches_treewalker(src).await;
    // The success path through recover round-trips the value as well.
    let ok = "fn g() { return [7, nil] }\nprint(recover(() => g()!))";
    assert_vm_run_matches_treewalker(ok).await;
}

// ===========================================================================
// V6-T4: DIAGNOSTICS-PARITY GATE + #132 span audit
// ===========================================================================
//
// Every Tier-2 panic the VM raises MUST be byte-identical to the tree-walker's
// in BOTH message AND source span (start/end byte offsets, hence line/col). The
// `assert_vm_run_error_matches_treewalker` helper asserts exactly that on a FULL,
// UNWRAPPED program — no `print(...)` wrapper. Previously some of these forms
// diverged because the compiler anchored an instruction at a CST node's RAW
// `text_range()` (which includes leading whitespace/newline trivia), so a panic
// from a bare leading statement (e.g. `f()`, `a[9]`, `a.foo`, `a + 1`) pointed
// one byte early vs the tree-walker (which anchors at the trivia-trimmed code
// span). The compiler now uses `node_code_span` for every PANICKING op
// (CALL, GET_INDEX, GET_PROP/GET_PROP_OPT, arithmetic/comparison, NEG/NOT,
// RANGE, PROPAGATE, UNWRAP, the for-range CHECK_NUMBERS, the for-of iterator),
// closing the #132 off-by-one.

/// The HEADLINE #132 case: a bare `f()` statement that PANICS (here an arity
/// mismatch raised by the CALL instruction) must anchor at the CALL expression's
/// trivia-trimmed code span, NOT at the leading newline. Before the fix the VM
/// span started one byte early (on the `\n`).
#[tokio::test]
async fn vm_diag_bare_call_panic_span_matches_treewalker_132() {
    // Bare call on line 2 — its CST node's raw range begins at the preceding '\n'.
    assert_vm_run_error_matches_treewalker("fn f(a) { return a }\nf()").await;
    // A blank line before the call widens the leading trivia further.
    assert_vm_run_error_matches_treewalker("fn f(a) { return a }\n\nf()").await;
    // Leading spaces before the call (indented bare statement).
    assert_vm_run_error_matches_treewalker("fn f(a) { return a }\n   f()").await;
    // The exact program from the V6 plan: a `!` deep in a function, called bare.
    // (The panic here originates at the `!`, but the program is the canonical
    // #132 repro shape and must still match byte-for-byte.)
    assert_vm_run_error_matches_treewalker("fn f() { return [nil, \"e\"]! }\nf()").await;
}

/// Diagnostics parity for every PANICKING op, in its BARE (unwrapped) form — the
/// forms that previously diverged on span because of leading-trivia anchoring.
/// Each asserts identical message AND span on both engines.
#[tokio::test]
async fn vm_diagnostics_parity_bare_forms() {
    let cases = [
        // bad `?` — not a Result pair (PROPAGATE)
        "5?",
        "let x = 5?",
        // bad `!` — not a Result pair (UNWRAP)
        "5!",
        "let x = 5!",
        // index out of bounds (GET_INDEX), bare leading statement
        "[1, 2][9]",
        "let a = [1, 2]\na[9]",
        // member read of nil (GET_PROP), bare leading statement
        "let a = nil\na.foo",
        // negate a non-number (NEG) — operand-anchored
        "-(true)",
        // binary type error (ADD/comparison), bare leading statement
        "let a = true\na + 1",
        "true + 1",
        "1 < \"x\"",
        // for-of over a non-iterable (the iterator-snapshot panic)
        "for (x of 5) {}",
        // for-range non-number bounds (CHECK_NUMBERS)
        "for (i in 1..\"x\") {}",
        // range value with a non-number bound (RANGE op)
        "let a = \"x\"\nlet r = 1..a",
    ];
    for src in cases {
        assert_vm_run_error_matches_treewalker(src).await;
    }
}

// ----- Part A: panic unwinding + recover boundary ---------------------------

/// An UNCAUGHT panic deep in a call chain unwinds every VM frame and surfaces at
/// the driver byte-identically to the tree-walker (message + span). This proves
/// the `Control::Panic` returned by an inner frame propagates out through every
/// `Vm::run`/`return_from_frame` boundary (frames/cells dropped on the way) to
/// `vm_run_source`, which maps it to the same `AsError` the tree-walker reports.
#[tokio::test]
async fn vm_uncaught_panic_deep_in_call_chain_matches_treewalker() {
    // a → b → c, and `c` force-unwraps a failure pair: the recoverable panic is
    // NOT recovered anywhere, so it unwinds all three frames to the driver.
    let src = "fn c() { return [nil, \"deep\"]! }\n\
               fn b() { return c() }\n\
               fn a() { return b() }\n\
               a()";
    assert_vm_run_error_matches_treewalker(src).await;
    // A type error deep in the chain (arithmetic on a non-number) unwinds too.
    let src2 = "fn c(x) { return x + 1 }\n\
                fn b(x) { return c(x) }\n\
                fn a(x) { return b(x) }\n\
                a(true)";
    assert_vm_run_error_matches_treewalker(src2).await;
}

/// A panic deep in a call chain CAUGHT by an OUTER `recover` becomes `[nil, err]`
/// — the recover boundary runs each callback on its OWN fresh Fiber (via the
/// native→VM bridge), and a `Control::Panic` returned from that Fiber is turned
/// into a Result pair. Byte-identical stdout on both engines.
#[tokio::test]
async fn vm_recover_catches_panic_deep_in_call_chain_matches_treewalker() {
    // The unwrap panic in `c` unwinds b→a, but the whole chain runs inside a
    // recover callback, so recover catches it and yields `[nil, {message:"deep"}]`.
    let src = "fn c() { return [nil, \"deep\"]! }\n\
               fn b() { return c() }\n\
               fn a() { return b() }\n\
               let r = recover(() => a())\n\
               print(r[0])\n\
               print(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;

    // A non-unwrap Tier-2 panic (type error) caught by recover. The message comes
    // from `error_message`/the panic's own message; both engines agree.
    let src2 = "fn boom(x) { return x + 1 }\n\
                let r = recover(() => boom(true))\n\
                print(r[0])\n\
                print(type(r[1]))";
    assert_vm_run_matches_treewalker(src2).await;

    // recover on a SUCCESS path round-trips the value as `[value, nil]`.
    let ok = "fn a() { return 99 }\n\
              let r = recover(() => a())\n\
              print(r[0])\n\
              print(r[1])";
    assert_vm_run_matches_treewalker(ok).await;
}

/// A panic raised from inside a NATIVE higher-order callback (the callback is a
/// VM closure the native builtin invokes via the native→VM bridge) must surface
/// as the same `Control::Panic` on both engines. The reachable native HOF in the
/// VM's compiled subset is `recover` itself (a bare-call builtin that re-enters
/// the VM to run its closure argument on a fresh Fiber); method-style HOFs like
/// `array.map(...)` are a V9 deferral and not yet compilable. Here a NESTED
/// `recover` proves the bridge cleanly composes: the INNER recover's callback
/// panics (an unrecoverable, NON-pair `?`/`!` is recoverable, so we use a real
/// uncaught form) — the inner recover catches it into a pair, and the OUTER
/// recover sees the inner one's success, so both engines print identically.
#[tokio::test]
async fn vm_panic_inside_native_hof_callback_matches_treewalker() {
    // The inner recover's callback force-unwraps a failure pair (a RECOVERABLE
    // panic raised from inside the native `recover` re-entering the VM). The inner
    // recover catches it → `[nil, err]`; the outer recover wraps THAT success.
    let src = "fn boom() { return [nil, \"inner\"]! }\n\
               let outer = recover(() => recover(() => boom()))\n\
               print(outer[0][0])\n\
               print(outer[0][1].message)\n\
               print(outer[1])";
    assert_vm_run_matches_treewalker(src).await;

    // A callback that itself calls another VM function which panics: the panic
    // unwinds the inner function frame AND out of the native `recover` frame,
    // caught by recover into a pair. Proves multi-frame unwind across the bridge.
    let src2 = "fn deep() { return [nil, \"x\"]! }\n\
                fn mid() { return deep() }\n\
                let r = recover(() => mid())\n\
                print(r[0])\n\
                print(r[1].message)";
    assert_vm_run_matches_treewalker(src2).await;
}

// ----- V6-T5: error-model multi-feature sync programs -------------------------
//
// These COMBINE the full V6 error model (`?` propagate chains, `!` force-unwrap
// on success AND failure, `recover` wrapping computations that may panic, error
// objects `{message: …}`, `Ok`/`Err`, nested recover) with the V1..V5 base
// (locals, arithmetic, arrays/objects, index/member READS, templates, short-
// circuit/`??`, `if`/`for`/`while`, ternary, `fn`/arrows/closures, recursion).
// The single-feature error-model tests above prove each construct in isolation;
// these prove they compose correctly through the compiler + VM, byte-for-byte
// against the tree-walker. This is the V6 sync-subset gate. NEVER weaken the
// byte-identical assertion: a divergence here is a real VM/compiler bug.
//
// NOTE: every snippet here resolves to a CLEAN value path (no escaping panic) so
// the program completes and its stdout is compared; the panic PATH is covered by
// the error-parity helper in the dedicated error tests below.

#[tokio::test]
async fn vm_run_error_model_multi_feature_programs() {
    let programs = [
        // (a) "safe divide" returning a Result pair, chained via `?` through a
        // second function; both the success and the error branch are printed.
        "fn safeDiv(a, b) { if (b == 0) { return [nil, \"divide by zero\"] }\n return [a / b, nil] }\n\
         fn compute(a, b, c) { let x = safeDiv(a, b)?\n let y = safeDiv(x, c)?\n return [y, nil] }\n\
         let good = compute(100, 5, 2)\nprint(good[0])\nprint(good[1])\n\
         let bad = compute(100, 0, 2)\nprint(bad[0])\nprint(bad[1])",
        // (b) `?` propagation chain through THREE functions; the middle one fails,
        // so the outer one short-circuits and returns the propagated pair.
        "fn a() { return [10, nil] }\nfn b() { return [nil, \"b-failed\"] }\nfn c() { return [30, nil] }\n\
         fn pipeline() { let x = a()?\n let y = b()?\n let z = c()?\n return [x + y + z, nil] }\n\
         let r = pipeline()\nprint(r[0])\nprint(r[1])",
        // (c) `!` unwrap SUCCESS inside an expression: a `[value, nil]` pair force-
        // unwraps to the value, threaded through arithmetic + a template.
        "fn parse(n) { return [n * 2, nil] }\nlet v = parse(21)!\nprint(`v = ${v}`)\nprint(v + 1)",
        // (d) `!` unwrap FAILURE inside `recover`: the unwrap panics (recoverable),
        // recover catches it into `[nil, err]`, and the error message is read back.
        "fn load(ok) { if (ok) { return [\"data\", nil] }\n return [nil, \"not found\"] }\n\
         let r = recover(() => load(false)!)\nprint(r[0])\nprint(r[1].message)\n\
         let ok = recover(() => load(true)!)\nprint(ok[0])\nprint(ok[1])",
        // (e) recover wrapping a computation that MAY panic (out-of-bounds index),
        // with the index chosen by a runtime condition; both branches exercised.
        "fn at(xs, i) { return recover(() => xs[i]) }\nlet data = [10, 20, 30]\n\
         let inb = at(data, 1)\nprint(inb[0])\nprint(inb[1])\n\
         let oob = at(data, 99)\nprint(oob[0])\nprint(oob[1] != nil)",
        // (f) mixing `?`/`!` with control flow + closures: a fn loops over inputs,
        // uses `?` to bail on the first failure, returns the accumulated total.
        "fn check(n) { if (n < 0) { return [nil, \"negative\"] }\n return [n, nil] }\n\
         fn sumChecked(xs) { let total = 0\n for (x of xs) { let v = check(x)?\n total = total + v }\n\
         return [total, nil] }\n\
         let okR = sumChecked([1, 2, 3])\nprint(okR[0])\nprint(okR[1])\n\
         let errR = sumChecked([1, -2, 3])\nprint(errR[0])\nprint(errR[1])",
        // (g) error OBJECTS `{message: …}`: a fn returns an Err with a structured
        // error; `!` inside recover surfaces the object, whose `.message` is read.
        "fn validate(age) { if (age < 18) { return [nil, {message: \"too young\", code: 403}] }\n\
         return [age, nil] }\n\
         let r = recover(() => validate(15)!)\nprint(r[1].message)\nprint(r[1].code)\n\
         let okV = validate(21)\nprint(okV[0])",
        // (h) NESTED recover: the inner recover catches a panic into a pair; the
        // outer recover wraps the inner's SUCCESS, so the outer error slot is nil.
        "fn boom() { return [nil, \"inner-err\"]! }\n\
         let outer = recover(() => recover(() => boom()))\n\
         print(outer[0][0])\nprint(outer[0][1].message)\nprint(outer[1])",
        // (i) `??` defaulting on a propagated/unwrapped value + ternary classifying
        // the outcome: combine the Result model with short-circuit + conditional.
        "fn lookup(key) { if (key == \"x\") { return [42, nil] }\n return [nil, \"missing\"] }\n\
         let hit = lookup(\"x\")\nlet miss = lookup(\"y\")\n\
         let hv = hit[0] ?? -1\nlet mv = miss[0] ?? -1\n\
         print(hv)\nprint(mv)\n\
         print(hit[1] == nil ? \"hit ok\" : \"hit err\")\nprint(miss[1] == nil ? \"miss ok\" : \"miss err\")",
        // (j) `Ok`/`Err` builtins (the canonical pair constructors) flowing through
        // a `?` chain + recover, with the `.message` of the Err read back.
        "fn step(n) { if (n == 0) { return Err(\"zero\") }\n return Ok(100 / n) }\n\
         fn run(n) { let v = step(n)?\n return Ok(v + 1) }\n\
         let good = run(4)\nprint(good[0])\nprint(good[1])\n\
         let bad = run(0)\nprint(bad[0])\nprint(bad[1].message)",
        // (k) recover around a RECURSIVE function that unwraps a failure pair deep
        // in the recursion: the recoverable panic unwinds every frame to recover.
        "fn descend(n) { if (n == 0) { return [nil, \"bottom\"]! }\n return descend(n - 1) }\n\
         let r = recover(() => descend(5))\nprint(r[0])\nprint(r[1].message)",
        // (l) a closure capturing an outer var, returning a Result; `?` propagates
        // its failure out of an enclosing fn; the success path is also shown.
        "fn makeChecker(limit) { return (n) => { if (n > limit) { return [nil, \"over limit\"] }\n\
         return [n, nil] } }\n\
         let under10 = makeChecker(10)\n\
         fn doubleIfOk(n) { let v = under10(n)?\n return [v * 2, nil] }\n\
         let okR = doubleIfOk(4)\nprint(okR[0])\nprint(okR[1])\n\
         let errR = doubleIfOk(20)\nprint(errR[0])\nprint(errR[1])",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

/// Error-model snippets whose panic PATH ESCAPES to the driver (not recovered):
/// both engines must abort with the IDENTICAL Tier-2 panic message AND span.
/// These complement the value-path programs above by pinning the diagnostics
/// parity of the error model when a `?`-non-pair / `!`-failure is NOT caught.
#[tokio::test]
async fn vm_run_error_model_uncaught_panic_programs() {
    let cases = [
        // `!` on a failure pair, uncaught, deep in a `?`-style call chain.
        "fn inner() { return [nil, \"kaboom\"]! }\n\
         fn outer() { return inner() }\n\
         print(outer())",
        // `!` on a NON-pair value (a number) — the "requires a Result pair" panic.
        "fn bad() { return 7! }\nprint(bad())",
        // `?` on a NON-pair value (a 3-element array) — the propagate non-pair panic.
        "fn bad() { let x = [1, 2, 3]?\n return x }\nprint(bad())",
        // `!` on an error OBJECT pair, uncaught: the panic carries `error_message`
        // of the object (its `.message` field), surfacing to the driver identically.
        "fn v() { return [nil, {message: \"invalid\"}]! }\nprint(v())",
    ];
    for src in cases {
        assert_vm_run_error_matches_treewalker(src).await;
    }
}

// ---- async / await (V7: eager-spawn + AWAIT, model 2a) -------------------------
//
// The risk-concentration slice. Each program drives a top-level `await` (the
// tree-walker runs top-level `await` directly — see `examples/async.as`); the VM
// driver (`vm_run_source`) drains the LocalSet the SAME way as `run_source`
// (`local.run_until(...).await; local.await;`), so an `async fn`'s eagerly-spawned
// task completes and its output is captured identically. Single-threaded LocalSet
// ⇒ deterministic ordering ⇒ byte-identical stdout on both engines.

#[tokio::test]
async fn vm_async_await_basic_matches_treewalker() {
    let programs = [
        // Simplest: call an async fn, await its result, print it.
        "async fn work() { return 1 }\nprint(await work())",
        // Await stored in a let, then printed.
        "async fn work() { return 42 }\nlet r = await work()\nprint(r)",
        // Async fn returning a string.
        "async fn greet() { return \"hi\" }\nprint(await greet())",
        // Async fn that takes an arg and uses it.
        "async fn dbl(n) { return n * 2 }\nprint(await dbl(21))",
        // Async arrow (expression body) — also is_async.
        "let g = async (n) => n + 1\nprint(await g(9))",
        // Async arrow, paren-less single param.
        "let h = async x => x - 1\nprint(await h(8))",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_await_non_future_is_identity_matches_treewalker() {
    // `await` on a non-future is identity (back-compat: `await 5 == 5`), on both
    // engines.
    for src in [
        "print(await 5)",
        "print(await \"x\")",
        "print(await (1 + 2))",
        "print(await true)",
        "let x = await nil\nprint(type(x))",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_await_in_arithmetic_and_multiple_matches_treewalker() {
    let programs = [
        // Await inside arithmetic, parenthesized.
        "async fn two() { return 2 }\nprint((await two()) + 10)",
        // Multiple awaits of the SAME async fn (two distinct spawned tasks).
        "async fn one() { return 1 }\nprint((await one()) + (await one()))",
        // Await feeding another async call's argument.
        "async fn inc(n) { return n + 1 }\nprint(await inc(await inc(0)))",
        // A sequence of async calls preserving deterministic ordering: each prints
        // as it is awaited, so stdout order is fixed and identical on both engines.
        "async fn step(label, n) { print(label)\n return n }\n\
         let a = await step(\"a\", 1)\n\
         let b = await step(\"b\", 2)\n\
         print(a + b)",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_async_panic_resurfaces_at_await_matches_treewalker() {
    // A panic raised in an `async fn` body crosses the spawned-task boundary and
    // RE-EMERGES at the awaiting site with the IDENTICAL message + span on both
    // engines (`!` on a failure pair is a recoverable Tier-2 panic; uncaught here,
    // it escapes to the driver).
    let panic_cases = [
        // `!` on an error pair inside an async fn, awaited and uncaught.
        "async fn boom() { return [nil, \"e\"]! }\nprint(await boom())",
        // `!` on a NON-pair value inside an async fn.
        "async fn bad() { return 7! }\nprint(await bad())",
        // Error-object pair: the panic carries `error_message` (the `.message`).
        "async fn v() { return [nil, {message: \"invalid\"}]! }\nprint(await v())",
    ];
    for src in panic_cases {
        assert_vm_run_error_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_async_contract_violation_surfaces_lazily_at_await() {
    // Async arity/contract errors surface LAZILY: the spawned task runs the
    // arity/contract check (`check_call_args`) when it is driven, the error resolves
    // into the SharedFuture, and it re-emerges at the `await` site — byte-identical
    // message + span on both engines.
    let lazy_cases = [
        // Param type contract violated (string passed to `n: number`), awaited.
        "async fn f(n: number) { return n }\nprint(await f(\"x\"))",
        // Too few args to an async fn, awaited.
        "async fn g(a, b) { return a + b }\nprint(await g(1))",
        // Too many args to an async fn, awaited.
        "async fn h(a) { return a }\nprint(await h(1, 2))",
        // Return-type contract violated in an async fn, awaited.
        "async fn r(): number { return \"nope\" }\nprint(await r())",
    ];
    for src in lazy_cases {
        assert_vm_run_error_matches_treewalker(src).await;
    }
}

// ----- V7-T4: cancel-on-drop (the M17 leak class) on the VM -------------------

#[tokio::test]
async fn vm_unawaited_async_call_is_cancelled_like_treewalker() {
    // M17 invariant: an `async fn` whose `Value::Future` is dropped WITHOUT being
    // awaited is CANCELLED (its task aborted), not orphaned — its side effect does
    // NOT run. The tree-walker does this too (its `SharedFuture` is cancel-on-drop
    // via AbortHandle); the VM reuses the EXACT same `SharedFuture` machinery in
    // the `Op::Call` async arm. We prove byte-identical behavior on both engines
    // for a program with an observable side effect (a `print` in an un-awaited
    // async body). This is the import-free analogue of M17's
    // `unawaited_async_call_is_cancelled` (which uses `time.sleep` — V12 on the
    // VM). Concretely: `worked` must NOT appear; only `main` is printed.
    // NOTE: we assert the BARE un-awaited call (the true M17 leak class: the
    // `Value::Future` is dropped at the end of its expression statement). The
    // distinct case of a future *held* in a local until program end
    // (`let f = work()`) is covered by `vm_held_future_drains_identically_to_treewalker`
    // (#147): both engines drain it at end-of-program, so its body runs identically.
    let cases = [
        // Bare un-awaited call: the future is dropped at the end of the statement.
        "async fn work() { print(\"worked\") }\nwork()\nprint(\"main\")\n",
        // Same, but with an internal await point before the side effect — the task
        // is aborted at the await suspension, so the side effect still never runs.
        "async fn work() { await 0\n print(\"worked\") }\nwork()\nprint(\"main\")\n",
    ];
    for src in cases {
        let tw = ascript::run_source(src).await.expect("tree-walker ok");
        let (vm, code) = ascript::vm_run_source(src).await.expect("vm ok");
        assert_eq!(code, None);
        assert!(
            !tw.contains("worked"),
            "tree-walker should cancel the un-awaited call: {tw:?}"
        );
        assert_eq!(
            tw, vm,
            "VM cancel-on-drop diverged from tree-walker for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

#[tokio::test]
async fn vm_held_future_drains_identically_to_treewalker() {
    // #147: a future HELD in a local until program end (not the bare cancel-on-drop
    // case) whose body awaits then prints. Both engines drain spawned tasks at
    // end-of-program (`local.run_until(..).await; local.await;` in src/lib.rs), so the
    // body runs on BOTH — byte-identical. (The neighboring test covers the bare
    // un-awaited cancel-on-drop case.)
    let src = "async fn work() { await 0\n print(\"worked\") }\nlet f = work()\nprint(\"main\")\n";
    let tw = ascript::run_source(src).await.expect("tree-walker ok");
    let (vm, _) = ascript::vm_run_source(src).await.expect("vm ok");
    let (gen, _) = ascript::vm_run_source_generic(src)
        .await
        .expect("generic vm ok");
    assert_eq!(tw, vm, "specialized VM diverged from tree-walker");
    assert_eq!(tw, gen, "generic VM diverged from tree-walker");
    assert_eq!(tw, "main\nworked\n");
}

#[tokio::test]
async fn vm_unawaited_async_loop_stays_bounded_and_matches_treewalker() {
    // A tight loop that spawns async calls WITHOUT awaiting them must stay
    // memory-bounded (each un-awaited future is dropped → its task cancelled) and
    // must NOT hang. We assert byte-identical output to the tree-walker (both
    // cancel; the only observable output is the trailing `done`) AND, on the VM,
    // assert the in-flight high-water mark stays bounded by `INFLIGHT_YIELD_CAP`'s
    // cooperative-yield reaping (the M17 leak guard, mirrored from the interp's
    // `unawaited_async_loop_keeps_inflight_bounded`).
    let src = "async fn work(n) { return n }\n\
               let i = 0\n\
               while (i < 5000) {\n  work(i)\n  i = i + 1\n}\n\
               print(\"done\")\n";
    let tw = ascript::run_source(src).await.expect("tree-walker ok");
    let (vm, code) = ascript::vm_run_source(src).await.expect("vm ok");
    assert_eq!(code, None);
    assert_eq!(tw, "done\n");
    assert_eq!(
        tw, vm,
        "VM un-awaited loop diverged from tree-walker\n  tw: {tw:?}\n  vm: {vm:?}"
    );
    // The VM-internal in-flight high-water-mark boundedness assertion (the M17
    // leak guard) lives in `src/vm/run.rs`
    // (`unawaited_async_loop_keeps_inflight_bounded_on_the_vm`) where the Vm's
    // shared `Interp::max_inflight` is reachable.
}

// ----- V7-T4: extra hand-written async snippets (byte-identical) --------------

#[tokio::test]
async fn vm_async_chained_and_control_flow_matches_treewalker() {
    let cases = [
        // Chained awaits across several async fns (each awaited in turn).
        "async fn a() { return 1 }\n\
         async fn b() { return 2 }\n\
         async fn c() { return 3 }\n\
         print((await a()) + (await b()) + (await c()))",
        // An async fn awaiting ANOTHER async fn's result inside its own body.
        "async fn inner() { return 10 }\n\
         async fn outer() { let x = await inner()\n return x + 5 }\n\
         print(await outer())",
        // Mixing await with control flow (if/else) and a closure.
        "async fn pick(flag) { if (flag) { return 100 } else { return 200 } }\n\
         let choose = (b) => b\n\
         print(await pick(choose(true)))\n\
         print(await pick(choose(false)))",
        // Await inside a loop, accumulating across iterations.
        "async fn dbl(n) { return n * 2 }\n\
         let total = 0\n\
         let i = 1\n\
         while (i <= 3) { total = total + (await dbl(i))\n i = i + 1 }\n\
         print(total)",
        // An async fn returning a Result pair, awaited then `?`-propagated.
        "async fn fetch() { return [42, nil] }\n\
         fn use(): number { return (await fetch())? }\n\
         print(use())",
        // An async fn returning a Result pair, awaited then `!`-force-unwrapped.
        "async fn fetch() { return [42, nil] }\n\
         print((await fetch())!)",
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ---- V8: generators (fn* / yield / next / close) ------------------------
//
// A VM generator is a Suspended Fiber (see `src/coro.rs` `GenImpl::Vm`): calling
// a `fn*` closure builds a not-started Fiber and returns a `Value::Generator`;
// `gen.next(v)` resumes it to the next `Op::Yield` (or completion → nil), and
// `next(v)` injects `v` as the suspended `yield` expression's value. Every case
// below asserts byte-identical stdout against the tree-walker.

#[tokio::test]
async fn vm_generator_basic_yields_then_done() {
    // The yielded values 1,2,3, then nil/done. next() returns the value (or nil
    // at completion) — matching the tree-walker's next() return shape.
    let src = "fn* g() { yield 1\n yield 2\n yield 3 }\nlet it = g()\nprint(it.next())\nprint(it.next())\nprint(it.next())\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_value_injection() {
    // The value `next(5)` passes in becomes the result of `yield 1`, so `x == 5`
    // and the second yield produces 15.
    let src = "fn* g() { let x = yield 1\n yield x + 10 }\nlet it = g()\nprint(it.next())\nprint(it.next(5))";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_first_next_ignores_input() {
    // The very first next(v) only starts the body and ignores `v` (first-next
    // semantics) — identical to the tree-walker.
    let src = "fn* g() { let x = yield 1\n yield x }\nlet it = g()\nprint(it.next(99))\nprint(it.next(7))";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_early_close() {
    // After one value, close() stops the generator; subsequent next() returns nil.
    let src = "fn* g() { yield 1\n yield 2 }\nlet it = g()\nprint(it.next())\nit.close()\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_empty_first_next_is_nil() {
    // An empty generator's first next() is the done sentinel (nil).
    let src = "fn* g() {}\nlet it = g()\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_return_value_is_discarded() {
    // A generator's `return x` value is DISCARDED — next() returns nil at
    // completion, not the body's return value. Mirror the tree-walker.
    let src = "fn* g() { yield 1\n return 42 }\nlet it = g()\nprint(it.next())\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_computed_sequence() {
    // A generator over a computed sequence: yield the running square.
    let src = "fn* squares(n) { let i = 0\n while (i < n) { yield i * i\n i = i + 1 } }\nlet it = squares(4)\nprint(it.next())\nprint(it.next())\nprint(it.next())\nprint(it.next())\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_params_and_closure_capture() {
    // A generator capturing an outer variable and using a param.
    let src = "let base = 100\nfn* g(step) { yield base + step\n yield base + step * 2 }\nlet it = g(5)\nprint(it.next())\nprint(it.next())\nprint(it.next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_generator_yield_used_as_statement() {
    // `yield` as a bare statement (its injected value discarded by the POP) —
    // exercises the statement-expression stack balance for Op::Yield.
    let src = "fn* g() { yield 1\n yield 2\n yield 3 }\nlet it = g()\nlet total = 0\ntotal = total + it.next()\ntotal = total + it.next()\ntotal = total + it.next()\nprint(total)";
    assert_vm_run_matches_treewalker(src).await;
}

// ---- V8-T4/T5: for-of / for-await over generators + async generators ------
//
// The tree-walker (`src/interp.rs`) only iterates a GENERATOR via `for await`
// (`exec_for_await` → `g.resume`); a SYNC `for (x of gen)` is the "not iterable"
// panic (sync for-of snapshots only Array/Str). The VM mirrors this exactly:
// `GET_ITER`/`ITER_NEXT` drive a generator lazily for `for await`, while the sync
// `ITER_SNAPSHOT` panics on a generator. break/early-return close the generator.

#[tokio::test]
async fn vm_sync_for_of_over_generator_is_not_iterable() {
    // SYNC `for (x of gen)` is NOT iterable in the tree-walker — generators are
    // driven only by `for await`. Both engines raise the same Tier-2 panic.
    let src = "fn* g() { yield 1\n yield 2\n yield 3 }\nfor (x of g()) { print(x) }";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_over_sync_generator() {
    // `for await (x of gen)` drives the generator via resume until done.
    let src = "fn* g() { yield 1\n yield 2 }\nfor await (x of g()) { print(x) }";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_over_three_yields() {
    let src = "fn* g() { yield 1\n yield 2\n yield 3 }\nfor await (x of g()) { print(x) }";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_over_async_generator() {
    // An async generator BOTH awaits internally AND yields: resume drives the
    // backing Fiber through Op::Await before producing the yielded value.
    let src = "async fn pick() { return 5 }\nasync fn* g() { let a = await pick()\n yield a\n yield a + 1 }\nfor await (x of g()) { print(x) }";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_generator_with_looping_body() {
    // A generator whose body is a loop computing each yielded value.
    let src = "fn* range2(n) { for (i in 0..n) { yield i * 10 } }\nfor await (x of range2(3)) { print(x) }";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_break_closes_generator() {
    // `break` out of a for-await over a generator stops iteration (and closes the
    // generator, byte-identically to the tree-walker).
    let src = "fn* g() { yield 1\n yield 2\n yield 3 }\nfor await (x of g()) { print(x)\n if (x == 2) { break } }\nprint(\"done\")";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_continue_skips() {
    // `continue` advances to the next yielded value.
    let src = "fn* g() { yield 1\n yield 2\n yield 3\n yield 4 }\nfor await (x of g()) { if (x == 2) { continue }\n print(x) }";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_empty_generator() {
    // An empty generator yields nothing: the body never runs.
    let src = "fn* g() {}\nfor await (x of g()) { print(x) }\nprint(\"after\")";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_over_non_iterable_panics() {
    // `for await` over a plain number is the "not async-iterable" Tier-2 panic.
    let src = "for await (x of 5) { print(x) }";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_for_await_accumulates_into_outer() {
    // The loop body mutates an outer local; the result survives the loop.
    let src = "fn* g() { yield 10\n yield 20\n yield 30 }\nlet sum = 0\nfor await (x of g()) { sum = sum + x }\nprint(sum)";
    assert_vm_run_matches_treewalker(src).await;
}

// ---- classes (V9-T1): decl, instances, fields, methods, self ------------
//
// Construction convention (verified against examples/typed_fields.as,
// examples/oop.as): positional args are passed to `init(...)`; fields are
// assigned via `self.field = arg` inside `init`. Field defaults apply BEFORE
// `init` runs. The differential harness compares VM vs tree-walker stdout (and
// panic message+span) byte-for-byte; the construction/dispatch is mirrored
// exactly from the tree-walker (`construct`/`invoke_method`/`read_member`/
// `set_member`).

#[tokio::test]
async fn vm_class_basic_method_and_fields() {
    // Two typed fields set in init; a method reading them via self.
    let src = "class Point {\n  x: number\n  y: number\n  fn init(x, y) { self.x = x\n self.y = y }\n  fn sum() { return self.x + self.y }\n}\nlet p = Point(3, 4)\nprint(p.sum())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_field_read_via_member() {
    // Read a field directly off the instance (not through a method).
    let src = "class Point {\n  x: number\n  y: number\n  fn init(x, y) { self.x = x\n self.y = y }\n}\nlet p = Point(10, 20)\nprint(p.x)\nprint(p.y)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_field_defaults_apply() {
    // A defaulted field (`role`) and an optional field (`nickname`) — construct
    // without those args; the default applies, the optional stays nil. Mirrors
    // examples/typed_fields.as.
    let src = "class User {\n  id: number\n  name: string\n  nickname: string?\n  role: string = \"guest\"\n  fn init(id, name) { self.id = id\n self.name = name }\n}\nlet u = User(1, \"Ada\")\nprint(u.id)\nprint(u.name)\nprint(u.nickname)\nprint(u.role)";
    assert_vm_run_matches_treewalker(src).await;
}

/// Build a class whose single field `x: <ty>` has the computed `default` source,
/// then exercise it through BOTH the construct path (`C()`) and the `.from` path
/// (`C.from({})`), asserting the VM's stdout matches the tree-walker byte-for-byte.
/// `prelude` declares any module-scope names the default references (e.g. a const).
///
/// This is the cst_default_expr-completeness oracle: a computed field default must
/// evaluate identically on the VM (whose `.from`/typed-parse path reads the lowered
/// `ast::Expr`, and whose construct path runs the compiled thunk) and the
/// tree-walker (which shares `validate_into`). Before the lowering was completed, a
/// non-trivial default form failed `compile_source` itself, breaking BOTH paths.
async fn assert_field_default_matches(prelude: &str, ty: &str, default: &str, print_expr: &str) {
    // Construct path: `C().<print_expr>`.
    let construct = format!(
        "{prelude}\nclass C {{ x: {ty} = {default}\n  fn init() {{}} }}\nprint(C().{print_expr})"
    );
    assert_vm_run_matches_treewalker(&construct).await;
    // `.from` path: `C.from({{}}).<print_expr>` (the missing field falls back to the
    // default, which `validate_into` evaluates from the lowered `ast::Expr`).
    let from = format!(
        "{prelude}\nclass C {{ x: {ty} = {default}\n  fn init() {{}} }}\nprint(C.from({{}}).{print_expr})"
    );
    assert_vm_run_matches_treewalker(&from).await;
}

#[tokio::test]
async fn vm_field_default_binary_arithmetic() {
    // `= A + B` and `= n * 2` — binary arithmetic defaults (the reviewer's category).
    assert_field_default_matches("", "number", "10 + 5", "x").await;
    assert_field_default_matches("let n = 7", "number", "n * 2", "x").await;
    assert_field_default_matches("", "number", "2 ** 3 % 5", "x").await;
}

#[tokio::test]
async fn vm_field_default_string_concat() {
    // `= PREFIX + "x"` — the exact divergence the reviewer reported.
    assert_field_default_matches("let PREFIX = \"pre\"", "string", "PREFIX + \"x\"", "x").await;
}

#[tokio::test]
async fn vm_field_default_comparison_and_equality() {
    assert_field_default_matches("let n = 3", "bool", "n > 0", "x").await;
    assert_field_default_matches("let n = 3", "bool", "n == 3", "x").await;
    assert_field_default_matches("let n = 3", "bool", "n != 4", "x").await;
}

#[tokio::test]
async fn vm_field_default_logical_and_nullish() {
    assert_field_default_matches("let a = true\nlet b = \"y\"", "any", "a && b", "x").await;
    assert_field_default_matches("let a = false\nlet b = \"y\"", "any", "a || b", "x").await;
    assert_field_default_matches("", "any", "nil ?? \"d\"", "x").await;
}

#[tokio::test]
async fn vm_field_default_index() {
    // `= arr[0]` — index read default.
    assert_field_default_matches("let arr = [10, 20, 30]", "number", "arr[1]", "x").await;
}

#[tokio::test]
async fn vm_field_default_ternary() {
    // `= cond ? a : b` — ternary default (lazy branches).
    assert_field_default_matches("let on = true", "number", "on ? 1 : 2", "x").await;
    assert_field_default_matches("let on = false", "number", "on ? 1 : 2", "x").await;
}

#[tokio::test]
async fn vm_field_default_template() {
    // Template default with an interpolated module const and a nested expression.
    assert_field_default_matches("let n = 5", "string", "`n=${n}!`", "x").await;
    assert_field_default_matches("let xs = [1, 2]", "string", "`v=${xs[0] + 10}`", "x").await;
}

#[tokio::test]
async fn vm_field_default_range_exclusive() {
    // `= a..b` — exclusive range default (eager `array<number>`).
    assert_field_default_matches("", "array<number>", "1..4", "x").await;
}

#[tokio::test]
async fn vm_field_default_optmember_try_unwrap_await() {
    // Optional-member, `?` propagate, `!` unwrap, and `await` defaults.
    assert_field_default_matches("let o = {a: 7}", "number", "o?.a", "x").await;
    assert_field_default_matches("fn f() { return [3, nil] }", "number", "f()?", "x").await;
    assert_field_default_matches("fn f() { return [4, nil] }", "number", "f()!", "x").await;
    assert_field_default_matches("fn g() { return 8 }", "number", "await g()", "x").await;
}

#[tokio::test]
async fn vm_field_default_assignment() {
    // Assignment defaults (plain and compound) — desugared to `target = (target OP
    // value)` exactly like the legacy parser; the captured mutable updates.
    assert_field_default_matches("let g = 0", "number", "(g = 5)", "x").await;
    assert_field_default_matches("let g = 2", "number", "(g += 5)", "x").await;
}

#[tokio::test]
async fn vm_field_default_spread() {
    // Spread defaults in array/object/call literals (the tree-walker accepts these;
    // they were previously rejected by `cst_default_expr`).
    assert_field_default_matches("let ys = [1, 2]", "array<number>", "[...ys, 3]", "x").await;
    assert_field_default_matches("let o = {a: 1}", "object", "{...o, b: 2}", "x").await;
    assert_field_default_matches(
        "fn f(a, b) { return a + b }\nlet ar = [1, 2]",
        "number",
        "f(...ar)",
        "x",
    )
    .await;
}

#[tokio::test]
async fn vm_field_default_arrow_and_match() {
    // Arrow and match defaults embed statement/pattern subtrees; lowered by
    // re-parsing the node text through the legacy front-end. Construct + `.from`.
    assert_field_default_matches("let base = 10", "fn", "(n) => n + base", "x(5)").await;
    assert_field_default_matches("", "string", "match 1 { 1 => \"one\", _ => \"z\" }", "x").await;
    assert_field_default_matches("", "string", "match 2 { 1 => \"one\", _ => \"z\" }", "x").await;
}

#[tokio::test]
async fn vm_field_default_range_inclusive_and_stepped() {
    // SP1 Phase E (E1 audit): the `cst_default_expr` lowering covers BOTH the
    // inclusive `..=` boundary and the signed `step` modifier as field defaults
    // (the tree-walker materializes the same `ExprKind::Range`). Exercise both the
    // construct path and the `.from` path — they read the SAME lowered default.
    assert_field_default_matches("", "array<number>", "1..=4", "x").await;
    assert_field_default_matches("", "array<number>", "0..=10 step 5", "x").await;
    assert_field_default_matches("", "array<number>", "5..=1 step -2", "x").await;
}

#[tokio::test]
async fn vm_field_default_yield_rejected_symmetrically() {
    // SP1 Phase E (E1): a `yield` field default is rejected by BOTH engines (it is
    // never valid outside a generator body). The VM rejects it at COMPILE time in
    // `cst_default_expr` (a specialized message); the tree-walker rejects it when it
    // EVALUATES the default. The messages/spans legitimately differ (compile-time
    // vs runtime), but the OBSERVABLE outcome is symmetric: both error, neither runs
    // the program. Assert the symmetry of the rejection (both `Err`), not the exact
    // text — mirroring the documented asymmetry note in `cst_default_expr`.
    for default in ["yield 1", "yield"] {
        let construct =
            format!("class C {{ x: any = {default}\n  fn init() {{}} }}\nprint(C().x)");
        let tw = ascript::run_source(&construct).await;
        let vm = ascript::vm_run_source(&construct).await;
        assert!(
            tw.is_err() && vm.is_err(),
            "expected BOTH engines to reject the `{default}` field default\n  tree-walker: {tw:?}\n  vm: {vm:?}"
        );
        // The generic VM must agree with the specialized VM (no specialization can
        // turn a rejected default into a running one).
        let vm_generic = ascript::vm_run_source_generic(&construct).await;
        assert!(
            vm_generic.is_err(),
            "generic VM must also reject the `{default}` field default\n  {vm_generic:?}"
        );
    }
}

#[tokio::test]
async fn sp2_f_inclusive_range_field_default_three_way() {
    // SP2 Phase F (regression-lock + spec correction): `..=` as a class-field default
    // materializes to an eager `array<number>` and runs BYTE-IDENTICAL on all three
    // engines (tree-walker == specialized-VM == generic-VM), via BOTH the construct
    // path (`C()`) and the `.from({})` path (which reads the SAME lowered default).
    // Per SP2 §6 this supersedes the stale SP1 note claiming `..=` field defaults are
    // rejected — they are supported; only `yield` defaults remain rejected (locked
    // below). The exact `[1, 2, 3]` rendering is the regression anchor.
    assert_three_way_matches("class C { xs: array<number> = 1..=3 }\nprint(C().xs)\n").await;
    assert_three_way_matches("class C { xs: array<number> = 1..=3 }\nprint(C.from({}).xs)\n").await;
    // Stepped inclusive — `0..=10 step 2` → `[0, 2, 4, 6, 8, 10]`, both paths.
    assert_three_way_matches(
        "class C { xs: array<number> = 0..=10 step 2 }\nprint(C().xs)\n",
    )
    .await;
    assert_three_way_matches(
        "class C { xs: array<number> = 0..=10 step 2 }\nprint(C.from({}).xs)\n",
    )
    .await;
}

#[tokio::test]
async fn sp2_f_yield_field_default_rejected_three_way() {
    // SP2 Phase F: the `yield` field default MUST stay rejected, symmetrically with
    // `..=` being accepted. `yield` is never valid outside a generator body. The VM
    // rejects it at COMPILE time (`cst_default_expr`); the tree-walker rejects it when
    // it EVALUATES the default. The message/span legitimately differ (compile vs
    // runtime), but the OBSERVABLE outcome is symmetric: ALL THREE engines error,
    // none runs the program (no output). Lock the symmetry of the rejection.
    for default in ["yield 5", "yield"] {
        let src = format!("class C {{ x: number = {default} }}\nC()\n");
        let tw = ascript::run_source(&src).await;
        let spec = ascript::vm_run_source(&src).await;
        let generic = ascript::vm_run_source_generic(&src).await;
        assert!(
            tw.is_err() && spec.is_err() && generic.is_err(),
            "expected ALL THREE engines to reject the `{default}` field default\n  \
             tree-walker: {tw:?}\n  specialized: {spec:?}\n  generic: {generic:?}"
        );
    }
}

#[tokio::test]
async fn vm_field_default_reviewer_regression() {
    // The EXACT program from the divergence report: a computed string-concat default
    // referencing a module const, printed off a freshly-constructed instance. The VM
    // previously errored "field-default expression form BinaryExpr is not supported";
    // now it matches the tree-walker (prints `prex`).
    let src = "let PREFIX = \"pre\"\nclass C { tag: string = PREFIX + \"x\"\n  fn init() {} }\nprint(C().tag)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_method_calls_method_via_self() {
    // A method that calls another method through self, threading state.
    let src = "class Counter {\n  n: number\n  fn init(start) { self.n = start }\n  fn bump() { self.n = self.n + 1 }\n  fn twice() { self.bump()\n self.bump()\n return self.n }\n}\nlet c = Counter(0)\nprint(c.twice())\nprint(c.n)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_no_init_no_args() {
    // A class with no init now auto-derives a positional constructor over its
    // declared fields (SP2 §5 records): `Bag(7)` binds `items`, and a method
    // reads it. (Pre-records this class was `Bag()` + a late `b.items = 7`; that
    // late-init style is superseded by the record constructor — the required
    // field must be supplied at construction, identically on both engines.)
    let src = "class Bag {\n  items: number\n  fn total() { return self.items }\n}\nlet b = Bag(7)\nprint(b.total())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_method_returns_closure_capturing_self() {
    // A method returns a closure that captures `self` — the closure observes the
    // instance's field through the captured receiver (self is a cell slot).
    let src = "class Box {\n  v: number\n  fn init(v) { self.v = v }\n  fn getter() { return () => self.v }\n}\nlet b = Box(99)\nlet g = b.getter()\nprint(g())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_typed_field_violation_in_init_panics() {
    // Assigning a wrong-typed value to a typed field INSIDE init is a Tier-2
    // contract panic — identical message + span on both engines.
    let src = "class Point {\n  x: number\n  fn init(x) { self.x = x }\n}\nlet p = Point(\"oops\")";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_typed_field_violation_via_member_panics() {
    // Assigning a wrong-typed value to a typed field via `obj.f = wrong` is a
    // Tier-2 contract panic — identical message + span.
    let src =
        "class Point {\n  x: number\n  fn init(x) { self.x = x }\n}\nlet p = Point(1)\np.x = true";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_no_init_with_args_panics() {
    // A class with no init given args is the same Tier-2 panic on both engines.
    let src = "class Empty {\n  v: number\n}\nlet e = Empty(1, 2)";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_init_arity_violation_panics() {
    // Wrong arg count to init surfaces the SAME arity panic (message + span).
    let src = "class Point {\n  x: number\n  y: number\n  fn init(x, y) { self.x = x\n self.y = y }\n}\nlet p = Point(1)";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_string_field_and_template() {
    // A string field interpolated in a template method (exercises Display + the
    // method dispatch + self field read in one program).
    let src = "class Greeter {\n  name: string\n  fn init(name) { self.name = name }\n  fn hello() { return `hi ${self.name}` }\n}\nprint(Greeter(\"Ada\").hello())";
    assert_vm_run_matches_treewalker(src).await;
}

// ---- .from defaults + nested-class coercion (V12, task #157) --------------
//
// `ClassName.from(obj)` is powered by the SHARED `Interp::validate_into`, which
// reads two pieces of class state the VM compiler must populate to match the
// tree-walker: (1) `FieldSchema.default` (the lowered default expr, applied to a
// MISSING field) and (2) `Class.def_env` (resolves a nested-class field type
// name + a default-expr name). These tests assert the VM `.from` path is now
// byte-identical to the tree-walker, including the recoverable field-path panic
// message+span via the error-parity helper.

#[tokio::test]
async fn vm_from_applies_field_default() {
    // A missing defaulted field (`role`) is filled by the lowered default expr
    // during `.from` (NOT via the construct-time thunk — `.from` does not run
    // init). Must be byte-identical to the tree-walker.
    let src = "class User {\n  id: number\n  name: string\n  role: string = \"guest\"\n}\nlet u = User.from({id: 1, name: \"Ada\"})\nprint(u.role)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_from_coerces_nested_class_field() {
    // A nested-class field (`address: Address`) — the raw sub-object validates
    // into an `Address` instance, which requires the field type name `Address`
    // to resolve in the class's `def_env`.
    let src = "class Address {\n  street: string\n  zip: number\n}\nclass User {\n  name: string\n  address: Address\n}\nlet u = User.from({name: \"Ada\", address: {street: \"1 Way\", zip: 90210}})\nprint(u.address.zip)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_from_wrong_type_panics_identically() {
    // A wrong-typed field is a recoverable field-path contract panic — identical
    // message + span on both engines.
    let src = "class User {\n  id: number\n  name: string\n  role: string = \"guest\"\n}\nlet u = User.from({id: \"nope\", name: \"Ada\"})";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_from_missing_required_panics_identically() {
    // A missing REQUIRED (non-defaulted) field — `name` defaults to nil, which
    // violates `string`. Identical recoverable field-path panic on both engines.
    let src = "class User {\n  id: number\n  name: string\n  role: string = \"guest\"\n}\nlet u = User.from({id: 1})";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_from_nested_class_field_violation_panics_identically() {
    // A nested-class field whose sub-object is wrong-typed — the recursion into
    // `Address` produces a nested field-path panic identical on both engines.
    let src = "class Address {\n  street: string\n  zip: number\n}\nclass User {\n  name: string\n  address: Address\n}\nlet u = User.from({name: \"Ada\", address: {street: \"1 Way\", zip: \"bad\"}})";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_construct_defaulted_field_still_byte_identical() {
    // CONSTRUCT (`ClassName(args)`) must stay byte-identical after the #157 fix:
    // the defaulted field still applies via the construct-time thunk (a FRESH
    // value per instance), init runs, the typed fields hold.
    let src = "class User {\n  id: number\n  name: string\n  role: string = \"guest\"\n  fn init(id, name) { self.id = id\n self.name = name }\n}\nlet u = User(7, \"Lin\")\nprint(u.id)\nprint(u.name)\nprint(u.role)";
    assert_vm_run_matches_treewalker(src).await;
}

// ---- inheritance, super (V9-T2): extends + super.method + super.init -----
//
// AScript has single inheritance via `extends` and `super.method(...)` (spec
// §8.1). There is NO `instanceof`/`is` operator in the surface language (the
// tree-walker has none either; the class name is used as a *type* in contracts,
// which §5 already covers and the VM checks via the shared `check_type`). So
// V9-T2 covers `extends` + `super` (method + init) + inherited fields/methods/
// contracts; `instanceof`/`is` is N/A (no syntax to mirror). The differential
// harness compares VM vs tree-walker byte-for-byte; semantics are mirrored from
// the tree-walker (`find_method`/`merged_field_schema`/`invoke_method`'s super
// binding) exactly.

#[tokio::test]
async fn vm_inheritance_method_override() {
    // A subclass overrides a parent method; dispatch picks the subclass's.
    let src = "class Animal {\n  fn speak() { return \"...\" }\n}\nclass Dog extends Animal {\n  fn speak() { return \"woof\" }\n}\nlet d = Dog()\nprint(d.speak())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_inheritance_inherited_method() {
    // A subclass calls a method it does NOT define — resolved up the chain to the
    // parent (the parent registered its compiled method in the VM side-table).
    let src = "class Animal {\n  fn speak() { return \"animal noise\" }\n}\nclass Dog extends Animal {\n  fn bark() { return \"woof\" }\n}\nlet d = Dog()\nprint(d.speak())\nprint(d.bark())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_super_method_call() {
    // `super.greet()` resolves greet starting at the DEFINING class's superclass,
    // bound to the current self. Result is "A" + "B" = "AB".
    let src = "class A {\n  fn greet() { return \"A\" }\n}\nclass B extends A {\n  fn greet() { return super.greet() + \"B\" }\n}\nprint(B().greet())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_super_init_then_own_fields() {
    // `super.init(...)` runs the parent init on the same self, then the subclass
    // sets its own field. Read both inherited and own fields after construction.
    let src = "class Animal {\n  name: string\n  fn init(name) { self.name = name }\n}\nclass Dog extends Animal {\n  breed: string\n  fn init(name, breed) { super.init(name)\n self.breed = breed }\n}\nlet d = Dog(\"Rex\", \"Husky\")\nprint(d.name)\nprint(d.breed)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_super_method_chains_with_self() {
    // The oop.as shape: super.describe() composed with a subclass override, and a
    // subclass-only method. Mirrors examples/oop.as (sans the enum).
    let src = "class Animal {\n  fn init(name) { self.name = name }\n  fn describe() { return `${self.name} is an animal` }\n}\nclass Dog extends Animal {\n  fn init(name) { super.init(name) }\n  fn describe() { return super.describe() + \", specifically a dog\" }\n  fn sound() { return \"woof\" }\n}\nlet d = Dog(\"Rex\")\nprint(d.describe())\nprint(d.sound())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_inherited_field_default_and_contract() {
    // A defaulted field declared on the BASE class is applied at construct time
    // for a subclass instance (merged_field_schema walks the chain base-first).
    let src = "class Base {\n  kind: string = \"base\"\n  fn init() {}\n}\nclass Sub extends Base {\n  n: number\n  fn init(n) { super.init()\n self.n = n }\n}\nlet s = Sub(5)\nprint(s.kind)\nprint(s.n)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_inherited_field_contract_violation_panics() {
    // Assigning a wrong-typed value to a field declared on the PARENT class is a
    // Tier-2 contract panic on both engines (lookup_field_schema walks the chain).
    let src = "class Base {\n  name: string\n  fn init(name) { self.name = name }\n}\nclass Sub extends Base {\n  fn init() { self.name = 42 }\n}\nlet s = Sub()";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_super_init_arity_violation_panics() {
    // A wrong-arity `super.init(...)` surfaces the parent init's arity panic
    // identically (the super-bound method runs the SAME check_call_args).
    let src = "class Base {\n  fn init(a, b) { self.a = a\n self.b = b }\n}\nclass Sub extends Base {\n  fn init() { super.init(1) }\n}\nlet s = Sub()";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_three_level_inheritance_chain() {
    // A three-deep chain: C extends B extends A. `super` and inherited-method
    // resolution must walk more than one ancestor link.
    let src = "class A {\n  fn who() { return \"A\" }\n}\nclass B extends A {\n  fn who() { return super.who() + \"B\" }\n}\nclass C extends B {\n  fn who() { return super.who() + \"C\" }\n}\nprint(C().who())";
    assert_vm_run_matches_treewalker(src).await;
}

// ----- V9-T3: enums + variants ------------------------------------------------

#[tokio::test]
async fn vm_enum_decl_and_variant_access() {
    // Declaring an enum binds a `Value::Enum`; `Color.Red` reads a `Value::EnumVariant`
    // whose display is `Color.Red` — byte-identical to the tree-walker.
    for src in [
        "enum Color { Red, Green, Blue }\nprint(Color.Red)",
        "enum Color { Red, Green, Blue }\nprint(Color.Green)",
        "enum Color { Red, Green, Blue }\nprint(Color.Blue)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_variant_equality() {
    // Variants are interned per enum: the SAME variant is `==`, a DIFFERENT variant
    // is not, and variants from DIFFERENT enums never compare equal. The EQ op uses
    // `Value` `PartialEq` (Rc::ptr_eq for `EnumVariant`), unchanged from the tree-walker.
    for src in [
        "enum Color { Red, Green }\nprint(Color.Red == Color.Red)",
        "enum Color { Red, Green }\nprint(Color.Red == Color.Green)",
        "enum Color { Red, Green }\nprint(Color.Red != Color.Green)",
        // Cross-enum: even same-named variants are never equal.
        "enum A { X, Y }\nenum B { X, Y }\nprint(A.X == B.X)",
        // A variant is never equal to its backing value.
        "enum Status { Ok = 200 }\nprint(Status.Ok == 200)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_type_of() {
    // `type(Color)` → "enum"; `type(Color.Red)` → "enum variant".
    for src in [
        "enum Color { Red, Green }\nprint(type(Color))",
        "enum Color { Red, Green }\nprint(type(Color.Red))",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_backing_value_and_name() {
    // Number- and string-backed variants: `.value` yields the backing literal,
    // `.name` the variant's name string; an unbacked variant's `.value` is `nil`.
    for src in [
        "enum Status { Ok = 200, NotFound = 404 }\nprint(Status.Ok.value)",
        "enum Status { Ok = 200, NotFound = 404 }\nprint(Status.NotFound.value)",
        "enum Status { Ok = 200 }\nprint(Status.Ok.name)",
        "enum Mode { Read = \"r\", Write = \"w\" }\nprint(Mode.Read.value)",
        "enum Mode { Read = \"r\", Write = \"w\" }\nprint(Mode.Write.value)",
        // Unbacked variant → `.value` is nil.
        "enum Color { Red, Green }\nprint(Color.Red.value)",
        "enum Color { Red, Green }\nprint(Color.Green.name)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_variant_in_let_and_fn() {
    // A variant stored in a local, passed to a function, returned, and compared.
    for src in [
        "enum Color { Red, Green, Blue }\nlet c = Color.Green\nprint(c)",
        "enum Color { Red, Green, Blue }\nlet c = Color.Green\nprint(c == Color.Green)",
        "enum Color { Red, Green }\nfn name(x) { return x.name }\nlet c = Color.Red\nprint(name(c))",
        "enum Color { Red, Green }\nfn isRed(x) { return x == Color.Red }\nprint(isRed(Color.Red))\nprint(isRed(Color.Green))",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_in_conditional() {
    // A variant used in an `if` condition (full `match` is V10).
    for src in [
        "enum Color { Red, Green }\nlet c = Color.Red\nif (c == Color.Red) { print(\"red\") }",
        "enum Color { Red, Green }\nlet c = Color.Green\nif (c == Color.Red) { print(\"red\") } else { print(\"other\") }",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_enum_no_variant_error() {
    // Accessing a missing variant raises the SAME Tier-2 panic message on both
    // engines (shared `read_member` on `Value::Enum`).
    let src = "enum Color { Red, Green }\nprint(Color.Purple)";
    assert_vm_run_error_matches_treewalker(src).await;
}

// ---- ClassName.from validation (V9-T4): via the CALL_METHOD path ---------
//
// `User.from({...})` compiles to `<class-name-ref> <obj-arg> CALL_METHOD "from"`.
// In CALL_METHOD the receiver is a `Value::Class` (NOT a schema value), so the
// hook falls through to `vm_read_member(class, "from")` → `Interp::read_member`
// (yields `Value::ClassMethod(c, "from")`) → `Vm::call_value` → the non-VM-class
// arm delegates to `Interp::call_value`, whose `ClassMethod(c, "from")` arm runs
// `validate_into`. So the `.from` CALL PATH is REACHABLE on the VM with NO import
// (the class is defined in-file): a valid object, an optional-no-default field,
// a recoverable/uncaught shape mismatch, strict mode, a non-object arg, and an
// unknown static member ALL match the tree-walker byte-for-byte. The two `.from`
// sub-features that depend on tree-walker-only class state — DEFAULTED fields
// (`FieldSchema.default`) and NESTED-class coercion (`Class.def_env`) — currently
// DIVERGE on the VM and are deferred to V12 (tracked separately — they need the
// global-env / CST->AST default bridge). Mirrored from examples/shape_validation.as.
#[tokio::test]
async fn vm_class_from_valid_constructs_instance() {
    // A valid object validates into an instance whose fields read back; `.from`
    // does NOT run `init` — it assigns validated fields directly.
    let src = "class User {\n  name: string\n  age: number\n}\nlet u = User.from({name: \"ann\", age: 30})\nprint(u.name)\nprint(u.age)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_from_optional_field_without_default() {
    // An OPTIONAL field with NO default (`nickname: string?`) is absent → binds
    // nil, with no default-eval needed. This part of `.from` is reachable on the
    // VM (no `FieldSchema.default`, no nested-class env lookup). (The DEFAULTED
    // field case `role: string = "guest"` is the documented divergence below.)
    let src = "class User {\n  id: number\n  name: string\n  nickname: string?\n}\nlet u = User.from({id: 1, name: \"Ada\"})\nprint(u.id)\nprint(u.name)\nprint(u.nickname)";
    assert_vm_run_matches_treewalker(src).await;
}

// NOTE: two `.from` sub-features (DEFAULTED fields + NESTED-class coercion) diverge
// on the VM and are tracked for V12 (task: VM .from defaults + nested-class via the
// def_env / CST->AST-default bridge). They depend on `validate_into` reading
// `FieldSchema.default` + `Class.def_env`, which the VM compiler does not populate
// yet (defaults are compiled thunks; user classes live in slots, not an Environment).
// Reproducers live in the V12 task description, not as `#[ignore]`d tests here.
#[tokio::test]
async fn vm_class_from_recovered_shape_mismatch_matches_treewalker() {
    // A wrong-typed field is a RECOVERABLE field-path panic; caught by `recover`
    // it yields `[nil, err]`. The error message must be byte-identical (it
    // carries the field path), which the printed `r[1].message` verifies.
    let src = "class User {\n  name: string\n  age: number\n}\nlet r = recover(() => User.from({name: \"Bug\", age: \"nope\"}))\nprint(r[0])\nprint(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_from_missing_field_recovered_matches_treewalker() {
    // A missing required field (no default) → nil → type contract violated;
    // recovered, the field-path message is byte-identical.
    let src = "class User {\n  name: string\n  age: number\n}\nlet r = recover(() => User.from({name: \"Bug\"}))\nprint(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_from_uncaught_shape_mismatch_aborts_identically() {
    // Uncaught, the `.from` field-path panic aborts the program with the SAME
    // message AND span on both engines.
    let src =
        "class User {\n  name: string\n  age: number\n}\nUser.from({name: \"Bug\", age: \"nope\"})";
    assert_vm_run_error_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_from_strict_rejects_unexpected_key_matches_treewalker() {
    // `.from(obj, true)` (strict) rejects an unexpected key; recovered, the
    // message is byte-identical.
    let src = "class User {\n  name: string\n}\nlet r = recover(() => User.from({name: \"Ada\", extra: 1}, true))\nprint(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_from_not_an_object_matches_treewalker() {
    // `.from` on a non-object argument is a recoverable panic with a byte-identical
    // message on both engines.
    let src = "class User {\n  name: string\n}\nlet r = recover(() => User.from(42))\nprint(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_unknown_static_member_matches_treewalker() {
    // A class static member other than `from` is the SAME Tier-2 panic on both
    // engines (shared `read_member` on `Value::Class`).
    let src = "class User {\n  name: string\n}\nprint(User.nope)";
    assert_vm_run_error_matches_treewalker(src).await;
}

// ---- std/schema fluent-method chaining hook (V9-T4): CALL_METHOD routing --
//
// The schema fluent-method hook in CALL_METHOD (`is_schema_value(recv) &&
// is_schema_method(name) → Interp::call_schema(name, [recv, ...args])`) mirrors
// the tree-walker's `eval_chain` Member-callee arm EXACTLY. The usual entry
// (`schema.string().minLength(3)`) needs `import * as schema` (V12), so an
// END-TO-END schema example is DEFERRED to V12. But the ROUTING is reachable
// NOW by building a schema-TAGGED Object literal in-file (no import): a
// `{__kind: "string", ...}` object IS a schema value (`is_schema_value` keys on
// the `__kind` tag), so calling a refiner/terminal method on it fires the hook.
// These prove the VM's CALL_METHOD takes the schema branch byte-identically to
// the tree-walker.
#[tokio::test]
async fn vm_schema_method_on_tagged_object_literal_matches_treewalker() {
    // `.minLength(3)` on a string-schema-tagged Object returns a new schema with
    // the constraint stored; reading it back proves `call_schema` ran on the VM.
    let src = "let s = {__kind: \"string\"}\nlet s2 = s.minLength(3)\nprint(s2.minLength)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_schema_method_chain_on_tagged_object_literal_matches_treewalker() {
    // A CHAIN of refiners, each a CALL_METHOD hitting the schema hook in turn.
    let src = "let s = {__kind: \"string\"}\nlet s2 = s.minLength(3).maxLength(10)\nprint(s2.minLength)\nprint(s2.maxLength)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_schema_parse_method_on_tagged_object_literal_matches_treewalker() {
    // The terminal `.parse(value)` method validates an input against the schema
    // (a `[value, err]` Tier-1 pair). Routing through the VM's schema hook must
    // match the tree-walker byte-for-byte.
    let src = "let s = {__kind: \"string\"}\nlet r = s.minLength(2).parse(\"hello\")\nprint(r[0])\nprint(r[1])";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_schema_parse_failure_on_tagged_object_literal_matches_treewalker() {
    // A failing `.parse` yields `[nil, err]`; the error message must be identical.
    let src = "let s = {__kind: \"string\"}\nlet r = s.minLength(5).parse(\"hi\")\nprint(r[0])\nprint(r[1].message)";
    assert_vm_run_matches_treewalker(src).await;
}

// ===========================================================================
// V9-T5: deferred assignment forms — compound (+= -= *= /=) + index/member
// assignment (a[i]=x, a.k=x). Differential: every case must be byte-identical to
// the tree-walker (the oracle). The tree-walker desugars `a OP= b` to the literal
// `a = (a OP b)` (so the target's sub-expressions are evaluated TWICE for a
// compound assignment); the VM reproduces that exactly (no receiver caching). The
// value is evaluated BEFORE the receiver/index in BOTH engines.
// ===========================================================================

#[tokio::test]
async fn vm_compound_assign_on_local() {
    for src in [
        "let x = 10\nx += 5\nprint(x)",
        "let x = 10\nx -= 3\nprint(x)",
        "let x = 10\nx *= 2\nprint(x)",
        "let x = 12\nx /= 4\nprint(x)",
        // chained on the same local
        "let x = 1\nx += 2\nx *= 3\nx -= 4\nprint(x)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_compound_assign_on_array_element_and_object_field() {
    for src in [
        "let a = [1, 2, 3]\na[1] += 10\nprint(a[1])",
        "let a = [1, 2, 3]\na[0] -= 5\nprint(a[0])",
        "let a = [2, 4, 6]\na[2] *= 3\nprint(a[2])",
        "let a = [20, 8, 6]\na[1] /= 2\nprint(a[1])",
        "let o = {n: 1}\no.n += 5\nprint(o.n)",
        "let o = {n: 10}\no.n -= 3\no.n *= 2\nprint(o.n)",
        // index by string key on an object
        "let o = {n: 1}\no[\"n\"] += 9\nprint(o.n)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_index_assignment_array_and_object() {
    for src in [
        "let a = [1, 2, 3]\na[0] = 99\nprint(a[0])",
        "let a = [1, 2, 3]\na[2] = 7\nprint(a[2])",
        // add a new key to an object via index assignment
        "let o = {}\no[\"k\"] = 7\nprint(o.k)",
        // overwrite existing key
        "let o = {k: 1}\no[\"k\"] = 2\nprint(o[\"k\"])",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_member_assignment_object_and_instance() {
    for src in [
        "let o = {a: 1}\no.a = 2\nprint(o.a)",
        // add a new field to an object
        "let o = {}\no.x = 5\nprint(o.x)",
        // instance field set inside + outside init
        "class P { name: string\n fn init(n) { self.name = n } }\n\
         let p = P(\"a\")\np.name = \"b\"\nprint(p.name)",
        // compound on a typed instance field (number contract holds)
        "class C { n: number\n fn init() { self.n = 1 } }\n\
         let c = C()\nc.n += 41\nprint(c.n)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_typed_instance_field_violation_panic_matches_treewalker() {
    // Assigning a string to a `number`-typed field is a Tier-2 contract panic.
    // Both engines must produce the identical message AND span.
    let src = "class C { n: number\n fn init() { self.n = 1 } }\n\
               let c = C()\nc.n = \"oops\"\nprint(c.n)";
    let tw = ascript::run_source(src)
        .await
        .expect_err("tree-walker errors");
    let vm = ascript::vm_run_source(src).await.expect_err("vm errors");
    assert_eq!(
        tw.message, vm.message,
        "panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
        tw.message, vm.message
    );
    assert_eq!(
        tw.span, vm.span,
        "panic span diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
        tw.span, vm.span
    );
}

#[tokio::test]
async fn vm_compound_assign_single_eval_order_matches_treewalker() {
    // CRITICAL: the tree-walker desugars `a[i] OP= b` to `a[i] = (a[i] OP b)`, so
    // it evaluates the receiver+index TWICE (read side + store side) and the rhs
    // once, in the order: receiver, index, rhs, receiver, index. The VM must print
    // these side effects in the EXACT same order/count. We assert byte-identical
    // stdout (which captures every `print` from the side-effecting helpers).
    // The side-effecting helpers are nested in a driver fn so they capture the
    // mutated container as an UPVALUE (the VM does not yet support a nested fn
    // referencing a top-level `let` as a bare global — a separate V4 deferral
    // unrelated to assignment).
    for src in [
        // index compound: order a, i, b, a, i
        "fn run() {\n let arr = [1, 2, 3]\n\
         fn a() { print(\"a\")\n return arr }\n\
         fn i() { print(\"i\")\n return 1 }\n\
         fn b() { print(\"b\")\n return 100 }\n\
         a()[i()] += b()\n print(arr[1]) }\nrun()",
        // member compound: order a, b, a
        "fn run() {\n let holder = {n: 5}\n\
         fn obj() { print(\"obj\")\n return holder }\n\
         fn rhs() { print(\"rhs\")\n return 7 }\n\
         obj().n += rhs()\n print(holder.n) }\nrun()",
        // plain index assign: order val, recv, idx
        "fn run() {\n let base = [1, 2, 3]\n\
         fn recv() { print(\"recv\")\n return base }\n\
         fn idx() { print(\"idx\")\n return 0 }\n\
         fn val() { print(\"val\")\n return 9 }\n\
         recv()[idx()] = val()\n print(base[0]) }\nrun()",
        // plain member assign: order val, obj
        "fn run() {\n let o = {}\n\
         fn obj() { print(\"obj\")\n return o }\n\
         fn val() { print(\"val\")\n return 5 }\n\
         obj().x = val()\n print(o.x) }\nrun()",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_assignment_is_an_expression() {
    for src in [
        // index assignment yields the assigned value
        "let a = [0]\nprint(a[0] = 5)",
        // member assignment yields the assigned value
        "let o = {}\nprint(o.k = 9)",
        // local assignment yields the assigned value
        "let x = 0\nprint(x = 3)",
        // compound assignment yields the NEW value
        "let x = 10\nprint(x += 5)",
        "let a = [1]\nprint(a[0] += 41)",
        // chained assignment (right-associative): both bound to the value
        "let x = 0\nlet y = 0\nx = y = 7\nprint(x)\nprint(y)",
    ] {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_compound_assign_on_upvalue_counter() {
    // A closure increments a captured (upvalue) variable via `+=`. Both engines
    // must thread the by-reference cell identically.
    let src = "fn counter() {\n let n = 0\n return () => { n += 1\n return n } }\n\
               let next = counter()\nprint(next())\nprint(next())\nprint(next())";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_index_assign_oob_and_wrong_type_panics_match_treewalker() {
    // Out-of-bounds and wrong-index-type index assignment are Tier-2 panics with
    // byte-identical message AND span on both engines (shared `index_set`). These
    // all anchor at the tree-walker's `index_span` (the whole `a[i]` expr), which
    // the VM's single `SET_INDEX` op span reproduces exactly.
    for src in [
        "let a = [1, 2, 3]\na[9] = 0\nprint(a)",   // OOB
        "let a = [1, 2, 3]\na[-1] = 0\nprint(a)",  // negative index
        "let a = [1, 2, 3]\na[1.5] = 0\nprint(a)", // non-integer index
        "let o = {}\no[5] = 1\nprint(o)",          // non-string object key
    ] {
        let tw = ascript::run_source(src)
            .await
            .expect_err("tree-walker errors");
        let vm = ascript::vm_run_source(src).await.expect_err("vm errors");
        assert_eq!(
            tw.message, vm.message,
            "panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
            tw.message, vm.message
        );
        assert_eq!(
            tw.span, vm.span,
            "panic span diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
            tw.span, vm.span
        );
    }

    // Index-assigning a non-container (`n[0] = 1` where `n` is a number) is also a
    // Tier-2 panic with a byte-identical MESSAGE. The tree-walker anchors this one
    // case at the RECEIVER's span (`obj_span`), whereas the VM's single `SET_INDEX`
    // op span is the whole-expr span (chosen to match the far-more-common OOB /
    // wrong-type / object-key cases above). This is the SAME documented single-VM-
    // span limitation as `Op::SetProp`'s "cannot set property" path, so message-
    // equality is asserted here, not span-equality.
    let src = "let n = 5\nn[0] = 1\nprint(n)";
    let tw = ascript::run_source(src)
        .await
        .expect_err("tree-walker errors");
    let vm = ascript::vm_run_source(src).await.expect_err("vm errors");
    assert_eq!(
        tw.message, vm.message,
        "panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
        tw.message, vm.message
    );
}

// ----- V9-T6: class/enum multi-feature sync programs ---------------------------

#[tokio::test]
async fn vm_run_class_enum_multi_feature_programs() {
    // Realistic OOP/enum SYNC snippets that COMBINE the V9 class/enum/super/method/
    // field/assignment feature set (on top of V1..V8) in one run: classes with
    // methods + typed/defaulted fields, inheritance + `super` chains, enum variant
    // comparisons in conditionals, instances stored in arrays/objects + iterated,
    // a method returning a closure capturing `self`, compound assignment on
    // instance fields (`self.count += 1`), small OOP simulations, and enum-driven
    // dispatch via if/else. NONE use `match`/destructuring/spread/`import`/`.from`
    // (V10/V12). Each must be byte-identical to the tree-walker.
    let programs = [
        // (a) class with methods + typed defaulted fields; construct + call + read.
        "class Point {\n  x: number = 0\n  y: number = 0\n  fn init(x, y) { self.x = x\n self.y = y }\n  fn norm2() { return self.x * self.x + self.y * self.y }\n}\nlet p = Point(3, 4)\nprint(p.norm2())\nprint(p.x)",
        // (b) inheritance + `super` chain (super.init + super.describe).
        "class Animal {\n  fn init(name) { self.name = name }\n  fn describe() { return `${self.name} is an animal` }\n}\nclass Dog extends Animal {\n  fn init(name) { super.init(name) }\n  fn describe() { return super.describe() + \", specifically a dog\" }\n  fn sound() { return \"woof\" }\n}\nlet d = Dog(\"Rex\")\nprint(d.describe())\nprint(d.sound())\nprint(d.name)",
        // (c) enum + variant comparisons used in conditionals.
        "enum Color { Red, Green, Blue }\nlet c = Color.Green\nif (c == Color.Green) { print(\"green\") } else { print(\"other\") }\nprint(c == Color.Red)\nprint(c == Color.Green)",
        // (d) instances stored in an array + iterated (for-of), member read.
        "class Box {\n  fn init(v) { self.v = v }\n}\nlet boxes = [Box(1), Box(2), Box(3)]\nlet sum = 0\nfor (b of boxes) { sum = sum + b.v }\nprint(sum)\nprint(boxes[1].v)",
        // (e) instances stored in an object; method mutates self via `+=`.
        "class Counter {\n  fn init() { self.n = 0 }\n  fn bump() { self.n += 1 }\n}\nlet reg = {a: Counter(), b: Counter()}\nreg.a.bump()\nreg.a.bump()\nreg.b.bump()\nprint(reg.a.n)\nprint(reg.b.n)",
        // (f) method returning a closure capturing `self`.
        "class Adder {\n  fn init(base) { self.base = base }\n  fn make() { return (x) => self.base + x }\n}\nlet a = Adder(10)\nlet f = a.make()\nprint(f(5))\nprint(f(100))",
        // (g) compound assignment on instance fields (`self.count += 1`).
        "class Acc {\n  fn init() { self.count = 0\n self.total = 0 }\n  fn add(n) { self.count += 1\n self.total += n }\n}\nlet acc = Acc()\nacc.add(5)\nacc.add(7)\nacc.add(3)\nprint(acc.count)\nprint(acc.total)",
        // (h) small OOP simulation: a fixed-capacity stack (index assignment into a
        // field array + a `self.n` top pointer; no stdlib import needed).
        "class Stack {\n  fn init() { self.items = [0, 0, 0, 0]\n self.n = 0 }\n  fn push(v) { self.items[self.n] = v\n self.n += 1 }\n  fn size() { return self.n }\n  fn top() { return self.items[self.n - 1] }\n}\nlet s = Stack()\ns.push(10)\ns.push(20)\ns.push(30)\nprint(s.size())\nprint(s.top())\nprint(s.items)",
        // (i) enum-driven dispatch via if/else if/else in a function.
        "enum Op { Add, Sub, Mul }\nfn apply(op, a, b) {\n  if (op == Op.Add) { return a + b }\n  else if (op == Op.Sub) { return a - b }\n  else { return a * b }\n}\nprint(apply(Op.Add, 6, 4))\nprint(apply(Op.Sub, 6, 4))\nprint(apply(Op.Mul, 6, 4))",
        // (j) enums stored in objects + an array, iterated with comparison + count.
        "enum Status { Active, Idle, Done }\nlet tasks = [{name: \"a\", st: Status.Active}, {name: \"b\", st: Status.Done}, {name: \"c\", st: Status.Active}]\nlet active = 0\nfor (t of tasks) { if (t.st == Status.Active) { active += 1 } }\nprint(active)\nprint(tasks[1].st == Status.Done)\nprint(tasks[0].name)",
        // (k) defaulted field + methods doing `+=`/`-=` on the field, returning it.
        "class Wallet {\n  balance: number = 100\n  fn deposit(n) { self.balance += n\n return self.balance }\n  fn withdraw(n) { self.balance -= n\n return self.balance }\n}\nlet w = Wallet()\nprint(w.balance)\nprint(w.deposit(50))\nprint(w.withdraw(30))",
        // (l) three-level shape hierarchy: overridden `area()` (Sq->Rect->Shape via
        // `super.init`), instances in an array, iterated with `+=` accumulation.
        "class Shape {\n  fn area() { return 0 }\n}\nclass Rect extends Shape {\n  fn init(w, h) { self.w = w\n self.h = h }\n  fn area() { return self.w * self.h }\n}\nclass Sq extends Rect {\n  fn init(s) { super.init(s, s) }\n}\nlet shapes = [Rect(2, 3), Sq(4)]\nlet total = 0\nfor (sh of shapes) { total += sh.area() }\nprint(total)\nprint(shapes[1].area())",
    ];
    for src in programs {
        assert_vm_run_matches_treewalker(src).await;
    }
}

// ----- V10-T2: spread in array/object literals + call args --------------------

#[tokio::test]
async fn vm_array_spread_matches_treewalker() {
    let cases = [
        // Single spread of a local array.
        "let a = [1, 2]\nprint([...a, 3])", // [1, 2, 3]
        // Leading item + two spreads of the SAME array (order + duplication).
        "let a = [1, 2]\nprint([0, ...a, ...a])", // [0, 1, 2, 1, 2]
        // Spread of an empty array literal.
        "print([...[]])", // []
        // Spread interleaved with items on both sides.
        "let a = [2, 3]\nprint([1, ...a, 4, 5])", // [1, 2, 3, 4, 5]
        // Nested: spread of an array containing arrays.
        "let a = [[1], [2]]\nprint([...a, [3]])", // [[1], [2], [3]]
        // Spread of array-literals directly.
        "print([...[1, 2], ...[3, 4]])", // [1, 2, 3, 4]
        // Self-spread (the builder array does not alias the source).
        "let a = [9]\nprint([...a, ...a, ...a])", // [9, 9, 9]
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_object_spread_matches_treewalker() {
    let cases = [
        // Spread then a new key.
        "let o = {a: 1, b: 2}\nprint({...o, c: 3})", // {a: 1, b: 2, c: 3}
        // LATER-WINS + FIRST-POSITION: `a` keeps its first position but takes the
        // later value 9 (byte-identical to the tree-walker's IndexMap insert).
        "print({a: 1, ...{a: 9, b: 2}})", // {a: 9, b: 2}
        // Spread then overwrite a spread key with an explicit later entry.
        "let o = {a: 1, b: 2}\nprint({...o, b: 99})", // {a: 1, b: 99}
        // Earlier explicit key overwritten by a spread value (position preserved).
        "print({a: 1, b: 2, ...{a: 7}})", // {a: 7, b: 2}
        // Spread of an empty object literal.
        "print({...{}})", // {}
        // Two spreads, second wins on the overlap, first-seen position kept.
        "print({...{a: 1, b: 2}, ...{b: 3, c: 4}})", // {a: 1, b: 3, c: 4}
        // Nested object value through a spread.
        "let o = {a: {x: 1}}\nprint({...o, b: 2})", // {a: {x: 1}, b: 2}
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_call_spread_matches_treewalker() {
    let cases = [
        // Pure spread of a local array as all args.
        "fn add3(x, y, z) { return x + y + z }\nlet args = [1, 2, 3]\nprint(add3(...args))", // 6
        // Mixed leading positional + spread of an array literal.
        "fn add3(x, y, z) { return x + y + z }\nprint(add3(1, ...[2, 3]))", // 6
        // Spread of an array literal as all args.
        "fn add3(x, y, z) { return x + y + z }\nprint(add3(...[10, 20, 30]))", // 60
        // Trailing positional after a spread.
        "fn add3(x, y, z) { return x + y + z }\nprint(add3(...[1, 2], 3))", // 6
        // Spread into a REST param (collects the flattened tail into an array).
        "fn sum(...nums) {\n  let t = 0\n  for (n of nums) { t = t + n }\n  return t\n}\nlet a = [1, 2, 3, 4]\nprint(sum(...a))", // 10
        // Leading fixed param + spread into a rest param.
        "fn tag(label, ...rest) {\n  print(label)\n  print(rest)\n}\ntag(\"xs\", ...[1, 2, 3])", // xs then [1, 2, 3]
        // Empty spread into a rest param (zero args).
        "fn sum(...nums) {\n  let t = 0\n  for (n of nums) { t = t + n }\n  return t\n}\nprint(sum(...[]))", // 0
        // Spread forwarding round-trip (spread into a wrapper that re-spreads).
        "fn sum(...nums) {\n  let t = 0\n  for (n of nums) { t = t + n }\n  return t\n}\nfn wrap(...args) { return sum(...args) }\nprint(wrap(5, 6, 7))", // 18
    ];
    for src in cases {
        assert_vm_run_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_spread_wrong_type_panics_match_treewalker() {
    // Non-array / non-object spreads are Tier-2 panics — the message AND the span
    // must be byte-identical on both engines (the operand's trivia-trimmed span).
    let cases = [
        // Array spread of a non-array.
        "print([...5])",
        "let n = 7\nprint([1, ...n])",
        "print([...\"hi\"])",
        // Object spread of a non-object.
        "print({...5})",
        "let n = 7\nprint({a: 1, ...n})",
        // Call-arg spread of a non-array (distinct message).
        "fn f(x) { return x }\nprint(f(...5))",
        "fn f(x) { return x }\nlet n = 3\nprint(f(...n))",
    ];
    for src in cases {
        assert_vm_error_matches_treewalker(src).await;
    }
}

#[tokio::test]
async fn vm_run_spread_examples_match_treewalker() {
    // The two corpus examples that exercise spread end-to-end (now compiling on the
    // VM) must produce byte-identical stdout. `spread.as` = array/object/call
    // spread; `rest.as` = rest params + spread (incl. forwarding round-trip).
    let root = env!("CARGO_MANIFEST_DIR");
    for rel in ["examples/spread.as", "examples/rest.as"] {
        let path = std::path::Path::new(root).join(rel);
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read example {rel}: {e}"));
        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on {rel}: {e:?}"));
        let vm = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("VM failed on {rel}: {e:?}"));
        assert_eq!(
            tw, vm,
            "VM diverged from tree-walker for `{rel}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
        );
    }
}

// ============================================================================
// V10-T3/T4: match expression — all pattern kinds, guards, Option-C, |-alts.
// Every case must be byte-identical between the tree-walker and the VM. The
// tree-walker is the ground truth (`src/interp.rs` match_pattern + the Match
// eval): first matching arm wins (top-to-bottom; within an arm any |-alt that
// matches AND whose guard passes runs it); a failed guard falls through to the
// next |-alt then the next arm; no arm matches → the Tier-2 panic
// "no matching arm in match expression".
// ============================================================================

/// Run `src` (a full program) on both engines and assert byte-identical
/// stdout+exit, OR (when both error) byte-identical panic message. This is the
/// match differential — covers both success programs and the no-arm panic.
async fn assert_vm_match_parity(src: &str) {
    let tw = ascript::run_source(src).await;
    let vm = ascript::vm_run_source(src).await;
    match (tw, vm) {
        (Ok(tw_out), Ok((vm_out, code))) => {
            assert_eq!(code, None, "no exit code expected for `{src}`");
            assert_eq!(
                tw_out, vm_out,
                "VM stdout diverged from tree-walker for `{src}`\n  tw: {tw_out:?}\n  vm: {vm_out:?}"
            );
        }
        (Err(tw_err), Err(vm_err)) => {
            assert_eq!(
                tw_err.message, vm_err.message,
                "VM panic message diverged for `{src}`\n  tw: {:?}\n  vm: {:?}",
                tw_err.message, vm_err.message
            );
        }
        (tw, vm) => {
            panic!("VM/tree-walker outcome shape diverged for `{src}`\n  tw: {tw:?}\n  vm: {vm:?}")
        }
    }
}

#[tokio::test]
async fn vm_match_value_wildcard() {
    let cases = [
        r#"let x = 1; print(match x { 1 => "one", 2 => "two", _ => "other" })"#,
        r#"let x = 2; print(match x { 1 => "one", 2 => "two", _ => "other" })"#,
        r#"let x = 9; print(match x { 1 => "one", 2 => "two", _ => "other" })"#,
        r#"print(match "sat" { "sat" => 1, "sun" => 2, _ => 0 })"#,
        r#"print(match true { true => "t", false => "f" })"#,
        r#"print(match nil { nil => "nil!", _ => "no" })"#,
        // match as a sub-expression value, used in arithmetic.
        r#"let n = 3; print((match n { 3 => 10, _ => 0 }) + 5)"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_option_c_compare_vs_bind() {
    let cases = [
        // `k` IS defined → compare (switch-like). `other` is a fall-through bind.
        r#"let k = 5; let v = 5; print(match v { k => "matched k", other => other })"#,
        r#"let k = 5; let v = 9; print(match v { k => "matched k", other => other })"#,
        // A bare-ident pattern that is NOT defined → binds the subject.
        r#"let v = 42; print(match v { captured => captured })"#,
        // Defined-name compare against a non-equal subject falls to the bind arm,
        // and the bound name is usable in the body.
        r#"const NOT_FOUND = 404; print(match 200 { NOT_FOUND => "nf", other => other })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_ranges() {
    let cases = [
        r#"print(match 5 { 0..10 => "small", 10..=100 => "med", _ => "big" })"#,
        r#"print(match 10 { 0..10 => "small", 10..=100 => "med", _ => "big" })"#, // 10 excluded from 0..10
        r#"print(match 100 { 0..10 => "small", 10..=100 => "med", _ => "big" })"#, // 100 included in ..=100
        r#"print(match 500 { 0..10 => "small", 10..=100 => "med", _ => "big" })"#,
        // Range against a non-number subject → no match (falls to default).
        r#"print(match "x" { 0..10 => "n", _ => "other" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_or_alternatives() {
    let cases = [
        r#"print(match "b" { "a" | "b" | "c" => "abc", _ => "?" })"#,
        r#"print(match "z" { "a" | "b" | "c" => "abc", _ => "?" })"#,
        r#"print(match 2 { 1 | 2 | 3 => "low", 4 | 5 => "mid", _ => "hi" })"#,
        r#"print(match 5 { 1 | 2 | 3 => "low", 4 | 5 => "mid", _ => "hi" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_guards() {
    let cases = [
        r#"print(match 200 { x if x > 100 => "big", x => "small" })"#,
        r#"print(match 50 { x if x > 100 => "big", x => "small" })"#,
        // Guard failure falls through to the next arm.
        r#"print(match 5 { n if n > 10 => "a", n if n > 3 => "b", _ => "c" })"#,
        r#"print(match 2 { n if n > 10 => "a", n if n > 3 => "b", _ => "c" })"#,
        // Guard on a defined-name compare.
        r#"let t = 7; print(match 7 { t if false => "no", _ => "yes" })"#,
        // Guard combined with |-alternatives: a failed guard tries the NEXT arm,
        // not the next alt (mirror the tree-walker's `continue`).
        r#"print(match 4 { 4 | 5 if false => "x", _ => "fallthrough" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_array_patterns() {
    let cases = [
        r#"fn d(a) { return match a { [] => "empty", [x] => "one", [x, y] => "two", [first, ...rest] => "many" } }
print(d([])); print(d([1])); print(d([1, 2])); print(d([1, 2, 3]))"#,
        // bind values used in the body.
        r#"print(match [1, 2, 3] { [a, b, c] => a + b + c, _ => 0 })"#,
        r#"print(match [10] { [x] => x, _ => -1 })"#,
        // rest captures the tail (as an array).
        r#"print(match [1, 2, 3, 4] { [head, ...tail] => tail, _ => nil })"#,
        // discard rest `...`.
        r#"print(match [1, 2, 3] { [first, ...] => first, _ => nil })"#,
        // nested value pattern inside the array.
        r#"print(match [1, nil] { [v, nil] => v, [_, e] => e, _ => "?" })"#,
        r#"print(match [1, "boom"] { [v, nil] => v, [_, e] => e, _ => "?" })"#,
        // array pattern against a non-array → no match.
        r#"print(match 5 { [x] => x, _ => "not array" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_object_patterns() {
    let cases = [
        // shorthand bind, sub-pattern value compare, rest.
        r#"fn f(o) { return match o { {type: "a", value} => value, {type} => type, _ => "none" } }
print(f({type: "a", value: 99})); print(f({type: "b"})); print(f({other: 1}))"#,
        // sub-pattern renames via `key: subpat`.
        r#"print(match {role: "admin"} { {role: "admin"} => "is admin", {role: r} => r, _ => "no role" })"#,
        r#"print(match {role: "guest", name: "Sam"} { {role: r, name: n} => n, {role: r} => r, _ => "?" })"#,
        // object rest collects leftover keys.
        r#"print(match {type: "click", x: 1, y: 2, target: "b"} { {type: t, ...extra} => extra, _ => nil })"#,
        // missing required key → no match, falls through.
        r#"print(match {a: 1} { {b} => b, _ => "no b" })"#,
        // object pattern against a non-object → no match.
        r#"print(match 5 { {a} => a, _ => "not object" })"#,
        // instance fields match an object pattern.
        r#"class P { fn init(x) { self.x = x } }
print(match P(7) { {x} => x, _ => "?" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_nested_patterns() {
    let cases = [
        r#"print(match {items: [1, 2, 3]} { {items: [first, ...]} => first, _ => nil })"#,
        r#"print(match {items: []} { {items: [first, ...]} => first, _ => "empty" })"#,
        // nested object inside array.
        r#"print(match [{k: "v"}] { [{k}] => k, _ => "?" })"#,
        // deep nesting with a guard.
        r#"print(match {a: {b: 5}} { {a: {b}} if b > 3 => "big b", _ => "small" })"#,
        r#"print(match {a: {b: 1}} { {a: {b}} if b > 3 => "big b", _ => "small" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_no_arm_panics_identically() {
    // No arm matches → both engines raise the SAME Tier-2 panic message.
    let cases = [
        r#"print(match 5 { 1 => "a", 2 => "b" })"#,
        r#"print(match "x" { "a" => 1, "b" => 2 })"#,
        r#"print(match [1, 2, 3] { [] => "e", [x] => "one" })"#,
    ];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_adt_named_variant_construction() {
    // ADT §3.2: named-field variant construction must be byte-identical on the
    // tree-walker and the VM (the VM routes through the new CALL_NAMED opcode →
    // `construct_variant_args`; the tree-walker through `call_value_named`).
    let enum_decl =
        "enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }\n";
    let bodies = [
        // Order-independent named construction + structural equality.
        "let a = Shape.Rect(w: 3.0, h: 4.0)\nlet b = Shape.Rect(h: 4.0, w: 3.0)\n\
         print(a.value); print(b.value); print(a == b); print(a.w); print(a.h)",
        // Single named field: positional convenience and named both work + equal.
        "print(Shape.Circle(2.0).radius); print(Shape.Circle(radius: 2.0).radius); \
         print(Shape.Circle(2.0) == Shape.Circle(radius: 2.0))",
        // First-class constructor accepts named args.
        "let mk = Shape.Rect\nprint(mk(h: 2.0, w: 1.0).value)",
        // Positional variant still positional-only.
        "print(Shape.Pair(3, 4).value)",
        // Error parity: multi-named called positionally.
        "print(Shape.Rect(3.0, 4.0))",
        // Error parity: unknown / missing / duplicate / named-on-positional.
        "print(Shape.Rect(w: 1.0, z: 2.0))",
        "print(Shape.Rect(w: 1.0))",
        "print(Shape.Rect(w: 1.0, w: 2.0))",
        "print(Shape.Pair(a: 1, b: 2))",
        // Error parity: named field type mismatch.
        "print(Shape.Rect(w: \"x\", h: 2.0))",
        // Error parity (Gate 1 byte-identity, reviewer finding): MIXED spread + named
        // args reach the SAME runtime "all named or all positional, not mixed" panic
        // on BOTH engines — it must NOT be a VM compile-time rejection (the tree-walker
        // reaches it at runtime, so it must be recoverable + dead-code-safe).
        "print(Shape.Rect(w: 1.0, ...[2.0]))",
        "print(Shape.Rect(...[1.0], h: 2.0))",
        // A non-array spread mixed with a named arg: the spread error wins (it is
        // evaluated during arg collection, before the mixed/validate check).
        "print(Shape.Rect(w: 1.0, ...5))",
        // `recover()` catches the mixed-args panic identically on both engines.
        "print(recover(() => Shape.Rect(w: 1.0, ...[2.0])))",
        // DEAD/uncalled code with spread+named must COMPILE and run fine on BOTH
        // engines (it is never executed, so no panic) — proving the VM does not
        // reject it at compile time.
        "fn dead() { return Shape.Rect(w: 1.0, ...[2.0]) }\nprint(\"alive\")",
    ];
    for body in bodies {
        let src = format!("{enum_decl}{body}");
        assert_vm_match_parity(&src).await;
        // Also assert the generic (no-specialize) VM agrees.
        let generic = ascript::vm_run_source_generic(&src).await;
        let special = ascript::vm_run_source(&src).await;
        match (generic, special) {
            (Ok((g, gc)), Ok((s, sc))) => {
                assert_eq!(g, s, "generic vs specialized stdout diverged for `{src}`");
                assert_eq!(gc, sc, "generic vs specialized exit diverged for `{src}`");
            }
            (Err(g), Err(s)) => assert_eq!(
                g.message, s.message,
                "generic vs specialized panic diverged for `{src}`"
            ),
            (g, s) => panic!("generic/specialized shape diverged for `{src}`\n g:{g:?}\n s:{s:?}"),
        }
    }
}

#[tokio::test]
async fn vm_match_enum_variant_patterns() {
    // Enum-variant patterns are `LiteralPat`s holding a member expr (`Shape.Circle`)
    // → a value compare against the resolved variant value.
    let cases = [r#"enum Shape { Circle, Square, Triangle }
fn name(s) { return match s { Shape.Circle => "circle", Shape.Square => "square", _ => "other" } }
print(name(Shape.Circle)); print(name(Shape.Square)); print(name(Shape.Triangle))"#];
    for src in cases {
        assert_vm_match_parity(src).await;
    }
}

#[tokio::test]
async fn vm_match_corpus_examples() {
    // The two corpus examples that exercise `match` end-to-end must now produce
    // byte-identical stdout+exit on the VM. `pattern_matching.as` covers every
    // pattern kind; `oop.as` covers enum-variant patterns inside a method.
    let root = env!("CARGO_MANIFEST_DIR");
    for rel in ["examples/pattern_matching.as", "examples/oop.as"] {
        let path = std::path::Path::new(root).join(rel);
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read example {rel}: {e}"));
        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on {rel}: {e:?}"));
        let vm = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("VM failed on {rel}: {e:?}"));
        assert_eq!(
            tw, vm,
            "VM diverged from tree-walker for `{rel}`\n  tw: {tw:?}\n  vm: {vm:?}"
        );
    }
}

// ============================================================================
// ORACLE #2 — RECORDED GOLDENS (V10-T6)
// ============================================================================
//
// The whole-corpus gate above (`vm_run_whole_corpus_matches_treewalker`) is a
// LIVE differential: it compares the VM against the tree-walker, both running
// right now. That gate is only meaningful while the tree-walker still exists.
//
// At the eventual cutover the tree-walker is DELETED. Oracle #1 then has no
// reference engine to diff against. The recorded goldens below are oracle #2:
// stdout + exit code captured FROM THE TREE-WALKER (the current source of
// truth) and committed under `tests/vm_goldens/`. After the tree-walker is
// gone, the VM must keep reproducing these byte-for-byte. They are the
// post-cutover reference.
//
// SCOPE: exactly the corpus examples the VM runs byte-identically today — i.e.
// `all_corpus_examples()` minus `EXAMPLE_SKIPS` (the same set oracle #1 runs).
// The golden set therefore GROWS automatically as V12 unblocks `import`
// examples and entries leave `EXAMPLE_SKIPS`: a newly-unskipped example fails
// `vm_recorded_goldens_cover_the_byte_identical_set` (missing golden) until its
// `.out` is recorded. The set never silently shrinks.
//
// REGENERATION (intentional only — never automatic):
//   cargo test --test vm_differential record_vm_goldens -- --ignored --nocapture
// That re-records every golden FROM THE TREE-WALKER. Run it ONLY when you have
// deliberately changed example output (or unblocked new examples) and have
// confirmed the new output is correct, then commit the changed `.out` files.
// The checker test (`vm_matches_recorded_goldens`) does NOT auto-regenerate —
// auto-regeneration would defeat the entire point of a frozen reference.

/// Directory holding the committed golden `.out` files, one per byte-identical
/// corpus example.
fn vm_goldens_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vm_goldens")
}

/// Map a corpus-relative example path (e.g. `examples/advanced/foo.as`) to its
/// golden file path. The relative path is flattened with `__` so the goldens
/// live in a single flat directory while staying collision-free across
/// `examples/` and `examples/advanced/`.
fn golden_path_for(rel: &str) -> std::path::PathBuf {
    let flat = rel.replace('/', "__");
    vm_goldens_dir().join(format!("{flat}.out"))
}

/// Encode (stdout, exit) into the on-disk golden format. The first line is a
/// machine-readable header `# exit: none` or `# exit: <N>`; everything after
/// the first `\n` is the verbatim stdout (which may contain anything, including
/// `#`/newlines). This keeps the exit code unambiguous without escaping stdout.
fn encode_golden(out: &str, exit: Option<i32>) -> String {
    let header = match exit {
        None => "# exit: none".to_string(),
        Some(n) => format!("# exit: {n}"),
    };
    format!("{header}\n{out}")
}

/// Inverse of [`encode_golden`]. Returns `(stdout, exit)`.
fn decode_golden(text: &str) -> (String, Option<i32>) {
    let (header, body) = text
        .split_once('\n')
        .unwrap_or_else(|| panic!("golden missing header line: {text:?}"));
    let exit = header
        .strip_prefix("# exit: ")
        .unwrap_or_else(|| panic!("golden header malformed: {header:?}"));
    let exit = match exit {
        "none" => None,
        n => Some(
            n.parse::<i32>()
                .unwrap_or_else(|e| panic!("golden exit code `{n}` not an integer: {e}")),
        ),
    };
    (body.to_string(), exit)
}

/// The set of examples for which a golden must exist: every corpus example NOT
/// on the `EXAMPLE_SKIPS` list (the byte-identical set, identical to oracle #1's
/// run set). Sorted, deterministic.
fn byte_identical_examples() -> Vec<String> {
    all_corpus_examples()
        .into_iter()
        .filter(|rel| skip_reason(rel).is_none())
        .collect()
}

/// ORACLE #2 — the recorded goldens checker. For every byte-identical corpus
/// example, run it through the VM and assert stdout+exit equals the committed
/// golden recorded from the tree-walker. This is the assertion that must keep
/// holding AFTER the tree-walker is deleted.
///
/// On drift this fails LOUDLY with the example name and a stdout/exit diff. It
/// does NOT regenerate the golden — see the regeneration note above.
#[tokio::test]
async fn vm_matches_recorded_goldens() {
    let mut checked = 0usize;
    for rel in byte_identical_examples() {
        let src =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&rel))
                .unwrap_or_else(|e| panic!("read example {rel}: {e}"));
        // Goldens are recorded under the full-feature build; under a reduced
        // feature config the example may import an unavailable module and cannot
        // reproduce its golden. Skip it for this build (covered by the full build).
        if feature_unavailable_in_this_build(&src).await {
            continue;
        }
        let gpath = golden_path_for(&rel);
        let golden_text = std::fs::read_to_string(&gpath).unwrap_or_else(|e| {
            panic!(
                "missing recorded golden for byte-identical example `{rel}` \
                 (expected at {}): {e}\n\
                 If this example was just unblocked (left EXAMPLE_SKIPS), record \
                 its golden from the tree-walker with:\n  \
                 cargo test --test vm_differential record_vm_goldens -- --ignored --nocapture",
                gpath.display()
            )
        });
        let (want_out, want_exit) = decode_golden(&golden_text);
        let (got_out, got_exit) = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("VM failed on `{rel}` (golden expected it to run): {e:?}"));
        assert_eq!(
            (got_out.as_str(), got_exit),
            (want_out.as_str(), want_exit),
            "VM output drifted from the recorded golden for `{rel}`.\n\
             This means the VM no longer reproduces the tree-walker's frozen \
             reference output. Either the VM regressed (fix it) OR the change \
             is intentional (then regenerate the golden from the tree-walker — \
             see the regeneration note in tests/vm_differential.rs — and commit \
             the updated {}).\n  golden stdout: {want_out:?}\n  vm stdout:     {got_out:?}\n  \
             golden exit: {want_exit:?}\n  vm exit:     {got_exit:?}",
            golden_path_for(&rel).display()
        );
        checked += 1;
    }
    // The goldens must actually cover the bulk of the corpus — guard against a
    // silently-empty golden dir making the check vacuous.
    assert!(
        checked >= 18,
        "expected >=18 recorded goldens checked against the VM, checked {checked}"
    );
    eprintln!("oracle #2: {checked} recorded goldens reproduced by the VM");
}

/// Guard that keeps oracle #2 HONEST: there must be exactly one golden file per
/// byte-identical example and NO stale golden files (e.g. for an example that
/// was renamed or re-skipped). Combined with `vm_matches_recorded_goldens`
/// (which fails on a MISSING golden), this pins the golden set to the
/// byte-identical set exactly.
#[test]
fn vm_recorded_goldens_cover_the_byte_identical_set() {
    use std::collections::BTreeSet;
    let expected: BTreeSet<std::path::PathBuf> = byte_identical_examples()
        .iter()
        .map(|rel| golden_path_for(rel))
        .collect();
    let dir = vm_goldens_dir();
    let mut found = BTreeSet::new();
    for entry in
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|x| x.to_str()) == Some("out") {
            found.insert(path);
        }
    }
    let missing: Vec<_> = expected.difference(&found).collect();
    let stale: Vec<_> = found.difference(&expected).collect();
    assert!(
        missing.is_empty(),
        "byte-identical examples without a recorded golden (record from the \
         tree-walker with `cargo test --test vm_differential record_vm_goldens \
         -- --ignored --nocapture`): {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "stale golden files with no matching byte-identical example (delete \
         them, or fix the path): {stale:?}"
    );
}

/// REGENERATOR (ignored by default — run intentionally only). Re-records every
/// golden FROM THE TREE-WALKER (the current source of truth) into
/// `tests/vm_goldens/`. The goldens are oracle #2's frozen post-cutover
/// reference, so this is deliberately NOT part of the normal test run.
///
///   cargo test --test vm_differential record_vm_goldens -- --ignored --nocapture
///
/// After running, `git diff tests/vm_goldens/` shows exactly what changed;
/// commit it only when the new output is intended.
#[tokio::test]
#[ignore = "regenerates committed goldens from the tree-walker; run intentionally"]
async fn record_vm_goldens() {
    let dir = vm_goldens_dir();
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let root = env!("CARGO_MANIFEST_DIR");
    let mut wrote = 0usize;
    for rel in byte_identical_examples() {
        let src = std::fs::read_to_string(std::path::Path::new(root).join(&rel))
            .unwrap_or_else(|e| panic!("read example {rel}: {e}"));
        // RECORD FROM THE TREE-WALKER — the source of truth the VM must match.
        let (out, exit) = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed recording golden for {rel}: {e:?}"));
        let gpath = golden_path_for(&rel);
        std::fs::write(&gpath, encode_golden(&out, exit))
            .unwrap_or_else(|e| panic!("write golden {}: {e}", gpath.display()));
        eprintln!("recorded golden: {} <- {rel}", gpath.display());
        wrote += 1;
    }
    eprintln!(
        "recorded {wrote} goldens from the tree-walker into {}",
        dir.display()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  V11-T3: INLINE-CACHE differential tests.
//
//  These hammer the GET_PROP/SET_PROP/CALL_METHOD inline caches across the full
//  state machine (cold→mono→poly→mega), through mutation-during-loop, with a
//  field that shadows a method, and with a schema receiver (which must bypass the
//  field IC). Every program's stdout must stay BYTE-IDENTICAL to the tree-walker
//  — the ICs are a pure fast path in front of the generic member/dispatch logic,
//  so any divergence is a guard bug (do NOT relax the assertion to make it pass).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ic_monomorphic_field_read_hot_loop() {
    // One shape seen at the GET_PROP site for thousands of iterations → the IC
    // sits in Mono and the running sum must equal the tree-walker's.
    assert_vm_run_matches_treewalker(
        r#"
        let total = 0
        for (i in 0..1000) {
          let o = { x: i, y: i + 1 }
          total = total + o.x + o.y
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_polymorphic_field_read_two_to_four_shapes() {
    // The SAME GET_PROP site sees 2..4 distinct object SHAPES (different key
    // layouts) → the IC promotes Mono→Poly. The read must pick the right index
    // per shape.
    assert_vm_run_matches_treewalker(
        r#"
        fn val(o) { return o.v }
        let total = 0
        for (i in 0..400) {
          let o = nil
          let m = i % 4
          if (m == 0) { o = { v: i } }
          else if (m == 1) { o = { a: 1, v: i } }
          else if (m == 2) { o = { a: 1, b: 2, v: i } }
          else { o = { a: 1, b: 2, c: 3, v: i } }
          total = total + val(o)
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_megamorphic_field_read_five_plus_shapes() {
    // >4 distinct shapes at one GET_PROP site → the IC saturates to Mega and
    // always takes the generic path; the result must still match exactly.
    assert_vm_run_matches_treewalker(
        r#"
        fn val(o) { return o.v }
        let total = 0
        for (i in 0..600) {
          let o = nil
          let m = i % 6
          if (m == 0) { o = { v: i } }
          else if (m == 1) { o = { a: 1, v: i } }
          else if (m == 2) { o = { a: 1, b: 2, v: i } }
          else if (m == 3) { o = { a: 1, b: 2, c: 3, v: i } }
          else if (m == 4) { o = { a: 1, b: 2, c: 3, d: 4, v: i } }
          else { o = { a: 1, b: 2, c: 3, d: 4, e: 5, v: i } }
          total = total + val(o)
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_set_prop_reassign_existing_field_in_loop() {
    // Reassigning an EXISTING object field keeps the shape, so the SET_PROP IC
    // fast path writes in place. The accumulated value must match.
    assert_vm_run_matches_treewalker(
        r#"
        let o = { count: 0, tag: "n" }
        for (i in 0..500) {
          o.count = o.count + i
        }
        print(o.count)
        print(o.tag)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_set_prop_adds_new_key_invalidates_then_recaches() {
    // Adding a NEW key transitions the object's shape; a subsequent read of an
    // existing field must still be correct (the old cache entry misses on the
    // new shape and re-resolves).
    assert_vm_run_matches_treewalker(
        r#"
        fn x(o) { return o.x }
        let total = 0
        for (i in 0..300) {
          let o = { x: i }
          total = total + x(o)
          o.y = i * 2          // shape transition
          total = total + x(o) // x is still at index 0 under the new shape
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_mutation_during_loop_object_field() {
    // A long-lived object whose field is mutated each iteration AND read through
    // the same GET_PROP — the IC must observe the latest value every time.
    assert_vm_run_matches_treewalker(
        r#"
        let acc = { sum: 0 }
        let seen = 0
        for (i in 0..1000) {
          acc.sum = acc.sum + i
          seen = acc.sum
        }
        print(seen)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_instance_field_read_and_write_hot() {
    // Instance field GET_PROP/SET_PROP in a loop — the field IC fast-paths the
    // declared field reads/writes; the field-type contract still applies.
    assert_vm_run_matches_treewalker(
        r#"
        class Point {
          x: number
          y: number
          fn init(x, y) { self.x = x; self.y = y }
          fn sum() { return self.x + self.y }
        }
        let total = 0
        for (i in 0..1000) {
          let p = Point(i, i + 1)
          p.x = p.x + 1
          total = total + p.x + p.y
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_method_dispatch_monomorphic_hot() {
    // One receiver class at the CALL_METHOD site for many iterations → the method
    // IC sits in Mono and dispatches the compiled method directly.
    assert_vm_run_matches_treewalker(
        r#"
        class Counter {
          n: number = 0
          fn bump(d) { self.n = self.n + d; return self.n }
        }
        let c = Counter()
        let last = 0
        for (i in 0..1000) { last = c.bump(i) }
        print(last)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_method_dispatch_polymorphic_across_classes() {
    // The SAME CALL_METHOD site sees TWO different classes (which can even share a
    // field layout) — the method IC keys on CLASS identity, so each dispatches its
    // own `kind`/`area` and the totals must match.
    assert_vm_run_matches_treewalker(
        r#"
        class Sq {
          s: number
          fn init(s) { self.s = s }
          fn area() { return self.s * self.s }
        }
        class Rec {
          s: number
          fn init(s) { self.s = s }
          fn area() { return self.s * 2 }
        }
        fn area_of(shape) { return shape.area() }
        let total = 0
        for (i in 0..400) {
          let shape = nil
          if (i % 2 == 0) { shape = Sq(i) } else { shape = Rec(i) }
          total = total + area_of(shape)
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_method_dispatch_through_inheritance() {
    // An inherited method resolved up the chain must cache + dispatch from the
    // correct defining class (so a `super`-style resolution stays correct).
    assert_vm_run_matches_treewalker(
        r#"
        class Animal {
          name: string
          fn init(name) { self.name = name }
          fn speak() { return self.name + " makes a sound" }
        }
        class Dog extends Animal {
          fn init(name) { super.init(name) }
        }
        let out = ""
        for (i in 0..50) {
          let d = Dog("Rex")
          out = d.speak()
        }
        print(out)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_field_shadows_method_is_not_fast_pathed() {
    // An object/instance member that is a FIELD holding a callable must be read as
    // a field (NOT dispatched as a method) — the IC fast path must decline. Here a
    // class field holds a closure; calling it must invoke the closure WITHOUT
    // `self`-binding, exactly like the tree-walker.
    assert_vm_run_matches_treewalker(
        r#"
        class Holder {
          op: fn
          fn init() { self.op = (a, b) => a + b }
        }
        let h = Holder()
        let total = 0
        for (i in 0..100) { total = total + h.op(i, 1) }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_method_name_read_without_call_returns_bound_method() {
    // GET_PROP of a METHOD name (no call) must yield a bound method, not a wrong
    // field value — the field IC must miss on a method name. We exercise the bound
    // method by calling it after the read.
    assert_vm_run_matches_treewalker(
        r#"
        class Box {
          v: number
          fn init(v) { self.v = v }
          fn get() { return self.v }
        }
        let b = Box(41)
        let m = b.get      // method-named GET_PROP (no call) → bound method
        print(m())
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_schema_value_member_access_bypasses_field_ic() {
    // A schema value is a tagged Object `{__kind: "<kind>", ...}`; reading a stored
    // constraint field on it must go through the GENERIC path (the field IC bypasses
    // schema receivers via `is_schema_value`). We build the tagged object directly
    // (the VM does not yet support `import`), then read its fields in a hot loop so
    // the GET_PROP site would WANT to cache — it must not, and the output must match
    // the tree-walker regardless.
    assert_vm_run_matches_treewalker(
        r#"
        let total = 0
        for (i in 0..200) {
          let s = { __kind: "string", minLength: i }
          total = total + s.minLength
        }
        print(total)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_optional_chain_nil_receiver_short_circuits() {
    // `?.` on a nil receiver must short-circuit to nil WITHOUT consulting the IC
    // or resolving a field, byte-identically to the tree-walker.
    assert_vm_run_matches_treewalker(
        r#"
        let o = nil
        print(o?.missing)
        let p = { a: 1 }
        print(p?.a)
        "#,
    )
    .await;
}

#[tokio::test]
async fn ic_mixed_object_and_instance_same_site() {
    // The same GET_PROP `.v` site sees BOTH a plain object and an instance across
    // iterations (poly across an object shape and an instance shape). The read
    // must resolve the right index for each.
    assert_vm_run_matches_treewalker(
        r#"
        class Cell {
          v: number
          fn init(v) { self.v = v }
        }
        fn val(o) { return o.v }
        let total = 0
        for (i in 0..400) {
          let o = nil
          if (i % 2 == 0) { o = { v: i } } else { o = Cell(i) }
          total = total + val(o)
        }
        print(total)
        "#,
    )
    .await;
}

// ─────────────────────────────────────────────────────────────────────────────
//  V11-T5: THE THREE-WAY DIFFERENTIAL (the specialization SAFETY NET).
//
//  The `--no-specialize` kill switch (`Vm::new_generic` / `vm_run_source_generic`,
//  `specialize = false`) disables EVERY specialization fast path: the polymorphic
//  field/method inline caches (GET_PROP/SET_PROP/CALL_METHOD) and the PEP-659
//  adaptive arithmetic + GET_GLOBAL caches. A non-specializing run takes only the
//  generic dispatch — no IC consult/record, no adaptive warmup/specialize/deopt.
//
//  THE INVARIANT, asserted byte-for-byte over the whole corpus + goldens + an
//  IC/adaptive/property/method/arithmetic-HEAVY program set:
//
//      tree-walker  ==  specialized-VM  ==  generic-VM
//
//  Both VM modes MUST be correct and identical; the only legitimate difference is
//  SPEED. If `generic` and `specialized` ever diverge, a specialization GUARD is
//  WRONG — that is precisely the bug this net exists to catch. Do NOT "fix" such a
//  failure by relaxing the assertion: investigate the guard.
// ─────────────────────────────────────────────────────────────────────────────

/// Run `src` on all THREE engines and assert byte-identical outcomes:
/// tree-walker == specialized-VM == generic-VM. The outcome of each engine is
/// normalized to `Ok((stdout, exit))` or `Err(rendered-message)`, so a faulting
/// program (panic) compares too — a panic in one engine but not the others is
/// itself a divergence and fails the assertion.
async fn assert_three_way_matches(src: &str) {
    let tw = ascript::run_source_exit(src).await;
    let spec = ascript::vm_run_source(src).await;
    let generic = ascript::vm_run_source_generic(src).await;

    let norm = |r: &Result<(String, Option<i32>), ascript::error::AsError>| match r {
        Ok((out, code)) => Ok((out.clone(), *code)),
        Err(e) => Err(e.to_string()),
    };
    let (tw_n, spec_n, gen_n) = (norm(&tw), norm(&spec), norm(&generic));

    // The load-bearing assertion of the whole task: generic == specialized.
    assert_eq!(
        spec_n, gen_n,
        "SPECIALIZATION DIVERGENCE (generic VM != specialized VM) — a real guard \
         bug surfaced by the kill switch.\n  src: {src:?}\n  specialized: {spec_n:?}\n  \
         generic:     {gen_n:?}"
    );
    // And both VM modes must equal the tree-walker (the ground truth).
    assert_eq!(
        tw_n, spec_n,
        "VM (specialized) diverged from the tree-walker.\n  src: {src:?}\n  \
         tree-walker: {tw_n:?}\n  vm:          {spec_n:?}"
    );
    assert_eq!(
        tw_n, gen_n,
        "VM (generic / --no-specialize) diverged from the tree-walker.\n  src: {src:?}\n  \
         tree-walker: {tw_n:?}\n  vm:          {gen_n:?}"
    );
}

#[tokio::test]
async fn srv_shared_freeze_reads_and_mutation_panic_three_way() {
    // SRV §5 (the data half): `shared.freeze` + reads + the mutation panic are pure
    // Value-layer logic shared by both engines, so they MUST be byte-identical across
    // tree-walker == specialized VM == generic VM.
    // Reads: scalar / descend (Shared view) / index / read-only method / iterate / len.
    assert_three_way_matches(
        "import { freeze } from \"std/shared\"\n\
         let cfg = freeze({ region: \"us\", flags: { beta: true }, limits: [10, 100] })\n\
         print(cfg.region)\n\
         print(cfg.flags.beta)\n\
         print(cfg.limits[0])\n\
         print(cfg[\"region\"])\n\
         print(len(cfg.limits))\n\
         print(cfg.has(\"region\"))\n\
         print(cfg.get(\"missing\", \"dflt\"))\n\
         print(type(cfg.flags))\n\
         let s = 0\n\
         for (l of cfg.limits) { s = s + l }\n\
         print(s)\n\
         print(cfg.missing)\n",
    )
    .await;
    // Idempotence: freeze(freeze(x)) is the SAME Arc → `==` is true.
    assert_three_way_matches(
        "import { freeze } from \"std/shared\"\n\
         let c = freeze({ x: 1 })\n\
         print(freeze(c) == c)\n",
    )
    .await;
    // Mutation panics (order-deterministic), caught by recover — the SHIPPED
    // `cannot mutate a frozen {kind}` wording, byte-identical on all three modes.
    assert_three_way_matches(
        "import { freeze } from \"std/shared\"\n\
         let cfg = freeze({ region: \"us\", limits: [10, 100] })\n\
         print(recover(() => { cfg.region = \"eu\" })[1])\n\
         print(recover(() => { cfg[\"region\"] = \"eu\" })[1])\n\
         print(recover(() => { cfg.limits[0] = 5 })[1])\n\
         print(recover(() => cfg.limits.push(3))[1])\n",
    )
    .await;
}

#[tokio::test]
async fn three_way_whole_corpus_generic_equals_specialized_equals_treewalker() {
    // THE central safety net over the WHOLE corpus (same non-skipped set as oracle
    // #1). For every non-skipped example, all three engines must agree byte-for-byte.
    let root = env!("CARGO_MANIFEST_DIR");
    let mut ran = 0usize;
    for rel in all_corpus_examples() {
        if skip_reason(&rel).is_some() {
            continue;
        }
        let path = std::path::Path::new(root).join(&rel);
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read example {rel}: {e}"));

        // Skip examples whose imported modules are unavailable in this build.
        if feature_unavailable_in_this_build(&src).await {
            continue;
        }

        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on non-skipped {rel}: {e:?}"));
        let spec = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("specialized VM failed on non-skipped {rel}: {e:?}"));
        let generic = ascript::vm_run_source_generic(&src)
            .await
            .unwrap_or_else(|e| panic!("generic VM failed on non-skipped {rel}: {e:?}"));

        assert_eq!(
            spec, generic,
            "SPECIALIZATION DIVERGENCE on `{rel}` (generic VM != specialized VM) — a \
             real guard bug.\n  specialized: {spec:?}\n  generic:     {generic:?}"
        );
        assert_eq!(
            tw, spec,
            "specialized VM diverged from tree-walker on `{rel}`\n  tw: {tw:?}\n  vm: {spec:?}"
        );
        assert_eq!(
            tw, generic,
            "generic VM diverged from tree-walker on `{rel}`\n  tw: {tw:?}\n  vm: {generic:?}"
        );
        ran += 1;
    }
    let floor = if cfg!(feature = "data") { 38 } else { 1 };
    assert!(
        ran >= floor,
        "expected the three-way gate to run >={floor} examples, ran {ran}"
    );
    eprintln!("three-way whole-corpus gate: {ran} examples generic==specialized==tree-walker");
}

#[tokio::test]
async fn three_way_recorded_goldens_generic_equals_specialized() {
    // Oracle #2 (recorded goldens) in three-way form: the generic VM must reproduce
    // the SAME committed golden the specialized VM does — both equal the tree-walker
    // snapshot. A generic-only drift from the golden is a specialization bug too.
    let root = env!("CARGO_MANIFEST_DIR");
    let mut checked = 0usize;
    for rel in byte_identical_examples() {
        let path = std::path::Path::new(root).join(&rel);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));

        // Skip examples whose imported modules are unavailable in this build.
        if feature_unavailable_in_this_build(&src).await {
            continue;
        }

        let gpath = golden_path_for(&rel);
        let golden_text = std::fs::read_to_string(&gpath).unwrap_or_else(|e| {
            panic!("missing recorded golden for `{rel}`: {e} (record with record_vm_goldens)")
        });
        let (want_out, want_exit) = decode_golden(&golden_text);

        let spec = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("specialized VM failed on `{rel}`: {e:?}"));
        let generic = ascript::vm_run_source_generic(&src)
            .await
            .unwrap_or_else(|e| panic!("generic VM failed on `{rel}`: {e:?}"));

        assert_eq!(
            spec, generic,
            "SPECIALIZATION DIVERGENCE on golden `{rel}`\n  specialized: {spec:?}\n  generic: {generic:?}"
        );
        assert_eq!(
            (want_out.clone(), want_exit),
            spec,
            "specialized VM drifted from recorded golden `{rel}`"
        );
        assert_eq!(
            (want_out, want_exit),
            generic,
            "generic VM drifted from recorded golden `{rel}`"
        );
        checked += 1;
    }
    assert!(
        checked >= 18,
        "expected >=18 goldens checked three-way, checked {checked}"
    );
    eprintln!("three-way goldens: {checked} reproduced by BOTH generic and specialized VM");
}

#[tokio::test]
async fn three_way_ic_and_adaptive_heavy_programs() {
    // Programs deliberately constructed to exercise the IC and adaptive fast paths
    // through their full state machines under BOTH modes: field reads/writes that
    // go cold→mono→poly→mega, method dispatch (mono / polymorphic-across-classes /
    // inherited / field-shadows-method), key-adding mutation that invalidates the
    // shape, schema receivers (which bypass the field IC), optional chaining, plus
    // arithmetic that warms up to Number/Decimal/concat and then DEOPTs on a mixed
    // operand. With specialization OFF none of this machinery runs — yet the output
    // MUST be byte-identical to the specialized run (and the tree-walker).
    let progs: &[&str] = &[
        // Monomorphic field read in a hot loop (Mono IC).
        "let o = { v: 7 }\nlet t = 0\nfor (i in range(0, 2000)) { t = t + o.v }\nprint(t)",
        // Polymorphic field read across several shapes at one site (Poly→Mega).
        "fn pick(i) { if (i % 3 == 0) { return { a: i } }\n if (i % 3 == 1) { return { a: i, b: 1 } }\n return { a: i, b: 1, c: 2 } }\n\
         let s = 0\nfor (i in range(0, 600)) { s = s + pick(i).a }\nprint(s)",
        // SET_PROP reassign existing field (in-place) then add a new key (shape change).
        "let o = { x: 0 }\nfor (i in range(0, 500)) { o.x = o.x + i }\no.y = 99\nprint(o.x)\nprint(o.y)",
        // Instance field hot read+write through declared-field contracts.
        "class P { x: number\n fn init(v) { self.x = v } }\nlet p = P(0)\nfor (i in range(0, 1000)) { p.x = p.x + 1 }\nprint(p.x)",
        // Monomorphic method dispatch in a hot loop (method IC Mono).
        "class C { fn bump(n) { return n + 1 } }\nlet c = C()\nlet t = 0\nfor (i in range(0, 1500)) { t = c.bump(t) }\nprint(t)",
        // Polymorphic method dispatch across distinct classes at one call site.
        "class A { fn k() { return 1 } }\nclass B { fn k() { return 2 } }\n\
         fn pick(i) { if (i % 2 == 0) { return A() } return B() }\n\
         let s = 0\nfor (i in range(0, 800)) { s = s + pick(i).k() }\nprint(s)",
        // Inherited method dispatch (resolves up the chain, ANCESTOR defining class).
        "class Animal { fn speak() { return 1 } }\nclass Dog extends Animal {}\n\
         let d = Dog()\nlet s = 0\nfor (i in range(0, 500)) { s = s + d.speak() }\nprint(s)",
        // A field SHADOWS a method — the IC must NOT fast-path it (reads the field).
        "class K { fn v() { return 1 } }\nlet k = K()\nk.v = 42\nprint(k.v)",
        // Optional chain with nil receiver short-circuits (never consults the IC).
        "let o = nil\nprint(o?.x)\nlet p = { x: 5 }\nprint(p?.x)",
        // Mixed Object/Instance at the SAME GET_PROP site (interleaved shapes).
        "class Cell { v: number\n fn init(n) { self.v = n } }\n\
         fn val(o) { return o.v }\n\
         let total = 0\nfor (i in range(0, 400)) {\n let o = nil\n if (i % 2 == 0) { o = { v: i } } else { o = Cell(i) }\n total = total + val(o)\n }\nprint(total)",
        // Adaptive arithmetic: warm up to Number, stay Number (Number fast path).
        "let t = 0\nfor (i in range(0, 3000)) { t = t + i * 2 - 1 }\nprint(t)",
        // Adaptive arithmetic across kinds at one offset (number then concat → deopt).
        "fn add(a, b) { return a + b }\nprint(add(1, 2))\nprint(add(\"x\", \"y\"))\nprint(add(3, 4))\nprint(add(\"p\", \"q\"))",
        // GET_GLOBAL cache: a builtin referenced repeatedly in a hot loop.
        "let t = 0\nfor (i in range(0, 1000)) { t = t + len([1, 2, 3]) }\nprint(t)",
    ];
    for src in progs {
        assert_three_way_matches(src).await;
    }
}

#[tokio::test]
async fn three_way_smoke_basic_constructs() {
    // A small spread of language constructs so the three-way invariant is checked
    // even outside the corpus/IC sets (arithmetic, strings, control flow, errors).
    let progs: &[&str] = &[
        "print(1 + 2 * 3 - 4 / 2)",
        "print(`x=${1 + 1} y=${\"a\" + \"b\"}`)",
        "let a = [1, 2, 3]\nprint(a[1])\nprint(len(a))",
        "fn f(n: number) { return n }\nprint(f(\"bad\"))", // contract panic — same on all three
        "fn parse(ok) { if (ok) { return Ok(1) } return Err(\"e\") }\nfn u(ok) { let v = parse(ok)?\n return v }\nprint(u(true))",
        "exit(3)",
    ];
    for src in progs {
        assert_three_way_matches(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SP1 Phase B — generator methods (`fn*` / `async fn*`) in classes.
//
//  A `fn*`/`async fn*` method dispatched as `c.g(args)` returns a generator bound
//  to the instance `self`, behaving exactly like a standalone generator (yield /
//  .next() / for await / close) with method dispatch + arity/contracts + inherit/
//  override. Both engines gain the feature in lockstep — byte-identical.
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn three_way_generator_methods() {
    let progs: &[&str] = &[
        // Basic fn* method consumed via `for await`.
        "class C { fn* g() { yield 1\n yield 2\n yield 3 } }\nlet c = C()\nfor await (v in c.g()) { print(v) }\n",
        // fn* method using `self` (set in init), consumed via `.next()`.
        "class C { fn init() { self.n = 10 }\n fn* g() { yield self.n\n yield self.n + 1 } }\n\
         let c = C()\nlet it = c.g()\nprint(it.next())\nprint(it.next())\n",
        // async fn* method (yield + await), consumed via `for await`.
        "class C { async fn* g() { yield 1\n let x = await 2\n yield x } }\n\
         let c = C()\nfor await (v in c.g()) { print(v) }\n",
        // Inheritance / override: subclass overrides a fn* method.
        "class A { fn* g() { yield 1 } }\nclass B extends A { fn* g() { yield 2\n yield 3 } }\n\
         for await (v in B().g()) { print(v) }\n",
        // Inheritance WITHOUT override: subclass inherits the parent's fn* method.
        "class A { fn* g() { yield 7\n yield 8 } }\nclass B extends A {}\n\
         for await (v in B().g()) { print(v) }\n",
        // Generator method with args + self interacting.
        "class C { fn init() { self.base = 100 }\n fn* g(a, b) { yield self.base + a\n yield self.base + b } }\n\
         let c = C()\nfor await (v in c.g(1, 2)) { print(v) }\n",
        // .close() on a generator method handle.
        "class C { fn* g() { yield 1\n yield 2\n yield 3 } }\nlet c = C()\nlet it = c.g()\n\
         print(it.next())\nit.close()\nprint(\"closed\")\n",
    ];
    for src in progs {
        assert_three_way_matches(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SP1 Phase C — static methods (`static fn` / `static async fn` / `static fn*`).
//  Called as `C.name(args)` with NO receiver; stored in a separate namespace;
//  inherited up the superclass chain. `static fn from` is reserved; `super` in a
//  static is invalid. Every case is byte-identical tree-walker == spec-VM ==
//  generic-VM (the helper normalizes Ok/Err so error cases compare too).
// ─────────────────────────────────────────────────────────────────────────────

/// The canonical Phase-C static-method program set (sync / static-calls-static /
/// inheritance / override / instance+static coexistence / async factory / `fn*` /
/// `from` coexists / unknown-static error / `static fn from` reserved). Shared by
/// the tree-walker-side check (C4) and the three-way check (C5).
const STATIC_METHOD_PROGRAMS: &[&str] = &[
    // Sync static factory-ish: `C.make()` constructs and returns an instance.
    "class C { fn init() { self.x = 1 } static fn make() { return C() } }\nlet c = C.make()\nprint(c.x)\n",
    // A static calling ANOTHER static + constructing C().
    "class C { fn init() { self.x = 0 } static fn one() { return 1 } static fn make() { let c = C()\n c.x = C.one()\n return c } }\nprint(C.make().x)\n",
    // Inheritance: a subclass resolves a parent static up the chain.
    "class A { static fn who() { return \"A\" } }\nclass B extends A {}\nprint(B.who())\n",
    // Inheritance with override: the subclass's own static wins.
    "class A { static fn who() { return \"A\" } }\nclass B extends A { static fn who() { return \"B\" } }\nprint(A.who())\nprint(B.who())\n",
    // Instance + static SAME name coexist (separate namespaces).
    "class C { fn init() { self.v = 9 } fn x() { return self.v } static fn x() { return 42 } }\nlet c = C()\nprint(c.x())\nprint(C.x())\n",
    // The blessed async factory: `static async fn create()` awaited → instance.
    "class C { fn init() { self.x = 0 } static async fn create() { let c = C()\n c.x = await 5\n return c } }\nlet c = await C.create()\nprint(c.x)\n",
    // `static fn*` generator consumed via for-await.
    "class C { static fn* seq() { yield 1\n yield 2\n yield 3 } }\nfor await (v in C.seq()) { print(v) }\n",
    // The built-in `from` still works alongside user statics.
    "class P { name: string = \"?\" static fn tag() { return \"P\" } }\nlet p = P.from({name: \"ok\"})\nprint(p.name)\nprint(P.tag())\n",
    // Unknown static member (read) → identical "no static member" error on all
    // three engines. (Read form, not `C.nope()`: a member-callee CALL anchors its
    // member-read error at the call span on the VM but the receiver span on the
    // tree-walker — a PRE-EXISTING CALL_METHOD span deviation orthogonal to static
    // methods; the read form exercises the static-namespace miss byte-identically.)
    "class C {}\nlet f = C.nope\n",
    // `static fn from` is reserved → identical compile/resolve error.
    "class C { static fn from() { return 1 } }\nprint(C.from())\n",
];

/// C4 — the tree-walker (reference engine) runs every static-method program to a
/// definite outcome (success or a deliberate error), establishing the oracle the
/// VM must match in C5.
#[tokio::test]
async fn tree_walker_static_methods() {
    for src in STATIC_METHOD_PROGRAMS {
        // Just exercising the path — a panic-free `run_source_exit` (Ok or Err)
        // both count; the three-way equality is asserted in `three_way_static_methods`.
        let _ = ascript::run_source_exit(src).await;
    }
    // Spot-check the headline behaviors so this test is meaningful on its own.
    let (out, _) = ascript::run_source_exit(STATIC_METHOD_PROGRAMS[0])
        .await
        .expect("C.make() runs on the tree-walker");
    assert_eq!(out, "1\n");
    let from_reserved = ascript::run_source_exit(STATIC_METHOD_PROGRAMS[9]).await;
    assert!(
        matches!(&from_reserved, Err(e) if e.message.contains("'from' is reserved")),
        "`static fn from` is rejected on the tree-walker, got {from_reserved:?}"
    );
}

/// C5 — every static-method program is byte-identical across all three engines
/// (tree-walker == specialized-VM == generic-VM), including the async factory,
/// `fn*`, inheritance/override, instance+static coexistence, and the two error
/// cases (unknown static, `static fn from` reserved).
#[tokio::test]
async fn three_way_static_methods() {
    for src in STATIC_METHOD_PROGRAMS {
        assert_three_way_matches(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SP1 Phase D — `async fn init` / `fn* init` forbidden (both engines).
//
//  Synchronous construction (`C()` returns an instance, not a future) means there
//  is no caller to `await` an async constructor, and a generator constructor makes
//  no sense either. Per spec §3, an `init` declared `async` (or `fn*`) is a clean
//  compile/resolve-time error with the IDENTICAL message + exit on BOTH engines.
//  The blessed async-construction pattern is a static async factory.
//
//  Today the engines DIVERGE on `async fn init` (the VM ran the body; the
//  tree-walker left fields nil) — these cases assert that both now reject it
//  byte-identically, and that a `static async fn create()` factory is unaffected.
// ─────────────────────────────────────────────────────────────────────────────
const FORBIDDEN_INIT_PROGRAMS: &[&str] = &[
    // async fn init — the headline divergence: must be a clean compile/resolve error.
    "class C { async fn init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n",
    // fn* init — a generator constructor is equally forbidden.
    "class C { fn* init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n",
    // async fn* init — both modifiers at once is still forbidden.
    "class C { async fn* init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n",
];

/// D1 — `async fn init` / `fn* init` are rejected byte-identically on all three
/// engines (tree-walker == specialized-VM == generic-VM), while the blessed
/// `static async fn create()` factory from Phase C still works.
#[tokio::test]
async fn three_way_forbidden_init() {
    for src in FORBIDDEN_INIT_PROGRAMS {
        assert_three_way_matches(src).await;
    }
    // The error message is the spec-mandated one (spot-checked on the tree-walker).
    let err = ascript::run_source_exit(FORBIDDEN_INIT_PROGRAMS[0]).await;
    assert!(
        matches!(&err, Err(e) if e.message.contains(
            "init must be a synchronous constructor; use a static async factory")),
        "async fn init is rejected with the spec message, got {err:?}"
    );
    // The blessed alternative — a static async factory — is unaffected.
    assert_three_way_matches(
        "class C { fn init() { self.x = 0 } static async fn create() { let c = C()\n c.x = await 5\n return c } }\nlet c = await C.create()\nprint(c.x)\n",
    )
    .await;
}

// ─────────────────────────────────────────────────────────────────────────────
//  SP1 Phase F — parser-accepts-runs invariant gate.
//
//  THE closing invariant of SP1: every grammar-accepted construct that SP1 touched
//  must RUN on the VM (no "parser accepts but engine rejects valid code" hole) and
//  be byte-identical to the tree-walker oracle. The front-end audit (spec §5) found
//  exactly two such holes — `a?.m()` optional calls and `fn*` generator methods —
//  and §§1–3 closed them; statics and the richer field-default forms widened the
//  accepted surface further. This curated set re-asserts that the whole surface
//  RUNS, so a future change that re-introduces a rejection trips this gate.
//
//  The ONLY legitimate compile-time rejections remaining are genuine, SYMMETRIC
//  errors — the engines reject them identically because they are real errors, not
//  engine gaps. They are listed (and asserted symmetric) in
//  `vm_parser_accepts_runs_legitimate_rejections` below:
//    • `async fn init` / `fn* init` — a constructor cannot be async/generator.
//    • `static fn from`              — `from` is a reserved class member.
//    • `yield` field default         — `yield` is never valid outside a generator.
// ─────────────────────────────────────────────────────────────────────────────

/// Every grammar-accepted construct SP1 touched, as a runnable program. Each MUST
/// run (no compile rejection) on BOTH VM modes and be byte-identical to the
/// tree-walker — that is the parser-accepts-runs invariant.
const PARSER_ACCEPTS_RUNS_PROGRAMS: &[&str] = &[
    // ── §1 optional method calls `a?.m()` + chains ──────────────────────────────
    // nil receiver short-circuits; the arg's side effect must NOT run.
    "fn se() { print(\"ARG\")\n return 1 }\nlet a = nil\nprint(a?.m(se()))\n",
    // non-nil receiver → ordinary bound method call.
    "class C { fn m(x) { return x + 1 } }\nlet c = C()\nprint(c?.m(10))\n",
    // a whole postfix chain short-circuits when the receiver is nil.
    "let a = nil\nprint(a?.m().n().o)\n",
    // mixed optional member + optional call in one chain.
    "class C { fn m() { return 5 } }\nlet c = C()\nprint(c?.m())\nlet a = nil\nprint(a?.b?.m())\n",
    // optional call with multiple args on a live receiver.
    "class C { fn add(a, b) { return a + b } }\nlet c = C()\nprint(c?.add(3, 4))\n",

    // ── §2 generator methods `fn*` / `async fn*` ───────────────────────────────
    // basic fn* method consumed via for-await.
    "class C { fn* g() { yield 1\n yield 2 } }\nfor await (v in C().g()) { print(v) }\n",
    // fn* method using `self`, consumed via .next().
    "class C { fn init() { self.n = 10 }\n fn* g() { yield self.n\n yield self.n + 1 } }\n\
     let it = C().g()\nprint(it.next())\nprint(it.next())\n",
    // async fn* method (yield + await).
    "class C { async fn* g() { yield 1\n let x = await 2\n yield x } }\nfor await (v in C().g()) { print(v) }\n",
    // inherited (no override) fn* method.
    "class A { fn* g() { yield 7\n yield 8 } }\nclass B extends A {}\nfor await (v in B().g()) { print(v) }\n",

    // ── §3 static methods (sync / async / fn* / inherited / C.name()) ───────────
    // sync static factory called as C.make().
    "class C { fn init() { self.x = 1 } static fn make() { return C() } }\nprint(C.make().x)\n",
    // static async factory awaited → instance.
    "class C { fn init() { self.x = 0 } static async fn create() { let c = C()\n c.x = await 5\n return c } }\nprint((await C.create()).x)\n",
    // static fn* generator consumed via for-await.
    "class C { static fn* seq() { yield 1\n yield 2 } }\nfor await (v in C.seq()) { print(v) }\n",
    // inherited static resolved up the superclass chain.
    "class A { static fn who() { return \"A\" } }\nclass B extends A {}\nprint(B.who())\n",
    // instance + static of the SAME name coexist (separate namespaces).
    "class C { fn init() { self.v = 9 } fn x() { return self.v } static fn x() { return 42 } }\nlet c = C()\nprint(c.x())\nprint(C.x())\n",

    // ── §4 richer field-default forms (computed / arrow / match) ────────────────
    // computed (binary) field default via C() and C.from({}).
    "class C { x: number = 10 + 5 }\nprint(C().x)\nprint(C.from({}).x)\n",
    // arrow field default — the value is the arrow itself, called to verify.
    "class C { f: fn = (a, b) => a + b }\nprint(C().f(2, 3))\n",
    // match field default.
    "let n = 2\nclass C { label: string = match n { 1 => \"one\", 2 => \"two\", _ => \"many\" } }\nprint(C().label)\n",
    // inclusive-range (`..=`) field default — a valid range expression, NOT a hole.
    "class C { r: any = 1..=3 }\nprint(C().r)\n",
    // stepped inclusive-range field default.
    "class C { r: any = 1..=10 step 2 }\nprint(C().r)\n",
];

/// F1 — the parser-accepts-runs invariant: every grammar-accepted construct SP1
/// touched RUNS on both VM modes and is byte-identical to the tree-walker. A new
/// "parser accepts but engine rejects valid code" hole trips this gate.
#[tokio::test]
async fn vm_parser_accepts_runs() {
    for src in PARSER_ACCEPTS_RUNS_PROGRAMS {
        // Each program must run to a value on the tree-walker AND both VM modes —
        // i.e. NONE of them is a compile rejection. (A genuine runtime outcome is
        // fine; a compile-time "not supported" rejection on only one engine is the
        // hole this gate forbids.)
        let tw = ascript::run_source_exit(src).await;
        assert!(
            tw.is_ok(),
            "tree-walker rejected a parser-accepted program (should RUN):\n  src: {src:?}\n  err: {tw:?}"
        );
        let spec = ascript::vm_run_source(src).await;
        assert!(
            spec.is_ok(),
            "specialized VM rejected a parser-accepted program (parser-accepts-runs hole):\n  \
             src: {src:?}\n  err: {spec:?}"
        );
        let generic = ascript::vm_run_source_generic(src).await;
        assert!(
            generic.is_ok(),
            "generic VM rejected a parser-accepted program (parser-accepts-runs hole):\n  \
             src: {src:?}\n  err: {generic:?}"
        );
        // …and all three agree byte-for-byte.
        assert_three_way_matches(src).await;
    }
}

/// F1 (companion) — the ONLY legitimate compile-time rejections after SP1 are
/// genuine, SYMMETRIC errors: every engine rejects them, with byte-identical
/// outcomes. This pins down that the remaining rejections are real errors (not
/// engine gaps) so the invariant above stays meaningful.
#[tokio::test]
async fn vm_parser_accepts_runs_legitimate_rejections() {
    // These reject byte-identically (same message + exit) on all three engines —
    // a structural compile/resolve error knowable before execution.
    let symmetric_message_rejections: &[&str] = &[
        // async constructor — there is no caller to await it.
        "class C { async fn init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n",
        // generator constructor — makes no sense.
        "class C { fn* init() { self.x = 1 } }\nlet c = C()\nprint(c.x)\n",
        // `from` is a reserved class member (the built-in validator).
        "class C { static fn from() { return 1 } }\nprint(C.from())\n",
    ];
    for src in symmetric_message_rejections {
        // Every engine must REJECT (these are real errors, not engine gaps)…
        let tw = ascript::run_source_exit(src).await;
        assert!(
            tw.is_err(),
            "expected the tree-walker to reject a genuine error, but it ran:\n  src: {src:?}\n  ok: {tw:?}"
        );
        // …and reject byte-identically across all three engines.
        assert_three_way_matches(src).await;
    }

    // `yield` as a field default is rejected by every engine, but the VM rejects it
    // at COMPILE time (`cst_default_expr`) and the tree-walker at default-EVAL time,
    // so the message/timing legitimately differ (documented in
    // `vm_field_default_yield_rejected_symmetrically`). Assert symmetry-of-rejection
    // (all three `Err`), not byte-identical text.
    for default in ["yield 1", "yield"] {
        let src = format!("class C {{ y: any = {default}\n  fn init() {{}} }}\nprint(C().y)\n");
        let tw = ascript::run_source(&src).await;
        let spec = ascript::vm_run_source(&src).await;
        let generic = ascript::vm_run_source_generic(&src).await;
        assert!(
            tw.is_err() && spec.is_err() && generic.is_err(),
            "expected ALL engines to reject the `{default}` field default\n  \
             tree-walker: {tw:?}\n  spec: {spec:?}\n  generic: {generic:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SP2 Phase G — feature-coverage invariant gate.
//
//  THE closing invariant of SP2: every NEW surface feature (instanceof, default
//  parameters, `#{…}` map literals, object.freeze/isFrozen + frozen-mutation
//  panic, records / auto-derived init) RUNS on both VM modes and is byte-identical
//  to the tree-walker oracle — including when several features are combined in one
//  program. A regression that re-introduces a divergence (or a "parser accepts but
//  one engine rejects/diverges on valid code" hole) on any SP2 feature trips this
//  gate. Mirrors the SP1 `vm_parser_accepts_runs` gate above.
//
//  `std/object` (freeze/isFrozen) is a CORE module, so these programs run in BOTH
//  feature configs — this file is compiled and run under `--no-default-features`
//  too, where the gate stays meaningful.
// ─────────────────────────────────────────────────────────────────────────────

/// Every SP2 feature, as a runnable program that must be byte-identical on all
/// three engines (tree-walker == specialized-VM == generic-VM). The last few
/// programs combine multiple features so the cross-feature interactions are
/// exercised together.
const SP2_FEATURE_PROGRAMS: &[&str] = &[
    // ── §1 instanceof ───────────────────────────────────────────────────────────
    // own class, subclass↑parent, parent-NOT↑subclass, non-instance is false.
    "class C {}\nlet c = C()\nprint(c instanceof C)\n",
    "class A {}\nclass B extends A {}\nprint(B() instanceof A)\nprint(A() instanceof B)\n",
    "class C {}\nprint(5 instanceof C)\nprint(\"x\" instanceof C)\nprint(nil instanceof C)\n",
    // precedence: binds at the comparison tier (looser than `&&`).
    "class C {}\nlet c = C()\nprint(c instanceof C && true)\n",

    // ── §2 default parameters ────────────────────────────────────────────────────
    // omitted vs supplied; a default that references an EARLIER param (left-to-right).
    "fn f(a, b = 10) { return a + b }\nprint(f(1))\nprint(f(1, 2))\n",
    "fn f(a, b = a * 2) { return b }\nprint(f(5))\n",
    // arrow default + composition with rest.
    "let g = (x, y = 5) => x + y\nprint(g(2))\nprint(g(2, 3))\n",
    "fn f(a, b = 2, ...xs) { return [a, b, xs] }\nprint(f(1))\nprint(f(1, 9, 8, 7))\n",
    // explicit nil suppresses the default.
    "fn f(a, b = 10) { return b }\nprint(f(1, nil))\n",

    // ── §3 `#{…}` map literals ───────────────────────────────────────────────────
    // empty; string keys; numeric/bool/nil keys; expression key; later-key-wins.
    "print(#{})\n",
    "print(#{ \"a\": 1, \"b\": 2 })\n",
    "print(#{ 1: \"x\", true: \"y\", nil: \"z\" })\n",
    "let k = \"x\"\nprint(#{ k: 1 })\n",
    "print(#{ 1: \"a\", 1: \"b\" })\n",

    // ── §4 object.freeze / isFrozen ──────────────────────────────────────────────
    // freeze returns the value; isFrozen reflects it; non-container is a no-op.
    "import * as object from \"std/object\"\nlet o = {a: 1}\nprint(object.isFrozen(o))\n\
     let r = object.freeze(o)\nprint(object.isFrozen(o))\nprint(r == o)\n",
    "import * as object from \"std/object\"\nprint(object.isFrozen(5))\n",
    // shallow: an element of a frozen array is still mutable.
    "import * as object from \"std/object\"\nlet a = [[1]]\nobject.freeze(a)\na[0][0] = 9\nprint(a)\n",
    // deep-clone of a frozen object is unfrozen.
    "import * as object from \"std/object\"\nlet o = {a: 1}\nobject.freeze(o)\n\
     let c = object.deepClone(o)\nprint(object.isFrozen(c))\n",

    // ── §5 records / auto-derived init ───────────────────────────────────────────
    // positional auto-constructor in declaration order; defaulted trailing field.
    "class Point { x: number\n y: number }\nlet p = Point(1, 2)\nprint(p.x)\nprint(p.y)\n",
    "class P { x: number\n y: number = 0 }\nprint(P(1).y)\nprint(P(1, 2).y)\n",
    // a class WITH explicit init is unchanged (auto-init NOT applied).
    "class C { x: number = 0\n fn init(v) { self.x = v + 1 } }\nprint(C(5).x)\n",
    // inheritance: base fields then subclass fields, positional.
    "class A { a: number }\nclass B extends A { b: number }\nlet x = B(1, 2)\nprint(x.a)\nprint(x.b)\n",

    // ── cross-feature combinations ───────────────────────────────────────────────
    // record + instanceof + inheritance + a defaulted trailing field together.
    "class A { a: number }\nclass B extends A { b: number = 9 }\nlet x = B(1)\n\
     print(x instanceof A)\nprint(x.a)\nprint(x.b)\n",
    // default param whose default is a `#{…}` map literal; instanceof of nothing.
    "fn f(m = #{ \"a\": 1 }) { return m }\nprint(f())\nprint(f(#{ \"b\": 2 }))\n",
    // freeze a record instance, then a default-param fn reads a field off it.
    "import * as object from \"std/object\"\nclass P { x: number\n y: number = 0 }\n\
     let p = object.freeze(P(3))\nfn sum(pt, bias = 10) { return pt.x + pt.y + bias }\n\
     print(object.isFrozen(p))\nprint(sum(p))\n",
];

/// G1 — the SP2 feature-coverage invariant: every SP2 feature (and several
/// cross-feature combinations) RUNS on all three engines and is byte-identical.
#[tokio::test]
async fn vm_sp2_features() {
    for src in SP2_FEATURE_PROGRAMS {
        // Each program must RUN (not be a compile rejection) on the tree-walker AND
        // both VM modes — a one-engine rejection on valid code is the hole this gate
        // forbids.
        let tw = ascript::run_source_exit(src).await;
        assert!(
            tw.is_ok(),
            "tree-walker rejected a valid SP2 program (should RUN):\n  src: {src:?}\n  err: {tw:?}"
        );
        let spec = ascript::vm_run_source(src).await;
        assert!(
            spec.is_ok(),
            "specialized VM rejected a valid SP2 program (parser-accepts-runs hole):\n  \
             src: {src:?}\n  err: {spec:?}"
        );
        let generic = ascript::vm_run_source_generic(src).await;
        assert!(
            generic.is_ok(),
            "generic VM rejected a valid SP2 program (parser-accepts-runs hole):\n  \
             src: {src:?}\n  err: {generic:?}"
        );
        // …and all three engines agree byte-for-byte.
        assert_three_way_matches(src).await;
    }
}

/// G1 (companion) — SP2 features whose ONLY valid outcome is a SYMMETRIC error:
/// every engine rejects them byte-identically (real errors, not engine gaps).
/// Covers instanceof with a non-class rhs, default-after-required, and the
/// frozen-mutation panic across all container kinds.
#[tokio::test]
async fn vm_sp2_features_symmetric_rejections() {
    // These error byte-identically (same message + exit) on all three engines.
    let byte_identical_rejections: &[&str] = &[
        // instanceof requires a class on the rhs.
        "let c = 1\nprint(c instanceof 5)\n",
        // a required parameter cannot follow a defaulted one.
        "fn f(a = 1, b) { return b }\nprint(f(1, 2))\n",
        // default-param arity: too few / too many (no rest).
        "fn f(a, b = 1) {}\nf()\n",
        "fn f(a, b = 1) {}\nf(1, 2, 3)\n",
        // typed default contract-checks the explicit arg.
        "fn f(a, b: number = 1) { return b }\nprint(f(1, \"x\"))\n",
        // record arity + positional contract.
        "class Point { x: number\n y: number }\nPoint(1)\n",
        "class Point { x: number\n y: number }\nPoint(1, 2, 3)\n",
        "class Point { x: number\n y: number }\nPoint(\"a\", 2)\n",
        // unhashable map key.
        "print(#{ [1]: 2 })\n",
        // frozen-mutation panic across object / array / map / instance.
        "import * as object from \"std/object\"\nlet o = {a: 1}\nobject.freeze(o)\no.a = 2\n",
        "import * as object from \"std/object\"\nimport * as array from \"std/array\"\n\
         let a = [1]\nobject.freeze(a)\narray.push(a, 2)\n",
        "import * as object from \"std/object\"\nimport * as map from \"std/map\"\n\
         let m = map.new()\nobject.freeze(m)\nmap.set(m, \"k\", 1)\n",
        "import * as object from \"std/object\"\nclass C { x: number = 0 }\nlet c = C()\n\
         object.freeze(c)\nc.x = 9\n",
    ];
    for src in byte_identical_rejections {
        let tw = ascript::run_source_exit(src).await;
        assert!(
            tw.is_err(),
            "expected the tree-walker to reject a genuine SP2 error, but it ran:\n  \
             src: {src:?}\n  ok: {tw:?}"
        );
        // …and reject byte-identically across all three engines.
        assert_three_way_matches(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  V12-T3: bytecode verifier — every chunk the compiler emits must VERIFY OK.
//
//  The verifier (`ascript::vm::verify`) is the load-time guard for `.aso`: it walks
//  the code stream and proves decode integrity, operand ranges, jump-target landing,
//  and stack-depth balance. The compiler emits valid bytecode by construction, so
//  the verifier MUST accept the chunk for every corpus example that compiles — both
//  the freshly-compiled chunk and the one round-tripped through `.aso`. A regression
//  in either the compiler or the verifier trips this gate.
// ─────────────────────────────────────────────────────────────────────────────
/// Whether a chunk (and every nested proto) has a literals-only const pool, so the
/// V12-T2 `.aso` writer can serialize it without panicking. Object-rest stores an
/// `Array`-of-keys const, which is not yet serializable — those chunks are still
/// verified in-memory but skip the round-trip step.
fn chunk_is_serializable(chunk: &ascript::vm::chunk::Chunk) -> bool {
    if chunk.check_consts_literal_only().is_err() {
        return false;
    }
    chunk.protos.iter().all(|p| chunk_is_serializable(&p.chunk))
}

#[test]
fn verifier_accepts_all_compiled_corpus_chunks() {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut verified = 0usize;
    for rel in all_corpus_examples() {
        let path = std::path::Path::new(root).join(&rel);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        // Only chunks the compiler actually produces are in scope; an example the VM
        // compiler cannot yet compile (a documented gap) is skipped here — it has no
        // chunk to verify.
        let chunk = match ascript::compile::compile_source(&src) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // 1) the freshly-compiled chunk verifies.
        ascript::vm::verify::verify(&chunk)
            .unwrap_or_else(|e| panic!("verifier rejected compiled `{rel}`: {e}"));
        // 2) IF the chunk is `.aso`-serializable (its const pool — including every
        //    nested proto — is literals-only; object-rest stores an Array-of-keys
        //    const that the V12-T2 writer does not yet serialize, a separate
        //    documented gap), it must survive an `.aso` round-trip AND re-verify on
        //    load.
        if chunk_is_serializable(&chunk) {
            let bytes = chunk
                .to_bytes()
                .unwrap_or_else(|e| panic!("to_bytes failed for `{rel}`: {e}"));
            ascript::vm::chunk::Chunk::from_bytes_verified(&bytes)
                .unwrap_or_else(|e| panic!("from_bytes_verified rejected `{rel}`: {e}"));
        }
        verified += 1;
    }
    assert!(
        verified >= 20,
        "expected the verifier corpus gate to cover >=20 examples, covered {verified}"
    );
    eprintln!(
        "verifier corpus gate: {verified} compiled chunks verify OK (fresh + .aso round-trip)"
    );
}

// ---------------------------------------------------------------------------
// Class field default capturing an enclosing local (fix: class field default
// may reference an enclosing local — module-top-level const or a function
// local for a class declared inside a function). The VM compiles the default
// into a thunk closure that captures the enclosing binding via the SAME upvalue
// machinery as any nested closure; the `.from`/typed-parse path resolves the
// same binding via the class `def_env` mirror. Both must match the tree-walker.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vm_field_default_captures_module_const_construct() {
    // The thunk runs at CONSTRUCT time and must read the module-top-level const.
    assert_vm_run_matches_treewalker(
        "const ROLE = \"admin\"\nclass Base {\n  role: string = ROLE\n  fn init() {}\n}\nlet b = Base()\nprint(b.role)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_field_default_captures_module_const_from() {
    // The `.from` path reads `FieldSchema.default` against `def_env`; the const
    // must be mirrored into `def_env` so it resolves.
    assert_vm_run_matches_treewalker(
        "const ROLE = \"admin\"\nclass Base {\n  id: number = 5\n  role: string = ROLE\n}\nlet b = Base.from({})\nprint(b.role)\nprint(b.id)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_field_default_captures_function_local() {
    // A class declared INSIDE a function whose field default references a function
    // local — proves general enclosing-scope capture (not just module top level).
    assert_vm_run_matches_treewalker(
        "fn make() {\n  let role = \"fnrole\"\n  class Inner {\n    role: string = role\n  }\n  return Inner.from({})\n}\nprint(make().role)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_field_default_captures_const_in_array_default() {
    // The captured const used inside a (supported) array-literal default — proves
    // capture works for a non-trivial default form, not just a bare ident.
    assert_vm_run_matches_treewalker(
        "const X = 7\nclass Base {\n  vals: array<number> = [X, X]\n  fn init() {}\n}\nlet b = Base()\nprint(b.vals[0])\nprint(b.vals[1])\n",
    )
    .await;
}

#[tokio::test]
async fn vm_literal_field_default_still_matches_treewalker() {
    // Regression guard: a plain literal default (no capture) is unchanged.
    assert_vm_run_matches_treewalker(
        "class Base {\n  role: string = \"guest\"\n  fn init() {}\n}\nprint(Base().role)\n",
    )
    .await;
}

// ---- Module-scope user-globals: forward references --------------------------
//
// The tree-walker treats module scope as one shared, late-bound `Environment`: a
// function/thunk body may reference a top-level `let`/`const` declared LATER, and
// it resolves when the function RUNS. Before this feature the VM compiled a
// top-level `let`/`const` as a frame SLOT-LOCAL and had NO user-globals, so a
// forward reference resolved as Global→GET_GLOBAL→`undefined variable`. These
// cases pin the now-byte-identical behavior; the use-before-init case stays
// SYMMETRIC (both engines error).

#[tokio::test]
async fn vm_forward_const_from_function() {
    // The canonical bug: `f` reads `X` declared AFTER it; the call runs after both
    // are defined, so the late-bound global resolves to 9 on BOTH engines.
    assert_vm_run_matches_treewalker("fn f() { return X }\nconst X = 9\nprint(f())").await;
}

#[tokio::test]
async fn vm_forward_const_in_class_field_default() {
    // A class field default referencing a later top-level const, via BOTH the
    // constructor `C()` AND the validating `C.from({})` path.
    assert_vm_run_matches_treewalker(
        "class C {\n  n: number = LATER\n  fn init() {}\n}\nconst LATER = 42\nprint(C().n)\n",
    )
    .await;
    assert_vm_run_matches_treewalker(
        "class C {\n  n: number = LATER\n}\nconst LATER = 42\nprint(C.from({}).n)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_use_before_init_is_symmetric_error() {
    // Reading a top-level const BEFORE its declaration runs is a TDZ-style error on
    // BOTH engines (the global is not yet defined when the read executes).
    assert_vm_run_error_matches_treewalker("print(X)\nconst X = 9").await;
}

#[tokio::test]
async fn vm_top_level_let_reassigned_then_read() {
    // A top-level mutable `let`, reassigned then read, must round-trip identically.
    assert_vm_run_matches_treewalker("let x = 1\nx = x + 4\nprint(x)").await;
    // Reassignment observed through a function that reads the global late.
    assert_vm_run_matches_treewalker(
        "let count = 0\nfn bump() { count = count + 1 }\nbump()\nbump()\nprint(count)",
    )
    .await;
}

#[tokio::test]
async fn vm_inner_function_reads_top_level_binding() {
    // An inner function (nested two frames deep) reads a top-level let/const; it is
    // a GLOBAL, not an upvalue, so it resolves late on both engines.
    assert_vm_run_matches_treewalker(
        "const BASE = 100\nfn outer() {\n  fn inner() { return BASE + 1 }\n  return inner()\n}\nprint(outer())",
    )
    .await;
    assert_vm_run_matches_treewalker(
        "let total = 5\nfn outer() {\n  let g = () => total * 2\n  return g()\n}\nprint(outer())",
    )
    .await;
}

#[tokio::test]
async fn vm_mutual_top_level_functions() {
    // Mutual recursion between two top-level fns: `a` forward-references `b`.
    assert_vm_run_matches_treewalker("fn a() { return b() }\nfn b() { return 1 }\nprint(a())")
        .await;
    // Even/odd mutual recursion exercises repeated forward+backward global reads.
    assert_vm_run_matches_treewalker(
        "fn even(n) { if (n == 0) { return true }\n return odd(n - 1) }\n\
         fn odd(n) { if (n == 0) { return false }\n return even(n - 1) }\n\
         print(even(10))\nprint(odd(7))",
    )
    .await;
}

#[tokio::test]
async fn vm_top_level_const_references_earlier_const() {
    // A top-level const whose initializer reads an EARLIER top-level const (the
    // common, already-defined direction) — must agree.
    assert_vm_run_matches_treewalker("const A = 3\nconst B = A * 2\nprint(B)").await;
    // And a top-level let initialized from an earlier top-level const.
    assert_vm_run_matches_treewalker("const A = 10\nlet b = A + 5\nprint(b)").await;
}

#[tokio::test]
async fn vm_inner_let_shadows_top_level_global() {
    // An inner `let x` must shadow a top-level global `x` (resolve_local wins).
    assert_vm_run_matches_treewalker(
        "let x = 1\nfn f() { let x = 99\n return x }\nprint(f())\nprint(x)",
    )
    .await;
}

// ---- Binding semantics: redeclaration + const immutability ------------------
//
// Both are RUNTIME-timed in the tree-walker (via `Environment::define` /
// `Environment::assign`): a same-scope redeclaration errors when the SECOND define
// executes; a const reassignment errors when the assignment executes — so dead /
// never-reached cases run fine. The VM matches byte-for-byte (message AND span;
// the redeclaration error carries no span, the immutable error carries the target
// span). `assert_vm_run_error_matches_treewalker` checks message + span; the
// success helper checks stdout.

#[tokio::test]
async fn vm_top_level_redeclaration_errors_match_treewalker() {
    // let/let, let/const, fn/fn, fn/let — all the same-scope redeclaration error,
    // fired at runtime when the SECOND define executes (same message, no span).
    assert_vm_run_error_matches_treewalker("let x = 1\nlet x = 2\nprint(x)").await;
    assert_vm_run_error_matches_treewalker("let x = 1\nconst x = 2\nprint(x)").await;
    assert_vm_run_error_matches_treewalker("fn f() { return 1 }\nfn f() { return 2 }\nprint(f())")
        .await;
    assert_vm_run_error_matches_treewalker("fn f() { return 1 }\nlet f = 2\nprint(f)").await;
}

#[tokio::test]
async fn vm_redeclaration_runtime_timing_matches_treewalker() {
    // The first `print` runs BEFORE the redeclaration error (runtime timing): the
    // tree-walker and VM both emit "before\n" then fail identically. The error
    // helper only fires when BOTH error; this program errors, so use the run-error
    // helper which (because both partial-print "before" to the LIVE sink under the
    // CLI, but CAPTURE under run_source) compares the panic itself.
    assert_vm_run_error_matches_treewalker("let x = 1\nprint(\"before\")\nlet x = 2").await;
}

#[tokio::test]
async fn vm_dead_code_redeclaration_runs_fine_matches_treewalker() {
    // A redeclaration inside an UN-ENTERED block / UNCALLED function is fine on both
    // engines (those `let`s are block/fn slot-locals, not module globals; and even a
    // genuine same-scope dup in dead code never executes its second define).
    assert_vm_run_matches_treewalker("if (false) { let y = 1\n let y = 2 }\nprint(\"ok\")").await;
    assert_vm_run_matches_treewalker("fn g() { let z = 1\n let z = 2 }\nprint(\"ok\")").await;
}

#[tokio::test]
async fn vm_const_reassignment_errors_match_treewalker() {
    // Executed const reassignment → immutable error on both engines (message +
    // target span). Covers a top-level const, a const inside a function, and a
    // compound `+=` on a const.
    assert_vm_run_error_matches_treewalker("const k = 1\nk = 2\nprint(k)").await;
    assert_vm_run_error_matches_treewalker("fn f() { const c = 1\n c = 2\n return c }\nprint(f())")
        .await;
    assert_vm_run_error_matches_treewalker(
        "fn f() { const c = 1\n c += 2\n return c }\nprint(f())",
    )
    .await;
}

#[tokio::test]
async fn vm_immutable_binding_kinds_error_match_treewalker() {
    // Every immutable binding kind: fn name, class name, loop var, const-destructure
    // bind — reassigning any is the immutable error on both engines.
    assert_vm_run_error_matches_treewalker("fn f() { return 1 }\nf = 2\nprint(f)").await;
    assert_vm_run_error_matches_treewalker("class C {}\nC = 3\nprint(C)").await;
    assert_vm_run_error_matches_treewalker("for (i in 0..3) { i = 9\n print(i) }").await;
    assert_vm_run_error_matches_treewalker("const [a, b] = [1, 2]\na = 9\nprint(a)").await;
}

#[tokio::test]
async fn vm_imported_binding_reassignment_errors_match_treewalker() {
    // An IMPORTED name is an immutable module global: reassigning it (in the SAME
    // entry chunk — the cross-MODULE case) is the immutable error on both engines,
    // now via the runtime SET_GLOBAL mutability check.
    assert_vm_run_error_matches_treewalker("import { abs } from \"std/math\"\nabs = 3\nprint(abs)")
        .await;
    // The same name USED first, then reassigned — still immutable.
    assert_vm_run_error_matches_treewalker(
        "import { max } from \"std/math\"\nprint(max(1, 2))\nmax = 9",
    )
    .await;
}

#[tokio::test]
async fn vm_dead_const_reassignment_runs_fine_matches_treewalker() {
    // A const reassignment that is NEVER REACHED runs fine on both engines (runtime
    // timing — only an EXECUTED assignment errors).
    assert_vm_run_matches_treewalker("const k = 1\nif (false) { k = 2 }\nprint(\"ok\")").await;
    assert_vm_run_matches_treewalker("const k = 1\nfn g() { k = 2 }\nprint(\"ok\")").await;
}

#[tokio::test]
async fn vm_const_reassign_runs_rhs_first_matches_treewalker() {
    // The RHS side-effect runs BEFORE the const-assign error (the immutable check is
    // at the store, after value evaluation) — byte-identical on both engines.
    assert_vm_run_error_matches_treewalker(
        "const k = 1\nfn side() { print(\"rhs ran\")\n return 9 }\nk = side()",
    )
    .await;
}

#[tokio::test]
async fn vm_mutable_let_and_param_reassign_still_work() {
    // Regression guard: a mutable `let` (top-level + local) and a `param` reassign
    // fine — the immutable check must NOT over-trigger.
    assert_vm_run_matches_treewalker("let x = 1\nx = 2\nprint(x)").await;
    assert_vm_run_matches_treewalker("fn f() { let y = 1\n y = 5\n return y }\nprint(f())").await;
    assert_vm_run_matches_treewalker("fn f(a) { a = 5\n return a }\nprint(f(1))").await;
    assert_vm_run_matches_treewalker("let [a, b] = [1, 2]\na = 9\nprint(a)").await;
}

#[tokio::test]
async fn vm_instanceof_basic_matches_treewalker() {
    // SP2 §1 — an instance `instanceof` its own class is true.
    assert_vm_run_matches_treewalker("class C {}\nlet c = C()\nprint(c instanceof C)\n").await;
}

#[tokio::test]
async fn vm_instanceof_subclass_matches_treewalker() {
    // Subclass instance `instanceof` parent → true; parent instance NOT instanceof
    // subclass → false (walk the `extends` chain).
    assert_vm_run_matches_treewalker(
        "class A {}\nclass B extends A {}\nprint(B() instanceof A)\nprint(A() instanceof B)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_instanceof_non_instance_matches_treewalker() {
    // Non-instances are always false, never panic.
    assert_vm_run_matches_treewalker(
        "class C {}\nprint(5 instanceof C)\nprint(\"x\" instanceof C)\nprint(nil instanceof C)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_instanceof_precedence_matches_treewalker() {
    // Precedence: binds like a comparison — `c instanceof C && true` parses as
    // `(c instanceof C) && true`.
    assert_vm_run_matches_treewalker("class C {}\nlet c = C()\nprint(c instanceof C && true)\n")
        .await;
}

#[tokio::test]
async fn vm_instanceof_in_if_matches_treewalker() {
    // `instanceof` as the condition of an `if`.
    assert_vm_run_matches_treewalker(
        "class C {}\nlet c = C()\nif (c instanceof C) { print(\"yes\") } else { print(\"no\") }\n",
    )
    .await;
}

#[tokio::test]
async fn vm_instanceof_rhs_not_class_errors_match_treewalker() {
    // RHS not a class → identical Tier-2 panic on both engines.
    assert_vm_run_error_matches_treewalker("let c = 1\nprint(c instanceof 5)\n").await;
    assert_vm_run_error_matches_treewalker("class C {}\nprint(C() instanceof nil)\n").await;
}

#[tokio::test]
async fn vm_instanceof_three_way_matches() {
    assert_three_way_matches("class A {}\nclass B extends A {}\nlet b = B()\nprint(b instanceof A)\nprint(b instanceof B)\nprint(5 instanceof A)\n").await;
}

// ---- SP2 Phase B: default parameters --------------------------------------

#[tokio::test]
async fn vm_default_param_basic_matches_treewalker() {
    // Omitted trailing arg uses the default; supplied arg suppresses it.
    assert_vm_run_matches_treewalker("fn f(a, b = 10) { return a + b }\nprint(f(1))\nprint(f(1, 2))\n").await;
}

#[tokio::test]
async fn vm_default_param_refs_earlier_matches_treewalker() {
    // A default may reference an earlier already-bound param.
    assert_vm_run_matches_treewalker("fn g(a, b = a + 1) { return b }\nprint(g(5))\n").await;
    assert_vm_run_matches_treewalker("fn f(a, b = a * 2) { return b }\nprint(f(5))\n").await;
}

#[tokio::test]
async fn vm_default_param_arrow_matches_treewalker() {
    assert_vm_run_matches_treewalker("let g = (x, y = 5) => x + y\nprint(g(2))\nprint(g(2, 3))\n").await;
    assert_vm_run_matches_treewalker("let h = (x = 10) => x\nprint(h())\nprint(h(7))\n").await;
}

#[tokio::test]
async fn vm_default_param_side_effect_only_when_omitted_matches_treewalker() {
    // The default expr's side effect (a `print`) must run ONLY when the arg is
    // omitted, and LEFT-TO-RIGHT across multiple omitted defaults.
    let src = "fn note(tag, v) { print(tag)\n return v }\n\
fn f(a, b = note(\"b\", 2), c = note(\"c\", 3)) { return [a, b, c] }\n\
print(\"--- f(1): both defaults run, b then c\")\n\
print(f(1))\n\
print(\"--- f(1, 9): only c default runs\")\n\
print(f(1, 9))\n\
print(\"--- f(1, 9, 8): no default runs\")\n\
print(f(1, 9, 8))\n";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_default_param_calls_global_matches_treewalker() {
    assert_vm_run_matches_treewalker("fn base() { return 42 }\nfn f(a, b = base()) { return a + b }\nprint(f(1))\nprint(f(1, 2))\n").await;
}

#[tokio::test]
async fn vm_default_param_with_rest_matches_treewalker() {
    assert_vm_run_matches_treewalker("fn h(a, b = 2, ...xs) { return [a, b, xs] }\nprint(h(1))\nprint(h(1, 9, 8, 7))\nprint(h(1, 9))\n").await;
}

#[tokio::test]
async fn vm_default_param_explicit_nil_suppresses_matches_treewalker() {
    // Explicit nil suppresses the default — only a MISSING arg triggers it.
    assert_vm_run_matches_treewalker("fn f(a, b = 10) { return b }\nprint(f(1, nil))\n").await;
}

#[tokio::test]
async fn vm_default_param_typed_contract_matches_treewalker() {
    // A typed defaulted param: the explicit value is contract-checked.
    assert_vm_run_error_matches_treewalker("fn f(a, b: number = 1) { return b }\nprint(f(1, \"x\"))\n").await;
    // The default value is itself contract-checked when applied.
    assert_vm_run_matches_treewalker("fn f(a, b: number = 1) { return b }\nprint(f(1))\nprint(f(1, 5))\n").await;
}

#[tokio::test]
async fn vm_default_param_required_after_default_errors_match_treewalker() {
    // A required param cannot follow a defaulted one — identical parse/compile
    // error on both engines.
    assert_vm_run_error_matches_treewalker("fn f(a = 1, b) { return b }\nprint(f(1, 2))\n").await;
}

#[tokio::test]
async fn vm_default_param_arity_errors_match_treewalker() {
    // Too few (below min) and too many (above max, no rest).
    assert_vm_run_error_matches_treewalker("fn f(a, b = 1) {}\nf()\n").await;
    assert_vm_run_error_matches_treewalker("fn f(a, b = 1) {}\nf(1, 2, 3)\n").await;
}

#[tokio::test]
async fn vm_default_param_three_way_matches() {
    assert_three_way_matches(
        "fn f(a, b = a + 1, c = 10) { return [a, b, c] }\n\
print(f(1))\n\
print(f(1, 2))\n\
print(f(1, 2, 3))\n\
let g = (x = 5) => x * 2\n\
print(g())\n\
print(g(4))\n",
    )
    .await;
}

// ---- Phase C: #{…} map literals ----------------------------------------

#[tokio::test]
async fn vm_map_lit_empty_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(#{})\n").await;
}

#[tokio::test]
async fn vm_map_lit_string_keys_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(#{ \"a\": 1, \"b\": 2 })\n").await;
}

#[tokio::test]
async fn vm_map_lit_numeric_keys_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(#{ 1: \"x\", 2: \"y\" })\n").await;
}

#[tokio::test]
async fn vm_map_lit_mixed_type_keys_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(#{ 1: \"x\", true: \"y\", nil: \"z\", \"k\": 4 })\n")
        .await;
}

#[tokio::test]
async fn vm_map_lit_expr_key_uses_value_matches_treewalker() {
    // The key expression is EVALUATED — `k` is keyed by its value "x", not "k".
    assert_vm_run_matches_treewalker("let k = \"x\"\nprint(#{ k: 1 })\n").await;
    assert_vm_run_matches_treewalker("let a = 2\nlet b = 3\nprint(#{ a + b: \"sum\" })\n").await;
}

#[tokio::test]
async fn vm_map_lit_later_key_wins_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(#{ 1: \"a\", 1: \"b\" })\n").await;
}

#[tokio::test]
async fn vm_map_lit_neg_zero_nan_canon_matches_treewalker() {
    // -0.0 canonicalizes to +0.0; both keys collapse, later wins.
    assert_vm_run_matches_treewalker("print(#{ -0.0: \"neg\", 0.0: \"pos\" })\n").await;
}

#[tokio::test]
async fn vm_map_lit_read_back_matches_treewalker() {
    assert_vm_run_matches_treewalker(
        "import * as map from \"std/map\"\nlet m = #{ \"a\": 1, \"b\": 2 }\nprint(map.get(m, \"a\"))\nprint(map.get(m, \"b\"))\n",
    )
    .await;
}

#[tokio::test]
async fn vm_map_lit_is_real_map_matches_treewalker() {
    assert_vm_run_matches_treewalker("print(type(#{ \"a\": 1 }))\n").await;
    assert_vm_run_matches_treewalker("print(type(#{}))\n").await;
}

#[tokio::test]
async fn vm_map_lit_unhashable_key_errors_match_treewalker() {
    assert_vm_run_error_matches_treewalker("print(#{ [1]: 2 })\n").await;
}

#[tokio::test]
async fn vm_map_lit_spread_is_parse_error_both_engines() {
    // D4: a `...` spread element inside `#{}` is a clean parse error on BOTH
    // front-ends (legacy oracle + CST/VM) — no panic, exit non-zero. The two
    // independent parsers legitimately word the error differently, so we assert
    // BOTH reject (Err), not message-equality (that's the conformance test's job).
    let src = "let m = #{ \"a\": 1 }\nprint(#{ ...m })\n";
    let tw = ascript::run_source(src).await;
    let vm = ascript::vm_run_source(src).await;
    assert!(
        tw.is_err(),
        "tree-walker (legacy parser) must reject `...` in a map literal, got {tw:?}"
    );
    assert!(
        vm.is_err(),
        "VM (CST parser) must reject `...` in a map literal, got {vm:?}"
    );
}

#[tokio::test]
async fn vm_map_lit_in_function_matches_treewalker() {
    assert_vm_run_matches_treewalker("fn f() { return #{ 1: \"x\" } }\nprint(f())\n").await;
}

#[tokio::test]
async fn vm_map_lit_three_way_matches() {
    assert_three_way_matches(
        "import * as map from \"std/map\"\n\
let k = \"dyn\"\n\
let m = #{ \"a\": 1, 2: \"two\", true: \"t\", k: 9, 2: \"two-again\" }\n\
print(m)\n\
print(map.get(m, \"a\"))\n\
print(map.get(m, 2))\n\
print(map.get(m, \"dyn\"))\n\
print(map.get(m, 2))\n\
print(#{})\n",
    )
    .await;
}

// ─────────────────────────── SP2 Phase D — object.freeze / isFrozen ──────────

#[tokio::test]
async fn vm_freeze_returns_value_and_isfrozen_tracks_matches_treewalker() {
    // freeze returns the value; isFrozen reflects before/after.
    assert_vm_run_matches_treewalker(
        "import * as object from \"std/object\"\n\
let o = {a: 1}\n\
print(object.isFrozen(o))\n\
let r = object.freeze(o)\n\
print(object.isFrozen(o))\n\
print(r == o)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_object_set_field_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
let o = {a: 1}\n\
object.freeze(o)\n\
o.a = 2\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_object_set_index_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
let o = {a: 1}\n\
object.freeze(o)\n\
o[\"a\"] = 2\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_array_push_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
import * as array from \"std/array\"\n\
let a = [1]\n\
object.freeze(a)\n\
array.push(a, 2)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_array_index_set_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
let a = [1, 2]\n\
object.freeze(a)\n\
a[0] = 9\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_map_set_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
import * as map from \"std/map\"\n\
let m = map.new()\n\
object.freeze(m)\n\
map.set(m, \"k\", 1)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_set_add_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
import * as set from \"std/set\"\n\
let s = set.new()\n\
object.freeze(s)\n\
set.add(s, 1)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_instance_set_field_panics_match_treewalker() {
    assert_vm_run_error_matches_treewalker(
        "import * as object from \"std/object\"\n\
class C { x: number = 0 }\n\
let c = C()\n\
object.freeze(c)\n\
c.x = 9\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_is_shallow_matches_treewalker() {
    // Element of a frozen array is still mutable (shallow freeze).
    assert_vm_run_matches_treewalker(
        "import * as object from \"std/object\"\n\
let a = [[1]]\n\
object.freeze(a)\n\
a[0][0] = 9\n\
print(a)\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_noncontainer_and_clone_unfrozen_matches_treewalker() {
    // Freezing a non-container is a no-op; a deepClone of a frozen object is unfrozen.
    assert_vm_run_matches_treewalker(
        "import * as object from \"std/object\"\n\
print(object.isFrozen(5))\n\
print(object.freeze(5))\n\
let o = {a: 1}\n\
object.freeze(o)\n\
let c = object.deepClone(o)\n\
print(object.isFrozen(c))\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_idempotent_and_nonfrozen_mutates_matches_treewalker() {
    // Freezing twice is fine; a non-frozen container still mutates normally.
    assert_vm_run_matches_treewalker(
        "import * as object from \"std/object\"\n\
import * as array from \"std/array\"\n\
let a = [1]\n\
object.freeze(a)\n\
object.freeze(a)\n\
let b = [1]\n\
array.push(b, 2)\n\
print(b)\n\
print(object.isFrozen(a))\n",
    )
    .await;
}

#[tokio::test]
async fn vm_freeze_three_way_matches() {
    assert_three_way_matches(
        "import * as object from \"std/object\"\n\
import * as array from \"std/array\"\n\
let o = {a: 1, b: 2}\n\
print(object.isFrozen(o))\n\
object.freeze(o)\n\
print(object.isFrozen(o))\n\
let a = [1, 2, 3]\n\
object.freeze(a)\n\
print(object.isFrozen(a))\n\
let live = [9]\n\
array.push(live, 10)\n\
print(live)\n",
    )
    .await;
}

// ---------------------------------------------------------------------------
// SP2 Phase E — records / auto-derived `init`. A class declaring fields but no
// explicit `init` gets an auto-derived positional constructor (params in
// field-declaration order, defaults compose with §2 arity, field contracts
// enforced). A class WITH an explicit `init` is unchanged. Both engines must be
// byte-identical.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn record_auto_init_positional_matches_treewalker() {
    // Positional auto-constructor in field-declaration order.
    assert_vm_run_matches_treewalker(
        "class Point { x: number\n y: number }\nlet p = Point(1, 2)\nprint(p.x)\nprint(p.y)\n",
    )
    .await;
}

#[tokio::test]
async fn record_auto_init_defaulted_field_matches_treewalker() {
    // A defaulted field becomes an optional trailing param: P(1) uses the
    // default (0), P(1, 2) overrides it.
    assert_vm_run_matches_treewalker(
        "class P { x: number\n y: number = 0 }\nprint(P(1).y)\nprint(P(1, 2).y)\n",
    )
    .await;
}

#[tokio::test]
async fn record_auto_init_arity_errors_match_treewalker() {
    // Too few / too many args -> identical message + span both engines.
    assert_vm_run_error_matches_treewalker("class Point { x: number\n y: number }\nPoint(1)\n")
        .await;
    assert_vm_run_error_matches_treewalker(
        "class Point { x: number\n y: number }\nPoint(1, 2, 3)\n",
    )
    .await;
    // Defaulted-field arity bounds (at least 1 / at most 2).
    assert_vm_run_error_matches_treewalker("class P { x: number\n y: number = 0 }\nP()\n").await;
    assert_vm_run_error_matches_treewalker(
        "class P { x: number\n y: number = 0 }\nP(1, 2, 3)\n",
    )
    .await;
}

#[tokio::test]
async fn record_auto_init_contract_mismatch_matches_treewalker() {
    // A positional arg failing the field's type contract -> identical panic.
    assert_vm_run_error_matches_treewalker(
        "class Point { x: number\n y: number }\nPoint(\"a\", 2)\n",
    )
    .await;
    // Mismatch on the second positional arg likewise.
    assert_vm_run_error_matches_treewalker(
        "class Point { x: number\n y: number }\nPoint(1, \"b\")\n",
    )
    .await;
}

#[tokio::test]
async fn record_class_with_explicit_init_unchanged() {
    // A class WITH an explicit init is unchanged (auto-init NOT applied): the
    // init body runs, fields bound via self.x.
    assert_vm_run_matches_treewalker(
        "class C { x: number = 0\n fn init(v) { self.x = v + 1 } }\nprint(C(5).x)\n",
    )
    .await;
}

#[tokio::test]
async fn record_zero_field_class_unchanged() {
    // A class with no fields and no init keeps the zero-arg-only behavior.
    assert_vm_run_matches_treewalker("class E {}\nlet e = E()\nprint(type(e))\n").await;
    assert_vm_run_error_matches_treewalker("class E {}\nE(1)\n").await;
}

#[tokio::test]
async fn record_inheritance_positional_matches_treewalker() {
    // Inheritance: base fields then subclass fields, positional, base-first.
    assert_vm_run_matches_treewalker(
        "class A { a: number }\nclass B extends A { b: number }\n\
         let x = B(1, 2)\nprint(x.a)\nprint(x.b)\n",
    )
    .await;
    // Defaulted base field then required subclass field: positional binding in
    // merged (base-first) order; arity bounds identical both engines.
    assert_vm_run_matches_treewalker(
        "class A { a: number\n b: number = 5 }\nclass B extends A { c: number }\n\
         let x = B(1, 2, 3)\nprint(x.a)\nprint(x.b)\nprint(x.c)\n",
    )
    .await;
}

#[tokio::test]
async fn record_subclass_inherits_base_init_not_auto_init() {
    // Inheritance-of-init decision: a subclass with no init of its own INHERITS the
    // base class's explicit init (find_method walks the chain), so NO auto-init is
    // derived — construction runs the inherited init. `Sub(5)` -> base init sets
    // a = 5*10 = 50; the subclass field default b = 7 is applied. Both engines agree.
    assert_vm_run_matches_treewalker(
        "class Base { a: number\n fn init(v) { self.a = v * 10 } }\n\
         class Sub extends Base { b: number = 7 }\n\
         let s = Sub(5)\nprint(s.a)\nprint(s.b)\n",
    )
    .await;
    // And the inherited-init arity is the BASE init's (1 arg) — passing 2 is the
    // same arity panic on both engines (it is NOT the subclass field count).
    assert_vm_run_error_matches_treewalker(
        "class Base { a: number\n fn init(v) { self.a = v } }\n\
         class Sub extends Base { b: number }\nSub(1, 2)\n",
    )
    .await;
}

#[tokio::test]
async fn record_from_still_works_on_auto_init_class() {
    // `.from` is independent of init; it must still validate/coerce an auto-init
    // (record) class, applying field defaults.
    assert_vm_run_matches_treewalker(
        "class P { x: number\n y: number = 0 }\n\
         let p = P.from({x: 1})\nprint(p.x)\nprint(p.y)\n\
         let q = P.from({x: 3, y: 4})\nprint(q.x)\nprint(q.y)\n",
    )
    .await;
}

#[tokio::test]
async fn record_three_way_matches() {
    // The full record surface through all three engines (tree-walker ==
    // specialized-VM == generic-VM), including instanceof + freeze interop.
    assert_three_way_matches(
        "import * as object from \"std/object\"\n\
         class Point { x: number\n y: number = 0 }\n\
         class P3 extends Point { z: number }\n\
         let p = Point(1)\n\
         print(p.x)\nprint(p.y)\n\
         let q = Point(2, 3)\nprint(q.y)\n\
         let r = P3(4, 5, 6)\nprint(r.x)\nprint(r.y)\nprint(r.z)\n\
         print(p instanceof Point)\n\
         print(r instanceof Point)\n\
         object.freeze(p)\n\
         print(object.isFrozen(p))\n",
    )
    .await;
}

// =============================================================================
// SP3 §B — recursion-depth guard: byte-identical on both engines
// =============================================================================
//
// A deep non-yielding recursion (and a deeply nested expression) USED to overflow
// the native stack and SIGABRT (exit 134) at WILDLY different depths on the two
// engines. SP3 adds a single shared logical-CALL counter (`MAX_CALL_DEPTH`,
// incremented EXACTLY ONCE per logical call on BOTH engines) so they raise the SAME
// clean, catchable Tier-2 panic `maximum recursion depth exceeded` — same message,
// same (no) stdout, same non-134 exit — at the SAME logical depth, before the
// native stack blows. These cases run on an enlarged worker stack
// (`run_on_worker_stack`, the same headroom the `run` binary uses).
//
// The CEILING is IDENTICAL on both engines: a recursion to `MAX_CALL_DEPTH - 1`
// completes on both; `MAX_CALL_DEPTH` fails on both. The boundary tests below read
// the crate const and assert at exactly `MAX-1` (last success) and `MAX` (first
// failure), so a re-introduced per-call double-count (e.g. counting expression
// nesting against the call counter) would FAIL them. Deeply nested EXPRESSIONS are
// a SEPARATE dimension (`EXPR_NEST_LIMIT`, on its own counter so it cannot
// contaminate the per-call count); they too trip identically on both engines.

/// A self-recursive driver to logical call-depth `n`.
fn sp3_rec_src(n: usize) -> String {
    format!("fn f(n) {{\n  if (n <= 0) {{ return 0 }}\n  return 1 + f(n - 1)\n}}\nprint(f({n}))\n")
}

/// `"let x = " + "("*k + "1" + ")"*k` — expression-nesting depth `k`.
fn sp3_nested_parens(k: usize) -> String {
    format!("let x = {}1{}\nprint(x)\n", "(".repeat(k), ")".repeat(k))
}

/// Run `src` on BOTH engines (tree-walker + specialized VM + generic VM) on an
/// enlarged worker stack and assert BYTE-IDENTICAL outcome: either all three
/// succeed with identical stdout, or all three error with the SAME message. The
/// recursion guard trips at the SAME logical depth on every engine; only the
/// panic's diagnostic SPAN differs (the tree-walker anchors at the innermost
/// subexpression, the VM at the call), so this compares the message, not the span
/// — the observable result (stdout + message + exit) must match exactly.
fn sp3_assert_three_way_identical(src: &str) {
    let src = src.to_string();
    // `AsError` is `!Send` (it holds `Rc<SourceInfo>`), so the worker thread reduces
    // each outcome to a `Send` comparable summary BEFORE crossing the join: either
    // `Ok((stdout, exit))` or `Err(message)`. The recursion panic's diagnostic span
    // differs by engine, so the message (not the span) is the byte-identical
    // observable — exactly what the differential cares about (stdout + outcome).
    type Summary = Result<(String, Option<i32>), String>;
    let (tw, vm, gen): (Summary, Summary, Summary) =
        ascript::run_on_worker_stack(move || async move {
            let summarize =
                |r: Result<(String, Option<i32>), ascript::error::AsError>| -> Summary {
                    r.map_err(|e| e.message)
                };
            let tw = summarize(ascript::run_source_exit(&src).await);
            let vm = summarize(ascript::vm_run_source(&src).await);
            let gen = summarize(ascript::vm_run_source_generic(&src).await);
            (tw, vm, gen)
        });
    assert_eq!(
        tw, vm,
        "tree-walker vs specialized VM diverged\n  tw: {tw:?}\n  vm: {vm:?}"
    );
    assert_eq!(
        vm, gen,
        "specialized vs generic VM diverged\n  vm: {vm:?}\n  gen: {gen:?}"
    );
}

#[test]
fn sp3_recursion_at_limit_minus_one_ok_identical() {
    // SPEC §B7 boundary: a recursion to EXACTLY `MAX_CALL_DEPTH - 1` is the LAST
    // depth that completes — and it must complete IDENTICALLY (same numeric stdout,
    // exit 0) on all three engines. This is the strong oracle: if the tree-walker
    // tripped earlier than the VM (the per-call double-count bug), this case would
    // diverge (tree-walker errors, VM prints the number).
    let n = (ascript::interp::MAX_CALL_DEPTH - 1) as usize;
    sp3_assert_three_way_identical(&sp3_rec_src(n));
}

#[test]
fn sp3_recursion_at_limit_panics_identical() {
    // SPEC §B7 boundary: a recursion to EXACTLY `MAX_CALL_DEPTH` is the FIRST depth
    // that fails — and it must fail IDENTICALLY (same `maximum recursion depth
    // exceeded` message, no stdout, non-134 exit) on all three engines.
    let n = ascript::interp::MAX_CALL_DEPTH as usize;
    sp3_assert_three_way_identical(&sp3_rec_src(n));
}

#[test]
fn sp3_recursion_over_limit_panics_identical() {
    // Comfortably over the cap → all three engines emit the recursion panic, no
    // stdout, non-134 exit — byte-identical (message + outcome).
    let n = (ascript::interp::MAX_CALL_DEPTH + 50) as usize;
    sp3_assert_three_way_identical(&sp3_rec_src(n));
}

#[test]
fn sp3_mutual_recursion_over_limit_panics_identical() {
    // Mutual recursion proves the counter is per-LOGICAL-CALL, not per-function:
    // a/b alternate, so neither alone reaches the cap, but their combined depth does.
    let n = ascript::interp::MAX_CALL_DEPTH + 50;
    let src = format!(
        "fn a(n) {{ if (n <= 0) {{ return 0 }} return b(n - 1) }}\n\
         fn b(n) {{ if (n <= 0) {{ return 0 }} return a(n - 1) }}\n\
         print(a({n}))\n"
    );
    sp3_assert_three_way_identical(&src);
}

#[test]
fn sp3_recover_catches_recursion_limit_identical() {
    // `recover` catches the Tier-2 recursion panic (it is an ordinary recoverable
    // panic). The handler observes `[nil, err]` with the fixed message — identical
    // on both engines. `f(n+1)` recurses without a base case → guaranteed over-cap.
    let src = "fn f(n) { return f(n + 1) }\n\
               let r = recover(() => f(0))\n\
               print(r[1].message)\n";
    sp3_assert_three_way_identical(src);
}

#[test]
fn sp3_recover_then_recurse_resets_depth_identical() {
    // After `recover` catches an over-limit recursion, the depth counter MUST be
    // restored to the caller's level so a subsequent normal recursion is not
    // wrongly tripped. The tree-walker gets this via its RAII guard unwinding; the
    // VM via the snapshot-restore guard at its re-entrant `run` boundary. Both must
    // print the recovered message AND then complete g(500) identically.
    let src = "fn f(n) { return f(n + 1) }\n\
               let r = recover(() => f(0))\n\
               print(r[1].message)\n\
               fn g(n) { if (n <= 0) { return 0 } return 1 + g(n - 1) }\n\
               print(g(500))\n";
    sp3_assert_three_way_identical(src);
}

#[test]
fn sp3_nested_expr_at_limit_minus_one_ok_identical() {
    // Expression nesting is a SEPARATE dimension (`EXPR_NEST_LIMIT`). At exactly
    // `EXPR_NEST_LIMIT - 1` it is the LAST nesting that compiles + evaluates — to `1`
    // on all three engines, identically.
    let k = (ascript::interp::EXPR_NEST_LIMIT - 1) as usize;
    sp3_assert_three_way_identical(&sp3_nested_parens(k));
}

#[test]
fn sp3_nested_expr_at_limit_panics_identical() {
    // At exactly `EXPR_NEST_LIMIT` the nesting trips identically on all three: the
    // tree-walker at runtime `eval_expr`, the VM at compile-time `compile_expr` —
    // BOTH surfaced as the SAME `maximum recursion depth exceeded`, no stdout,
    // non-134 exit. (This far-edge case must NOT reintroduce the call-depth
    // double-count — the recursion boundary tests above guard against that.)
    let k = ascript::interp::EXPR_NEST_LIMIT as usize;
    sp3_assert_three_way_identical(&sp3_nested_parens(k));
}

// ── SP8 Phase A: index-stable global-access fast path (regression guards) ──
// These exercise the GET_GLOBAL/SET_GLOBAL `IndexBound` cache. They must stay
// byte-identical across tree-walker == generic VM == specialized VM — the cache
// only changes the lookup mechanism (stable index vs string hash), never the value.

#[tokio::test]
async fn sp8_global_reassign_loop() {
    // Hot reassigned top-level `let` in a loop (the regression target): the index
    // cache must hit every iteration (a SET never bumps `struct_gen`).
    assert_three_way_matches("let sum = 0\nfor (i in 0..1000) { sum = sum + i }\nprint(sum)\n")
        .await;
}

#[tokio::test]
async fn sp8_global_while_two() {
    // Two globals read+written each iteration (the while-loop shape, regressed most).
    assert_three_way_matches(
        "let i = 0\nlet s = 0\nwhile (i < 1000) { s = s + i\n i = i + 1 }\nprint(s)\n",
    )
    .await;
}

#[tokio::test]
async fn sp8_global_forward_ref() {
    // Forward/late read of a top-level `let` from a function defined earlier.
    assert_three_way_matches("fn get() { return later }\nlet later = 42\nprint(get())\n").await;
}

#[tokio::test]
async fn sp8_global_shadows_builtin() {
    // A user-global shadows a builtin (resolution order: user-global wins — the
    // index cache is filled only after user-globals resolve first).
    assert_three_way_matches("let print2 = 7\nlet len = 99\nprint(len)\n").await;
}

#[tokio::test]
async fn sp8_global_const_reassign() {
    // Immutable global reassignment -> same-chunk compile-time immutable error.
    assert_three_way_matches("const k = 1\nk = 2\nprint(k)\n").await;
}

#[tokio::test]
async fn sp8_global_redeclare() {
    // Redeclaration -> 'already defined in this scope' (errors before any cache read;
    // the 1st define bumps `struct_gen`).
    assert_three_way_matches("let x = 1\nlet x = 2\nprint(x)\n").await;
}

#[tokio::test]
async fn sp8_global_use_before_define() {
    // Reference to a not-yet-defined global from a CALL before its define ->
    // undefined variable. The index cache must NEVER serve a value for an unresolved
    // name — both VM engines must agree (the SP8 load-bearing assertion) and both
    // must error. We compare spec==generic and that both error, rather than the full
    // byte-identical span against the tree-walker, because `return <ident>` carries a
    // PRE-EXISTING front-end span offset between the VM and tree-walker unrelated to
    // SP8 (verified on the clean tree before these changes).
    let src = "fn get() { return nope }\nprint(get())\n";
    let spec = ascript::vm_run_source(src).await;
    let generic = ascript::vm_run_source_generic(src).await;
    let norm = |r: &Result<(String, Option<i32>), ascript::error::AsError>| match r {
        Ok((out, code)) => Ok((out.clone(), *code)),
        Err(e) => Err(e.to_string()),
    };
    assert_eq!(
        norm(&spec),
        norm(&generic),
        "SP8 index cache must not diverge spec vs generic on use-before-define"
    );
    assert!(
        spec.is_err(),
        "use-before-define must error (undefined variable), got {:?}",
        norm(&spec)
    );
}

#[tokio::test]
async fn sp8_global_define_then_reread_in_loop() {
    // A new define mid-program bumps `struct_gen`, invalidating an earlier site's
    // cache; the later loop still reads the correct (possibly re-resolved) value.
    assert_three_way_matches(
        "let a = 1\nlet b = 10\nlet total = 0\n\
         for (i in 0..100) { total = total + a + b\n a = a + 1 }\nprint(total)\n",
    )
    .await;
}

// ── SP8 Phase B (#136): capture-by-value closure optimization ──────────────
// A captured-but-never-reassigned binding is captured BY VALUE; a reassigned one
// stays a shared cell. Both must be byte-identical across all three engines.
// (AScript's anonymous-closure form is the arrow `=>`, not a `fn` expression.)

#[tokio::test]
async fn sp8_capture_const() {
    // Captured-but-never-reassigned constant: by-value eligible (== by-ref result).
    assert_three_way_matches(
        "fn make() {\n let k = 10\n return () => k\n}\nlet f = make()\nprint(f())\n",
    )
    .await;
}

#[tokio::test]
async fn sp8_capture_counter() {
    // Captured AND reassigned (counter): stays a shared cell — mutation visible.
    assert_three_way_matches(
        "fn make() {\n let n = 0\n return () => {\n n = n + 1\n return n\n }\n}\n\
         let c = make()\nprint(c())\nprint(c())\nprint(c())\n",
    )
    .await;
}

#[tokio::test]
async fn sp8_capture_loop_fresh() {
    // Per-iteration capture freshness: each closure captures its own iteration's `v`
    // (by value), so the three closures print 0/10/20, not 20/20/20. (Array append is
    // spread reassignment in AScript — there is no `.push`.)
    assert_three_way_matches(
        "let fns = []\nfor (i in 0..3) {\n let v = i * 10\n fns = [...fns, () => v]\n}\n\
         for (g in fns) { print(g()) }\n",
    )
    .await;
}

#[tokio::test]
async fn sp8_capture_transitive() {
    // Transitive capture (closure over closure) of a never-reassigned binding.
    assert_three_way_matches("fn a() {\n let k = 5\n return () => (() => k)\n}\nprint(a()()())\n")
        .await;
}

#[tokio::test]
async fn sp8_capture_mixed() {
    // Mixed: one captured-constant + one captured-counter in the same closure.
    assert_three_way_matches(
        "fn make() {\n let base = 100\n let n = 0\n return () => {\n n = n + 1\n \
         return base + n\n }\n}\nlet c = make()\nprint(c())\nprint(c())\n",
    )
    .await;
}

#[tokio::test]
async fn sp8_capture_before_later_reassignment() {
    // THE SUBTLE CASE: capture textually BEFORE a reassignment must stay BY-REF, so
    // the closure observes the later mutation (prints 7, not 0). A wrong by-value
    // decision would freeze the captured 0 and diverge.
    assert_three_way_matches(
        "fn make() {\n let n = 0\n let f = () => n\n n = 7\n return f\n}\n\
         let g = make()\nprint(g())\n",
    )
    .await;
}

// ── Workers §11.3: all-four-modes byte-identical verification ─────────────────
//
// The three-way gate (tree-walker == specialized VM == generic VM) over the whole
// corpus already covers the worker examples (they are not in EXAMPLE_SKIPS and are
// auto-enumerated by all_corpus_examples()). The FOURTH mode required by §11.3 is
// `.aso`-compiled: `ascript build <example>.as -o <tmp>.aso` then `ascript run
// <tmp>.aso`. This proves the worker bytecode survives serialization through the
// `.aso` format and produces byte-identical stdout.
//
// Worker programs are order-deterministic by construction (gather + ordered consume),
// so the stdout oracle is stable across runs. ASCRIPT_WORKERS=2 caps the pool to
// keep resource pressure low under parallel test execution.

/// Build `source_path` to a `.aso` in a unique temp directory and run it via the
/// real `ascript` binary. Returns the stdout of the run. Sets `ASCRIPT_WORKERS=2`
/// so the pool stays lean during parallel test execution (all worker examples use
/// at most a handful of isolates and are order-deterministic regardless of pool
/// size — only throughput differs, not output).
async fn build_and_run_aso(source_path: &std::path::Path) -> String {
    let bin = env!("CARGO_BIN_EXE_ascript");
    // Unique temp dir — no two parallel tests share a dir.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tag = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prog");
    let dir = std::env::temp_dir().join(format!("ascript_workers_aso_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create temp dir: {e}"));
    // Copy the source into the temp dir so relative paths stay self-contained.
    let src_name = source_path.file_name().unwrap();
    let dest = dir.join(src_name);
    std::fs::copy(source_path, &dest)
        .unwrap_or_else(|e| panic!("copy {} to temp dir: {e}", source_path.display()));
    // `ascript build <file>` → produces `<stem>.aso` in the same dir.
    let build_out = std::process::Command::new(bin)
        .arg("build")
        .arg(src_name)
        .current_dir(&dir)
        .env("ASCRIPT_WORKERS", "2")
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript build: {e}"));
    assert!(
        build_out.status.success(),
        "ascript build {} failed\n  stdout: {}\n  stderr: {}",
        source_path.display(),
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );
    let stem = source_path.file_stem().unwrap().to_string_lossy();
    let aso_path = dir.join(format!("{stem}.aso"));
    assert!(
        aso_path.exists(),
        "expected {aso_path:?} after `ascript build`"
    );
    // `ascript run <stem>.aso` — VM path, no compile step.
    let run_out = std::process::Command::new(bin)
        .arg("run")
        .arg(format!("{stem}.aso"))
        .current_dir(&dir)
        .env("ASCRIPT_WORKERS", "2")
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript run .aso: {e}"));
    // Non-zero exit is itself a divergence (all worker examples exit 0).
    assert!(
        run_out.status.success(),
        "ascript run {stem}.aso exited with {:?}\n  stdout: {}\n  stderr: {}",
        run_out.status.code(),
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    String::from_utf8_lossy(&run_out.stdout).into_owned()
}

/// IFACE Task 9: build inline `src` text to a `.aso` and run it via the real
/// `ascript` binary, returning stdout. Mirrors [`build_and_run_aso`] but for an
/// in-test source string (writes it to a unique temp `.as` first). Used to add
/// `.aso` as the fourth mode for interface programs.
async fn build_and_run_aso_from_src(src: &str, tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ascript_iface_aso_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create temp dir: {e}"));
    let src_path = dir.join(format!("{tag}.as"));
    std::fs::write(&src_path, src).unwrap_or_else(|e| panic!("write temp .as: {e}"));
    build_and_run_aso(&src_path).await
}

/// Build inline `src` to `.aso`, run it, and return `(run_succeeded, stdout, stderr)`
/// WITHOUT asserting success — for interface ERROR-path programs whose `.aso` run must
/// FAIL the same way the other three engines do (a runtime panic; the build itself
/// still succeeds because cyclic/bad-`extends`/contract faults are runtime, not
/// compile-time). Mirrors [`build_and_run_aso`] otherwise.
async fn build_and_run_aso_status(src: &str, tag: &str) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ascript_iface_err_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create temp dir: {e}"));
    let src_name = format!("{tag}.as");
    std::fs::write(dir.join(&src_name), src).unwrap_or_else(|e| panic!("write temp .as: {e}"));
    let build_out = std::process::Command::new(bin)
        .arg("build")
        .arg(&src_name)
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript build: {e}"));
    // The build MUST succeed — these faults are runtime, not compile-time. (If a future
    // change makes them compile errors, this assert flags the behavior change loudly.)
    assert!(
        build_out.status.success(),
        "ascript build {tag} should succeed (runtime fault, not compile error)\n  stderr: {}",
        String::from_utf8_lossy(&build_out.stderr)
    );
    let run_out = std::process::Command::new(bin)
        .arg("run")
        .arg(format!("{tag}.aso"))
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn ascript run .aso: {e}"));
    (
        run_out.status.success(),
        String::from_utf8_lossy(&run_out.stdout).into_owned(),
        String::from_utf8_lossy(&run_out.stderr).into_owned(),
    )
}

/// Strip ANSI SGR escape sequences (`\x1b[…m`) so a rendered CLI diagnostic can be
/// substring-matched against the raw `AsError.message`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Consume up to and including the final 'm' of the SGR sequence.
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// IFACE Task 9: interface programs must be byte-identical across the FULL four-mode
/// matrix (tree-walker == specialized VM == generic VM == `.aso`). Unit A's iface
/// tests only ran the first three because `.aso` REJECTED interface programs; now that
/// `.aso` serializes interface descriptors (v25), `.aso` joins the identity. Each case
/// is a complete `print`-driven program so stdout is the comparison surface.
#[tokio::test]
async fn iface_programs_four_mode_byte_identical() {
    let cases: &[(&str, &str)] = &[
        (
            "instanceof",
            "interface R { fn read(b): int }\nclass File { fn read(b) { return 0 } }\nclass NoRead { fn write(b) { return 0 } }\nprint(File() instanceof R)\nprint(NoRead() instanceof R)\nprint(5 instanceof R)",
        ),
        (
            "inherited",
            "interface R { fn read(b): int }\nclass Base { fn read(b) { return 1 } }\nclass Sub extends Base {}\nprint(Sub() instanceof R)",
        ),
        (
            "compose",
            "interface Reader { fn read(b): int }\ninterface Writer { fn write(b): int }\ninterface RW extends Reader, Writer {}\nclass Sock { fn read(b) { return 1 } fn write(b) { return 2 } }\nclass OnlyR { fn read(b) { return 1 } }\nprint(Sock() instanceof RW)\nprint(OnlyR() instanceof RW)",
        ),
        (
            "contract",
            "interface R { fn read(b): int }\nclass File { fn read(b) { return 7 } }\nfn slurp(r: R) { return r.read(0) }\nprint(slurp(File()))",
        ),
        (
            "printiface",
            "interface R { fn read(b): int }\nprint(R)",
        ),
        (
            "arity",
            "interface R1 { fn read(b): int }\ninterface R2 { fn read(b, o): int }\nclass D { fn read(b, opts) { return 0 } }\nprint(D() instanceof R1)\nprint(D() instanceof R2)",
        ),
        (
            "implements",
            "interface R { fn read(b): int }\nclass File implements R { fn read(b) { return 9 } }\nprint(File() instanceof R)\nprint(File().read(0))",
        ),
    ];
    for (tag, src) in cases {
        // Modes 1–3: tree-walker, specialized VM, generic VM (captured stdout).
        let tw = ascript::run_source(src).await.expect("tree-walker ok");
        let (vm, _) = ascript::vm_run_source(src).await.expect("specialized vm ok");
        let (gen, _) = ascript::vm_run_source_generic(src)
            .await
            .expect("generic vm ok");
        assert_eq!(tw, vm, "specialized VM diverged for `{tag}`");
        assert_eq!(tw, gen, "generic VM diverged for `{tag}`");
        // Mode 4: .aso-compiled (the new mode this task wires in).
        let aso = build_and_run_aso_from_src(src, tag).await;
        assert_eq!(
            tw, aso,
            ".aso output diverged from tree-walker for iface `{tag}`\n  tw: {tw:?}\n  aso: {aso:?}"
        );
    }
}

/// IFACE review follow-up: interface ERROR paths (cyclic `extends`, `extends` a class /
/// unknown name, a non-conforming interface-typed contract) must FAIL byte-identically
/// across ALL FOUR modes — the three in-process engines (identical `AsError.message`)
/// AND the `.aso`-compiled binary (non-zero exit whose rendered stderr carries the same
/// message). The faults fire in the shared `flatten_interface_inner`/`check_type_env`,
/// so `.aso` is identical by construction; this locks that so a future regression in
/// `.aso` panic parity can't slip past the gates.
#[tokio::test]
async fn iface_error_paths_four_mode_byte_identical() {
    let cases: &[(&str, &str)] = &[
        (
            "cycle",
            "interface A extends B {}\ninterface B extends A {}\nclass C {}\nprint(C() instanceof A)",
        ),
        (
            "badclass",
            "class K {}\ninterface I extends K {}\nclass C { fn m() {} }\nprint(C() instanceof I)",
        ),
        (
            "unknown",
            "interface I extends Nope {}\nclass C {}\nprint(C() instanceof I)",
        ),
        (
            "contract",
            "interface R { fn read(b): int }\nclass NoRead { fn write(b) { return 0 } }\nfn slurp(r: R) { return 0 }\nprint(slurp(NoRead()))",
        ),
    ];
    for (tag, src) in cases {
        // Modes 1–3: all three in-process engines error with an IDENTICAL message.
        let tw = ascript::run_source(src).await.expect_err("tree-walker should error");
        let vm = ascript::vm_run_source(src).await.expect_err("specialized vm should error");
        let gen = ascript::vm_run_source_generic(src)
            .await
            .expect_err("generic vm should error");
        assert_eq!(tw.message, vm.message, "specialized VM message diverged for `{tag}`");
        assert_eq!(tw.message, gen.message, "generic VM message diverged for `{tag}`");
        // Mode 4: .aso-compiled — build succeeds, run FAILS non-zero, and the rendered
        // stderr (ANSI-stripped) carries the same panic message.
        let (ok, _out, err) = build_and_run_aso_status(src, tag).await;
        assert!(!ok, ".aso run for iface error `{tag}` should exit non-zero");
        let err = strip_ansi(&err);
        assert!(
            err.contains(&tw.message),
            ".aso stderr for `{tag}` must carry the same panic message\n  expected substring: {:?}\n  stderr: {err}",
            tw.message
        );
    }
}

/// TYPE Unit-B review regression: the expression-level `<` disambiguation
/// (type-args-vs-comparison) must produce IDENTICAL parses on the legacy and CST
/// front-ends, so the engines execute identically. A token-balancer earlier diverged
/// on `a < b() > (c)` (a `(` inside the angle span — must be comparison) and
/// `f<fn(int) -> string>(x)` (the `->` arrow's `>` is not a list close — must be a
/// generic call). These behavioral cases lock both decisions across all engines.
#[tokio::test]
async fn type_arg_disambiguation_byte_identical() {
    // Generic CALLS (the `>` is immediately followed by `(`): erased, run identically.
    let ok_cases = [
        "fn f<T>(x: T) { return x }\nprint(f<int>(5))",
        "class Box<T> { v: T\n  fn get() { return self.v } }\nprint(Box<int>(7).get())",
        // FnSig type argument — the `->` must NOT close the angle list (B2).
        "fn f<T>(x: T) { return x }\nprint(f<fn(int) -> string>(5))",
        // Nested generic closes via the `>>`-split, then the `(`.
        "fn f<T>(x: T) { return x }\nprint(f<Box<int>>(9))\nclass Box<T> { v: T }",
        // Comparison/bitwise are UNAFFECTED (no `>` immediately before `(`).
        "let a = 3\nlet b = 5\nprint(a < b)\nprint(b > a)\nprint(a << 1)\nprint(b >> 1)\nprint(a < b && b > a)",
        // A `<`/`>` pair around a parenthesised arg that is NOT a generic head.
        "fn lt(x, y) { return x < y }\nfn gt(x, y) { return x > y }\nprint(lt(1, 2))\nprint(gt(1, 2))",
    ];
    for src in ok_cases {
        assert_opt_call_ok_three_way(src).await;
    }
    // B1: `a < b() > (c)` is a comparison chain — `bool > number` panics IDENTICALLY on
    // all three engines (a parse divergence would instead surface as a VM parse error).
    assert_opt_call_error_three_way(
        "fn b() { return 2 }\nlet a = 1\nlet c = 3\nprint(a < b() > (c))",
    )
    .await;
}

/// Workers §11.3: every worker example must produce IDENTICAL, order-deterministic
/// output across all four modes: tree-walker, specialized VM, generic VM, and
/// .aso-compiled. Worker programs are byte-identical by construction (gather +
/// ordered consume).
#[tokio::test]
async fn worker_examples_all_modes_byte_identical() {
    let root = env!("CARGO_MANIFEST_DIR");
    // Select by USAGE, not filename: ANY example that dispatches a worker — a pooled
    // `worker fn`, a `worker class`, or the dedicated `run_in_worker({caps})` path — must
    // be four-mode byte-identical (incl. `ascript run x.aso`). Matching the source (not a
    // `workers_*` filename) ensures a differently-named worker example (e.g.
    // `caps_sandbox.as`, which exercises the dedicated `.aso`-sensitive path) can't slip
    // the `.aso` mode — the gap that let a real four-mode divergence through.
    let worker_examples: Vec<String> = all_corpus_examples()
        .into_iter()
        .filter(|p| {
            let path = std::path::Path::new(root).join(p);
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            src.contains("worker fn")
                || src.contains("worker class")
                || src.contains("run_in_worker")
        })
        .collect();
    assert!(
        !worker_examples.is_empty(),
        "no worker examples found — expected worker-using .as files in examples/ or examples/advanced/"
    );
    for rel in &worker_examples {
        let path = std::path::Path::new(root).join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read worker example {rel}: {e}"));
        // Skip if a required stdlib module is unavailable in this build config.
        if feature_unavailable_in_this_build(&src).await {
            continue;
        }
        // Modes 1–3: tree-walker, specialized VM, generic VM (already covered by the
        // whole-corpus three-way gate, but we assert them here too for an explicit
        // per-worker-example §11.3 record and to obtain the tree-walker reference).
        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on worker example {rel}: {e:?}"));
        let spec = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("specialized VM failed on worker example {rel}: {e:?}"));
        let gen = ascript::vm_run_source_generic(&src)
            .await
            .unwrap_or_else(|e| panic!("generic VM failed on worker example {rel}: {e:?}"));
        assert_eq!(
            tw, spec,
            "tree-walker vs specialized VM diverged for worker example {rel}"
        );
        assert_eq!(
            tw, gen,
            "tree-walker vs generic VM diverged for worker example {rel}"
        );
        // Mode 4: .aso-compiled. Build the source to bytecode, run the .aso, compare
        // stdout. This is the new assertion that Task 15 adds.
        let aso_out = build_and_run_aso(&path).await;
        assert_eq!(
            tw.0, aso_out,
            ".aso output diverged from tree-walker for worker example {rel}\n  \
             tree-walker stdout: {:?}\n  .aso stdout:        {aso_out:?}",
            tw.0
        );
    }
    eprintln!(
        "worker §11.3 all-modes gate: {} worker examples verified across \
         tree-walker + specialized VM + generic VM + .aso",
        worker_examples.len()
    );
}

// ----- NUM: annotated `let`/`const` runtime type contracts (CHECK_LOCAL) -------
// A `: T` annotation on a `let`/`const` binding is a runtime contract on the
// initializer value, byte-identical to the tree-walker's `Stmt::Let` check (same
// `type contract violated: expected {ty}, got {name} ({value})` message, same span
// = the init EXPRESSION's span). Before CHECK_LOCAL the VM silently bound the
// mismatched value; these regressions trip if that divergence ever returns.

#[tokio::test]
async fn vm_let_contract_mismatch_matches_treewalker() {
    // Each MUST panic byte-identically on tree-walker == specialized-VM == generic-VM.
    let cases = [
        "let x: int = 5.0\nprint(x)\n",
        "let x: float = 5\nprint(x)\n",
        "let s: string = 5\nprint(s)\n",
        "const c: int = 2.5\nprint(c)\n",
        // module-global path AND slot-local (inside a fn body) path.
        "fn f() {\n  let y: int = 1.5\n  return y\n}\nprint(f())\n",
        "fn g() {\n  const k: string = 7\n  return k\n}\nprint(g())\n",
    ];
    for src in cases {
        assert_opt_call_error_three_way(src).await;
    }
}

#[tokio::test]
async fn vm_let_contract_ok_matches_treewalker() {
    // Well-typed annotated bindings must run identically (no spurious panic) and
    // print the value the same way on all three engines.
    let cases = [
        "let x: int = 5\nprint(x)\n",
        "let x: number = 5\nprint(x)\n",
        "let x: number = 5.0\nprint(x)\n",
        "const c: int = 2\nprint(c)\n",
        "let s: string = \"hi\"\nprint(s)\n",
        // int is a subtype of number; float is a subtype of number.
        "let n: number = 5\nlet f: float = 5.0\nprint(n + f)\n",
        // slot-local path inside a fn body.
        "fn f() {\n  let y: int = 5\n  return y\n}\nprint(f())\n",
        // optional / nil-bearing annotation accepts nil.
        "let m: int? = nil\nprint(m)\n",
    ];
    for src in cases {
        assert_opt_call_ok_three_way(src).await;
    }
}

// ---- IFACE Task 6: interfaces byte-identical across tree-walker / spec / generic ----

#[tokio::test]
async fn iface_instanceof_and_contract_three_way() {
    let cases = [
        // structural instanceof: conforming, non-conforming, non-instance
        "interface R { fn read(b): int }\nclass File { fn read(b) { return 0 } }\nclass NoRead { fn write(b) { return 0 } }\nprint(File() instanceof R)\nprint(NoRead() instanceof R)\nprint(5 instanceof R)",
        // inherited method satisfies
        "interface R { fn read(b): int }\nclass Base { fn read(b) { return 1 } }\nclass Sub extends Base {}\nprint(Sub() instanceof R)",
        // composition via extends (transitive union)
        "interface Reader { fn read(b): int }\ninterface Writer { fn write(b): int }\ninterface RW extends Reader, Writer {}\nclass Sock { fn read(b) { return 1 } fn write(b) { return 2 } }\nclass OnlyR { fn read(b) { return 1 } }\nprint(Sock() instanceof RW)\nprint(OnlyR() instanceof RW)",
        // interface-typed param contract accepts a conforming arg
        "interface R { fn read(b): int }\nclass File { fn read(b) { return 7 } }\nfn slurp(r: R) { return r.read(0) }\nprint(slurp(File()))",
        // a class instanceof still nominal (unchanged)
        "class A {}\nclass B extends A {}\nprint(B() instanceof A)\nprint(A() instanceof B)",
        // printing an interface value
        "interface R { fn read(b): int }\nprint(R)",
        // arity table: defaulted param satisfies arity-1 and arity-2 requirements
        "interface R1 { fn read(b): int }\ninterface R2 { fn read(b, o): int }\nclass D { fn read(b, opts) { return 0 } }\nprint(D() instanceof R1)\nprint(D() instanceof R2)",
        // a REST class method (min 0, max ∞) satisfies a fixed-arity requirement —
        // the tw `find_method` arity check and the VM compiled-proto side table must agree
        "interface R2 { fn read(b, o): int }\nclass V { fn read(...xs) { return 0 } }\nprint(V() instanceof R2)",
        // a REST requirement demands a variadic method: a rest method conforms, a
        // fixed-arity one does not (exercises req.has_rest on both engines)
        "interface RV { fn log(...xs) }\nclass L { fn log(...ys) { return 0 } }\nclass L2 { fn log(a) { return 0 } }\nprint(L() instanceof RV)\nprint(L2() instanceof RV)",
    ];
    for src in cases {
        assert_opt_call_ok_three_way(src).await;
    }
}

#[tokio::test]
async fn iface_contract_and_extends_errors_three_way() {
    // Interface enforcement + malformed-`extends` must FAIL byte-identically on all three
    // engines (tree-walker == specialized == generic). Closes the IFACE-review test gaps:
    // a non-conforming arg to an interface-typed param, a cyclic `extends`, and an
    // `extends` naming a class / an unknown name.
    let cases = [
        // negative interface-typed param contract: NoRead lacks `read` → rejected
        "interface R { fn read(b): int }\nclass NoRead { fn write(b) { return 0 } }\nfn slurp(r: R) { return 0 }\nprint(slurp(NoRead()))",
        // cyclic extends → recoverable panic naming the chain (no stack overflow)
        "interface A extends B {}\ninterface B extends A {}\nclass C {}\nprint(C() instanceof A)",
        // extends a CLASS, not an interface → clean error
        "class K {}\ninterface I extends K {}\nclass C { fn m() {} }\nprint(C() instanceof I)",
        // extends an UNKNOWN name → clean error
        "interface I extends Nope {}\nclass C {}\nprint(C() instanceof I)",
    ];
    for src in cases {
        assert_opt_call_error_three_way(src).await;
    }
}

#[tokio::test]
async fn iface_instanceof_bad_rhs_three_way() {
    // `x instanceof <a number value>` panics identically on all three engines.
    let cases = [
        "let n = 5\nlet x = 3\nx instanceof n",
    ];
    for src in cases {
        assert_opt_call_error_three_way(src).await;
    }
}
