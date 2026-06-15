//! SRV Task 9 — Negative-space verification.
//!
//! The Server Tier / shared read-only heap (SRV) is **purely runtime + stdlib**:
//! `shared.freeze` is an ordinary `std/shared` call, `Value::shared` is a runtime
//! value variant, and `server.serve({ workers })` is a stdlib call — NONE of them
//! touch the grammar, the two parsers, the formatter, the tree-sitter grammar, or
//! the `.aso` bytecode format. The `worker`/`fn` keywords SRV reuses already
//! existed before SRV.
//!
//! Per the plan (Task 9) this is asserted as a CI tripwire so a future change can't
//! silently regress the "Touching syntax is genuinely N/A" property:
//!
//!  - `ASO_FORMAT_VERSION` is pinned (the `TAG_SHARED` wire tag is worker-wire only,
//!    NOT an `.aso` constant — freeze is a runtime call, not a bytecode constant).
//!  - The tree-sitter grammar (`grammar.js`) has NO `shared`/`freeze`/`serve`/
//!    `Shared` rule — SRV added no surface form.
//!  - The generated `parser.c` ABI version is unchanged (no regen).
//!  - A program that builds a `Value::shared` compiles to `.aso` and round-trips —
//!    proving freeze is a runtime object, not a serialized bytecode constant.

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// SRV must NOT bump the `.aso` format version. The shared heap is a runtime value
/// (a frozen `Arc` graph built by a runtime call), and the `TAG_SHARED` serializer
/// tag is the worker-wire airlock format, NOT the `.aso` constant pool — so the
/// bytecode format is untouched. This literal pin trips on ANY bump, forcing each
/// bumping feature to consciously update it and confirm the bump is not from SRV.
#[test]
fn aso_format_version_is_unchanged_by_srv() {
    // Literal pin (trips on ANY bump). SRV itself bumped nothing; the value has since
    // been bumped to 28 by DEFER (the DeferPush / DeferPushMethod opcodes, ASO 27→28).
    // The invariant this guards is that SRV is NOT the cause of any bump. When a later
    // feature legitimately bumps the version, update this literal in the same commit
    // after confirming the bump is NOT attributable to SRV.
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        28,
        "ASO_FORMAT_VERSION changed — confirm the bump is NOT attributable to SRV \
         (the shared heap is a runtime value, not a bytecode constant; TAG_SHARED is a \
         worker-wire tag, not an .aso constant), then update this literal pin."
    );
}

/// SRV added no surface syntax — `shared`/`freeze`/`isShared`/`serve` are ordinary
/// identifiers/calls, never grammar rules. The tree-sitter grammar must contain no
/// rule or named node for them (the `worker_keyword`/`function_declaration` rules
/// SRV reuses predate SRV).
#[test]
fn tree_sitter_grammar_has_no_srv_surface_form() {
    let grammar = std::fs::read_to_string(repo_root().join("tree-sitter-ascript/grammar.js"))
        .expect("read grammar.js");
    // A grammar RULE is `name: $ =>` or a `$.name` reference / `field('name', ...)`.
    // SRV introduced none of these for its runtime surface. We assert the absence of
    // any grammar rule named after an SRV concept. (Plain prose in comments is fine —
    // these tokens never appear as a rule.)
    for forbidden in [
        "shared_freeze",
        "freeze_expression",
        "shared_expression",
        "serve_expression",
        "$.shared",
        "$.freeze",
        "$.serve",
        "SharedNode",
    ] {
        assert!(
            !grammar.contains(forbidden),
            "tree-sitter grammar.js mentions `{forbidden}` — SRV must add NO grammar \
             rule; `shared.freeze`/`serve` are runtime stdlib calls parsed as ordinary \
             member-call expressions, not surface syntax."
        );
    }
}

/// The generated `parser.c` ABI version is unchanged — SRV ran no
/// `tree-sitter generate`, so the vendored parser is byte-for-byte the pre-SRV
/// grammar. (A regen would bump/rewrite this file; this pins the ABI.)
#[test]
fn tree_sitter_parser_abi_is_unchanged() {
    let parser_c = std::fs::read_to_string(repo_root().join("tree-sitter-ascript/src/parser.c"))
        .expect("read parser.c");
    assert!(
        parser_c.contains("#define LANGUAGE_VERSION 14"),
        "parser.c LANGUAGE_VERSION changed — SRV must not regenerate the tree-sitter \
         parser (it adds no grammar)."
    );
}

/// The Rust front-ends + formatter contain no SRV surface form either. `shared`
/// and `freeze` are NOT tokens, keywords, AST nodes, or formatter arms — they are
/// ordinary identifiers handled by the existing call/member machinery. We assert
/// the lexer/token/ast/fmt/legacy-parser sources never grew a `shared`/`freeze`
/// keyword or AST variant.
#[test]
fn rust_frontends_and_formatter_have_no_srv_keyword_or_node() {
    // These files define the surface: a new keyword would appear in the lexer/token,
    // a new node in ast.rs, a new formatter arm in fmt.rs, a new parse rule in
    // parser.rs / syntax. None of them should name an SRV surface form.
    let surface_files = [
        "src/lexer.rs",
        "src/token.rs",
        "src/ast.rs",
        "src/fmt.rs",
        "src/parser.rs",
    ];
    // A KEYWORD or AST-node token for SRV would be one of these. (We deliberately do
    // NOT forbid the substring "shared"/"freeze" wholesale — `object.freeze` is a
    // pre-existing stdlib concept and unrelated identifiers may legitimately appear.
    // We forbid the specific SRV *surface* tokens that would only exist if SRV had
    // added syntax.)
    for rel in surface_files {
        let text = std::fs::read_to_string(repo_root().join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for forbidden in ["SharedNode", "Tok::Shared", "Kw::Shared", "ExprKind::Freeze"] {
            assert!(
                !text.contains(forbidden),
                "{rel} mentions `{forbidden}` — SRV must add NO keyword/token/AST node; \
                 `shared.freeze`/`serve` are runtime stdlib calls."
            );
        }
    }
}

/// End-to-end proof that freeze is a RUNTIME object, not a bytecode constant: a
/// program that constructs a `Value::shared` compiles to `.aso` and runs identically
/// to direct execution. If freeze had leaked into the `.aso` constant pool, this
/// would need a format-version bump (which `aso_format_version_is_unchanged_by_srv`
/// forbids) and/or a serializer arm; instead the `.aso` carries only the *bytecode*
/// that CALLS `shared.freeze` at runtime.
#[cfg(feature = "shared")]
#[tokio::test]
async fn shared_program_compiles_to_aso_without_format_change() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join("ascript_srv_negspace_aso");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("frozen.as");
    std::fs::write(
        &prog,
        "import * as shared from \"std/shared\"\n\
         let cfg = shared.freeze({ region: \"us\", limits: [10, 100] })\n\
         print(cfg.region)\n\
         print(cfg.limits[0])\n\
         print(shared.isShared(cfg))\n",
    )
    .unwrap();
    let aso = dir.join("frozen.aso");
    // build → .aso (this is where a format-version mismatch would surface).
    let build = Command::new(bin)
        .args(["build", prog.to_str().unwrap(), "-o", aso.to_str().unwrap()])
        .output()
        .expect("run build");
    assert!(
        build.status.success(),
        "ascript build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    // run the .aso — proves freeze executes at runtime from plain bytecode.
    let run = Command::new(bin)
        .args(["run", aso.to_str().unwrap()])
        .output()
        .expect("run .aso");
    assert!(
        run.status.success(),
        "ascript run .aso failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8_lossy(&run.stdout);
    assert_eq!(out, "us\n10\ntrue\n", "frozen-value .aso round-trip");
    std::fs::remove_dir_all(&dir).ok();
}
