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
//  2. GROWING-CORPUS SCAFFOLD (`SYNC_EXAMPLE_ALLOWLIST` + `vm_run_allowlisted_examples_*`):
//     a curated allow-list of WHOLE `examples/*.as` files run end-to-end (file
//     contents → VM stdout+exit == tree-walker stdout+exit). The list GROWS one
//     slice at a time as the VM gains features:
//       - V2  (now): only examples restricted to literals/locals/arithmetic/
//                     containers/short-circuit/print qualify.
//       - V3  adds `if`/`while`/`for` examples (e.g. ranges, factorial, ...).
//       - V4  adds `fn` definitions/calls examples (functions, ...).
//       - V5..V9 add closures, `?`/`!`/recover, async/await, generators,
//                 classes/enums.
//       - V10 FLIPS the gate to the ENTIRE `examples/` corpus and this allow-list
//             is retired in favour of the whole-corpus runner.
//     Add files here the moment the VM can run them; never remove the byte-
//     identical assertion to make a file "pass".

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

/// Whole `examples/*.as` files (paths relative to the crate root) that the VM can
/// run end-to-end TODAY. See the module note above: this list grows one VM slice
/// at a time and is retired at V10 when the gate flips to the entire corpus.
///
/// V2 qualifiers: programs restricted to literals, locals (`let`/`const`/assign),
/// block scoping, globals/bare-builtins, arithmetic/comparison/equality/range,
/// short-circuit `&&`/`||`/`??`, array/object literals, index/member reads, and
/// `print`. They must contain NO `fn`, control flow (`if`/`while`/`for`/`match`),
/// `class`/`enum`, `import`, async/await, generators, `?`/`!`/recover, or
/// destructuring/spread — those arrive in V3..V10.
///
/// V3 qualifiers (added now that the VM supports control flow): examples that
/// also use `if`/`else`/`while`/`for (i in a..b)`/`for (x of …)`/ternary, but
/// STILL contain NO `fn`, `match`, `class`/`enum`, `import`, async/await,
/// generators, `?`/`!`/recover, destructuring/spread, OR compound assignment
/// (`+=`/`-=`/`*=`/`/=`, deferred to V9). NOTE: `for (i in <value>)` over a NAME
/// bound to a range/array value now qualifies — the VM routes `in` over a
/// non-`RangeExpr` to the for-of path (matching the legacy parser's `in` overload),
/// so a range-VALUE for-in iterates fine (V3-T4b).
///
/// V4 qualifiers (added now that the VM supports user functions — definitions,
/// calls, recursion, mutual/forward refs, closures, params/rest/contracts): an
/// example that ALSO uses `fn`/arrows/calls now qualifies, provided it STILL
/// contains none of V7..V12: async/await, generators, `match`, `class`/`enum`,
/// `import`, destructuring/spread, OR compound assignment (`+=`/`-=`/`*=`/`/=`,
/// V9). The set of files that match byte-identically is established empirically
/// (run both engines over each `examples/*.as` and keep only the ones that
/// agree); see the audit note below each entry.
///
/// V6 qualifiers (added now that the VM supports the error model — `?` propagate,
/// `!` unwrap, `recover`, panic unwinding, diagnostics parity): an example that
/// ALSO uses `?`/`!`/`recover`/`Ok`/`Err` now qualifies, provided it STILL
/// contains none of V7..V12 (async/await, generators, `match`, `class`/`enum`,
/// `import`, destructuring/spread, compound assignment, member-method CALLS).
/// Verified empirically by running both engines over every `examples/*.as` and
/// keeping only the byte-identical agreers. Deliberately EXCLUDED, with the
/// verified reason (from that scan):
///   - `functions.as` — uses `+=` (line 9; V9 compound assignment).
///   - `factorial.as` — uses `*=` (V9 compound assignment).
///   - `data.as`      — uses `+=` (V9 compound assignment).
///   - `typed.as`     — uses `+=` (V9) and async/await (V7).
///   - `validation.as`— uses `import` (V12) AND destructuring `let [u, e1] = …`
///     (V10), so the VM rejects it at compile time (it does NOT diverge): it
///     exercises `?`/`!`/recover only via the stdlib `std/schema` it imports.
///   - every other function-heavy example uses `import` (V12), `match` (V10),
///     `class`/`enum` (V9), async/generators (V7/V8), destructuring/spread (V10),
///     or compound assignment (V9) — none of which the VM compiles yet, so they
///     ERROR (rather than diverge) and are correctly not on the list.
const SYNC_EXAMPLE_ALLOWLIST: &[&str] = &[
    "examples/hello.as",   // `print(1 + 2 * 3)` — literals + arithmetic + print.
    "examples/numbers.as", // numeric-literal forms bound to locals, then printed.
    // V3 control-flow qualifier:
    "examples/strings.as", // string literals + escapes through print.
    // V3-T4b: range/array VALUE for-in (`for (i in r)`) routes to for-of; also uses
    // `let` w/o initializer, typed decl, `len`, and a literal-range for-in.
    "examples/ranges.as",
    // V4 function qualifier: typed locals with the `T?` nullable suffix, a `fn`
    // with optional (`string?`) params + return type, calls, and `assert`. No
    // `import`/compound-assign/`match`/`class`/`?`-operator — byte-identical
    // (verified by running both engines over the file).
    "examples/optional_types.as",
    // V6 error-model qualifiers (verified byte-identical by running both engines):
    // `safeDivide` returns `Ok`/`Err` Result pairs, `compute` chains `?`
    // propagation through two calls, a ternary reuses `?`, and `recover` wraps a
    // function that out-of-bounds-indexes (a Tier-2 panic → `[nil, err]`). No
    // `import`/`match`/`class`/destructuring/compound-assign — all V6 features.
    "examples/result.as",
    // `!` force-unwrap on Result pairs (success yields the value; an error pair
    // panics, caught by `recover` which round-trips the `.message`), plus arrows,
    // `assert`, and `Ok`/`Err`. All V6 — byte-identical on both engines.
    "examples/force_unwrap.as",
    // V7 async qualifier (verified byte-identical by running both engines): uses
    // only `async fn`, an `async (n) =>` arrow, an `async x =>` bare arrow, and
    // `await` (incl. `await 5` identity). NO `import`/`task`/`match`/`class`, so
    // it compiles + runs end-to-end on the VM today. The async arrows correctly
    // pick up `is_async` in `compile_fn_proto` (the parser bumps the `async`
    // token INSIDE the `ArrowExpr` node, so it is a direct token child), so they
    // eager-spawn a `Value::Future` exactly like `async fn`.
    //
    // DEFERRED to the V12 gate: the structured-concurrency examples
    // (`concurrency.as`, `structured_concurrency.as`) use `import * as task` /
    // `time`, and the VM compiler does NOT support `import` until V12. The
    // underlying machinery is already sound — the VM produces ordinary
    // `Value::Future`s (cancel-on-drop `SharedFuture`s) that the native
    // `task.gather`/`race`/`timeout`/`spawn` ops await/select over (de-risked by
    // the `task_gather_awaits_vm_produced_futures_in_order` /
    // `task_race_resolves_a_vm_produced_future` unit tests in `src/vm/run.rs`).
    // Those examples are added to this allowlist when `import` lands (V12).
    "examples/async.as",
];

#[tokio::test]
async fn vm_run_allowlisted_examples_match_treewalker() {
    // For each allow-listed example, the VM's stdout+exit must be byte-identical
    // to the tree-walker's over the SAME file contents. This is the growing-corpus
    // gate; it is currently small because the VM lacks `fn`/control-flow (V3/V4),
    // so the sync multi-feature snippets above carry the bulk of the V2 gate.
    let root = env!("CARGO_MANIFEST_DIR");
    for rel in SYNC_EXAMPLE_ALLOWLIST {
        let path = std::path::Path::new(root).join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read allow-listed example {rel}: {e}"));
        let tw = ascript::run_source_exit(&src)
            .await
            .unwrap_or_else(|e| panic!("tree-walker failed on {rel}: {e:?}"));
        let vm = ascript::vm_run_source(&src)
            .await
            .unwrap_or_else(|e| panic!("VM failed on {rel}: {e:?}"));
        assert_eq!(
            tw, vm,
            "VM diverged from tree-walker for allow-listed example `{rel}`\n  tree-walker: {tw:?}\n  vm:          {vm:?}"
        );
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
async fn vm_for_range_inclusive_rejected_like_treewalker() {
    // FINDING: the legacy parser (the differential oracle) REJECTS `..=` in a
    // for-range head ("expected RParen, found DotDotEq") — inclusive for-range is
    // unsupported by the tree-walker. The VM must not invent inclusive behavior it
    // lacks: an `..=` for-range is rejected at compile time. Both engines surface
    // an `Err(AsError)` from the public entry points; the messages legitimately
    // differ (a parse error vs a compile-time rejection), so we only assert BOTH
    // reject — never that the VM silently runs it (which would be a bug: a silent
    // exclusive loop). A divergence to "both accept" would be the real bug.
    for src in [
        "for (i in 0..=3) { print(i) }",
        "for (i in 0..=0) { print(i) }",
    ] {
        let tw = ascript::run_source(src).await;
        let vm = ascript::vm_run_source(src).await;
        assert!(
            tw.is_err(),
            "tree-walker should REJECT inclusive for-range `{src}`, got {tw:?}"
        );
        assert!(
            vm.is_err(),
            "VM should REJECT inclusive for-range `{src}` (no silent exclusive run), got {vm:?}"
        );
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
        ("for (x of 5) { print(x) }", "value of type number is not iterable"),
        ("for (x of true) { print(x) }", "value of type bool is not iterable"),
        ("for (x of nil) { print(x) }", "value of type nil is not iterable"),
        // An OBJECT is not iterable in sync for-of.
        ("for (x of {a: 1}) { print(x) }", "value of type object is not iterable"),
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
        ("for (i in 5) { print(i) }", "value of type number is not iterable"),
        ("let n = 5\nfor (i in n) { print(i) }", "value of type number is not iterable"),
        ("for (i in true) { print(i) }", "value of type bool is not iterable"),
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
    assert_vm_run_matches_treewalker(
        "fn a() { return b() + 1 }\nfn b() { return 7 }\nprint(a())",
    )
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
    // flat, so recursion is heap-bounded — 50_000 deep does NOT overflow the native
    // stack (the tree-walker cannot reach this depth). sum(50000) = 1_250_025_000.
    // This is the proof that VM frames live on the heap, not the Rust stack.
    let src = "fn sum(n) { if (n == 0) { return 0 }\n return n + sum(n - 1) }\nprint(sum(50000))";
    let (vm_out, code) = ascript::vm_run_source(src).await.expect("vm ok");
    assert_eq!(code, None);
    assert_eq!(vm_out, "1250025000\n", "deep VM recursion result");
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
    assert_vm_run_matches_treewalker("fn f(a, ...rest) { return a + len(rest) }\nprint(f(1))").await;
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
    let tw = ascript::run_source(src).await.expect_err("tree-walker errors");
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
    // `Value::Future` is dropped at the end of its expression statement). A future
    // *held* in a local until program end (`let f = work()`) is NOT part of this
    // class — it interacts with end-of-program task draining and the two engines
    // legitimately differ there (the tree-walker's end-of-program drain runs the
    // still-held task; the VM does not). That held-future case is out of scope for
    // cancel-on-drop and is therefore not asserted here.
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

#[tokio::test]
async fn vm_class_method_calls_method_via_self() {
    // A method that calls another method through self, threading state.
    let src = "class Counter {\n  n: number\n  fn init(start) { self.n = start }\n  fn bump() { self.n = self.n + 1 }\n  fn twice() { self.bump()\n self.bump()\n return self.n }\n}\nlet c = Counter(0)\nprint(c.twice())\nprint(c.n)";
    assert_vm_run_matches_treewalker(src).await;
}

#[tokio::test]
async fn vm_class_no_init_no_args() {
    // A class with no init, constructed with no args, fields written later via
    // member assignment; a method reads them.
    let src = "class Bag {\n  items: number\n  fn total() { return self.items }\n}\nlet b = Bag()\nb.items = 7\nprint(b.total())";
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
    let src = "class Point {\n  x: number\n  fn init(x) { self.x = x }\n}\nlet p = Point(1)\np.x = true";
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
