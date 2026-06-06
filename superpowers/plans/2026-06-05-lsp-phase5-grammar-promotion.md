# LSP Phase 5 — Tree-sitter Grammar Promotion + Full Query Set Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Here "tests" are **tree-sitter parse/query checks** (`tree-sitter parse`, `tree-sitter query`) plus the existing/extended Rust conformance test — NOT new Rust unit-test modules.

**Goal:** Promote the in-repo tree-sitter grammar to a first-class, publishable `tree-sitter-ascript` artifact (npm + cargo + node bindings) and ship the **complete query set** (`highlights`, `injections`, `locals`, `folds`, `indents`, `textobjects`, `tags`, `brackets`) — every capture grounded in the REAL node names of `grammar.js`. Add a drift guard to the conformance test that (a) re-asserts every `examples/*.as` parses with no ERROR node and (b) loads each `queries/*.scm` and asserts it compiles against the grammar, so a grammar change that breaks a query fails CI.

**Architecture:** Today the grammar lives ONLY at `docs/superpowers/specs/grammar/tree-sitter-ascript/` and contains `grammar.js`, a generated `src/` (`parser.c`, `grammar.json`, `node-types.json`, `tree_sitter/`), and `queries/highlights.scm`. `build.rs` compiles `…/grammar/tree-sitter-ascript/src/parser.c` via the `cc` crate; `tests/treesitter_conformance.rs` links the compiled `tree_sitter_ascript()` and asserts both the grammar and the hand-written parser accept the corpus.

This phase makes the **`docs/.../grammar/tree-sitter-ascript/` directory itself** the publishable artifact in place — adding the npm/cargo/node-binding manifests and the full `queries/` set NEXT TO the existing `grammar.js`/`src/`/`queries/highlights.scm`. **`build.rs`'s `parser.c` path is unchanged and stays valid** (it already points inside this directory). The directory is the single source of truth; Phase 6 (`editors/`) references this same path. (Path decision rationale is in Task 1.)

**Tech Stack:** tree-sitter grammar JS (`grammar.js`), `tree-sitter-cli` (`tree-sitter generate --abi 14`, `tree-sitter parse`, `tree-sitter query`), npm `package.json`, cargo `Cargo.toml` + `bindings/rust/`, `node-gyp` `binding.gyp` + `bindings/node/`, the `cc`-compiled `parser.c` consumed by Rust `build.rs`, and `tests/treesitter_conformance.rs` (the Rust drift guard, links the grammar via the `tree-sitter` crate).

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §5.0 (shared foundation: grammar promotion + full query set) and §6 (Phase 5).
- `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` — the bite-sized TDD task format this plan mirrors.
- **The grammar (READ to ground every capture):** `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (the authoritative node names + field names), `…/queries/highlights.scm` (the existing capture conventions), `…/src/node-types.json` (the generated node-type catalog — use to confirm a node/field name before adding a capture).
- `tests/treesitter_conformance.rs` — the existing conformance test (`example_files()`, `treesitter_parses_all_examples_without_errors`).
- `build.rs` — the `cc` build of `parser.c` (the path this phase must keep valid).
- CLAUDE.md "Tree-sitter grammar" section: after any grammar change, regen `parser.c` with `tree-sitter generate --abi 14`.

**Ground-truth node/field names** (verified against `grammar.js` + `node-types.json` — do NOT invent others; if you need a name not listed, READ `grammar.js`/`node-types.json` and cite the line):

| Concept | Node | Fields / anon tokens |
|---|---|---|
| function | `function_declaration` | `name:`, `parameters:` (`parameter_list`), `return_type:`, `body:` (`block`); anon `async`, `fn`, `*` |
| method | `method_definition` | `name:`, `parameters:`, `return_type:`, `body:`; anon `static_keyword`, `async`, `fn`, `*` |
| class | `class_declaration` | `name:`, `superclass:`, `body:` (`class_body`); anon `class`, `extends` |
| enum | `enum_declaration` / `enum_variant` | `name:`; variant `name:`, `value:` |
| param | `parameter` / `rest_parameter` | `name:`, `type:`, `default:` |
| let/const | `let_declaration` / `const_declaration` | `name:` (`_binding_target`), `type:`, `value:` |
| for | `for_statement` | `binding:`, `kind:` (`of`/`in`), `iterable:`, `body:`; anon `for`, `await` |
| if/while | `if_statement` / `while_statement` | `condition:`, `consequence:`/`body:`, `alternative:` |
| block | `block` | anon `{` … `}` |
| match | `match_expression` / `match_arm` | subject `subject:`; arm `pattern:`, `guard:`, `value:`; anon `match`, `=>`, `if` |
| call/member | `call_expression` / `member_expression` / `optional_member_expression` / `index_expression` | call `function:`, `arguments:` (`arguments`); member `object:`, `property:` |
| arrow | `arrow_function` | `parameters:`, `body:`; anon `=>` |
| import | `import_declaration` / `import_specifier` | `source:` (`string`), `namespace:`; anon `import`, `from`, `as`, `*` |
| export | `export_declaration` | anon `export` |
| literals | `number`, `string`, `template_string`, `boolean`, `nil`, `identifier` | — |
| template subst | `template_substitution` | anon `${`, `}` (interpolation) |
| object/map | `object_literal` / `object_entry` / `map_literal` / `map_entry` | entry `key:`, `value:` |
| array | `array_literal` | — |
| types | `primitive_type`, `array_type`, `map_type`, `result_type`, `future_type`, `tuple_type`, `optional_type`, `union_type` | — |
| comments | `line_comment`, `block_comment` | — |

> Note on `node.children()` vs `descendants()` and on the `_match_subject`/`_postfix_expression` rules: names beginning with `_` (e.g. `_expression`, `_match_subject`, `_postfix_expression`, `_primary_expression`, `_type`, `_statement`, `_item`) are **hidden/inline rules** — they do NOT appear as nodes in the parsed tree, so queries must NOT reference them. They are listed here only to explain the structure.

---

## File Structure

Created (all under `docs/superpowers/specs/grammar/tree-sitter-ascript/`, the existing grammar dir):

- `package.json` — npm manifest (`tree-sitter` config block, `tree-sitter-cli` devDep, `main`/`bindings`).
- `Cargo.toml` — cargo manifest so the grammar is `cargo add`/`cargo install`-able.
- `bindings/rust/lib.rs` — Rust binding (`LANGUAGE`/`language()` + the query strings).
- `bindings/rust/build.rs` — the cargo crate's own `cc` build of `src/parser.c` (separate from the repo-root `build.rs`).
- `bindings/node/index.js` — node binding entrypoint.
- `bindings/node/binding.cc` — node N-API binding source.
- `binding.gyp` — node-gyp build config.
- `.gitignore` — ignore node build artifacts (`build/`, `node_modules/`, `*.node`).
- `queries/injections.scm` — template `${…}` re-injection (+ commented SQL/HTML/CSS future stubs).
- `queries/locals.scm` — `@local.scope` / `@local.definition.*` / `@local.reference`.
- `queries/folds.scm` — `@fold` over blocks/bodies/literals.
- `queries/indents.scm` — `@indent.begin`/`@indent.end`/`@indent.branch`.
- `queries/textobjects.scm` — `@function.inner/outer`, `@class.inner/outer`, `@parameter.inner/outer`.
- `queries/tags.scm` — `@definition.function` / `@definition.class` / `@definition.method` / `@reference.call`.
- `queries/brackets.scm` — bracket pairs (`@bracket` / matched `@punctuation.bracket`).

Modified:

- `tests/treesitter_conformance.rs` — add the query-compilation drift guard (`queries_compile_against_grammar`) and keep the existing no-ERROR parse assertion.

Unchanged (verify still valid):

- `build.rs` (repo root) — its `parser.c` path is untouched; `cargo build` must still link the grammar.
- `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`, `src/parser.c` — DO NOT edit the grammar in this phase (queries + manifests only). If a regen is needed for a manifest sanity check, it must produce a byte-identical `parser.c` (no grammar rule changed).

---

## Task 1: Decide & record the artifact home; add the npm `package.json`

**Path decision (record this in the commit body):** Keep the grammar **in place** at `docs/superpowers/specs/grammar/tree-sitter-ascript/` and promote *that directory* to the publishable artifact. Rationale: (1) `build.rs` already compiles `…/grammar/tree-sitter-ascript/src/parser.c` — leaving it here means ZERO risk to the Rust build; (2) a single source of truth avoids a copy that can drift from the `parser.c` the Rust crate actually links; (3) Phase 6 `editors/{vscode,zed,nvim}` reference this one path. (A top-level `tree-sitter-ascript/` or `editors/tree-sitter-ascript/` symlink may be added in Phase 6 if a publisher requires a repo-root package dir, but is OUT OF SCOPE here and must not break `build.rs`.)

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/package.json`
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/.gitignore`

- [ ] **Step 1: Write the `.gitignore`**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/.gitignore`:

```gitignore
# Node / node-gyp build artifacts (the committed src/parser.c is the source of truth)
node_modules/
build/
*.node
package-lock.json
# Rust binding build dir
target/
```

- [ ] **Step 2: Write the npm manifest**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/package.json`. The `tree-sitter` array block tells editors (Zed/Neovim/Atom) where the grammar scope + queries live; `main`/`types` point at the node binding; `files` whitelists what npm publishes (the committed generated `src/`, `grammar.js`, `queries/`, `bindings/`).

```json
{
  "name": "tree-sitter-ascript",
  "version": "0.1.0",
  "description": "AScript grammar for tree-sitter",
  "repository": "https://github.com/mahmoud/ascript",
  "license": "MIT",
  "keywords": ["parser", "lexer", "tree-sitter", "ascript"],
  "main": "bindings/node",
  "types": "bindings/node",
  "files": [
    "grammar.js",
    "binding.gyp",
    "bindings/node/*",
    "queries/*",
    "src/**"
  ],
  "dependencies": {
    "node-addon-api": "^7.1.0",
    "node-gyp-build": "^4.8.0"
  },
  "devDependencies": {
    "tree-sitter-cli": "^0.22.0"
  },
  "peerDependencies": {
    "tree-sitter": "^0.21.0"
  },
  "peerDependenciesMeta": {
    "tree-sitter": { "optional": true }
  },
  "scripts": {
    "install": "node-gyp-build",
    "generate": "tree-sitter generate --abi 14",
    "parse": "tree-sitter parse",
    "test": "tree-sitter parse examples/*.as"
  },
  "tree-sitter": [
    {
      "scope": "source.ascript",
      "file-types": ["as"],
      "highlights": "queries/highlights.scm",
      "injections": "queries/injections.scm",
      "locals": "queries/locals.scm",
      "folds": "queries/folds.scm",
      "indents": "queries/indents.scm",
      "tags": "queries/tags.scm"
    }
  ]
}
```

- [ ] **Step 3: Verify the manifest is valid JSON and the grammar still generates**

```bash
python3 -c "import json,sys; json.load(open('docs/superpowers/specs/grammar/tree-sitter-ascript/package.json')); print('package.json OK')"
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && git -C "$(git rev-parse --show-toplevel)" diff --quiet -- docs/superpowers/specs/grammar/tree-sitter-ascript/src/parser.c && echo "parser.c byte-identical (no grammar change)"
```
Expected: `package.json OK`; `tree-sitter generate` succeeds; `parser.c` is byte-identical (this task adds manifests only — the grammar is unchanged). If `parser.c` changed, you accidentally edited `grammar.js` — revert it.

- [ ] **Step 4: Confirm the Rust build still links the grammar**

```bash
cargo build
```
Expected: builds clean — `build.rs` still finds `…/grammar/tree-sitter-ascript/src/parser.c`.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/package.json docs/superpowers/specs/grammar/tree-sitter-ascript/.gitignore
git commit -m "build(grammar): promote tree-sitter-ascript to a publishable npm package (in-place)

Path decision: the grammar stays at docs/superpowers/specs/grammar/tree-sitter-ascript/
(build.rs compiles its src/parser.c); this directory IS the publishable artifact, so
there is a single source of truth and zero risk to the Rust cc build.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add the cargo manifest + Rust binding (so it's `cargo`-installable)

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/Cargo.toml`
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/rust/lib.rs`
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/rust/build.rs`

> **Important:** this `Cargo.toml` defines a SEPARATE crate that is NOT a member of the repo workspace (the repo `Cargo.toml` does not list it). To avoid cargo treating it as an implicit workspace member, it declares an empty `[workspace]` table (standard tree-sitter-grammar pattern). The repo-root `build.rs` is unaffected — it never reads this `Cargo.toml`.

- [ ] **Step 1: Write `Cargo.toml`**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/Cargo.toml`:

```toml
[package]
name = "tree-sitter-ascript"
description = "AScript grammar for tree-sitter"
version = "0.1.0"
authors = ["AScript"]
license = "MIT"
readme = "README.md"
keywords = ["incremental", "parsing", "tree-sitter", "ascript"]
categories = ["parser-implementations", "text-editors"]
repository = "https://github.com/mahmoud/ascript"
edition = "2021"
autoexamples = false

build = "bindings/rust/build.rs"
include = ["bindings/rust/*", "grammar.js", "queries/*", "src/*"]

# Standalone crate: NOT a member of the ascript repo workspace.
[workspace]

[lib]
path = "bindings/rust/lib.rs"

[dependencies]
tree-sitter-language = "0.1"

[build-dependencies]
cc = "1.0"
```

- [ ] **Step 2: Write the Rust binding `lib.rs`**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/rust/lib.rs`. It exposes the `LANGUAGE` constant (the modern `tree-sitter-language` convention) plus the query strings embedded from `queries/` so downstream Rust consumers (and a future LSP semantic-token bridge) can load them without filesystem access.

```rust
//! This crate provides AScript language support for the [tree-sitter] parsing library.
//!
//! [tree-sitter]: https://tree-sitter.github.io/

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_ascript() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for this grammar.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_ascript) };

/// The syntax-highlighting query for this grammar.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
/// The injection query (template `${…}` re-injection).
pub const INJECTIONS_QUERY: &str = include_str!("../../queries/injections.scm");
/// The local-variable / scope query.
pub const LOCALS_QUERY: &str = include_str!("../../queries/locals.scm");
/// The code-folding query.
pub const FOLDS_QUERY: &str = include_str!("../../queries/folds.scm");
/// The indentation query.
pub const INDENTS_QUERY: &str = include_str!("../../queries/indents.scm");
/// The text-objects query.
pub const TEXTOBJECTS_QUERY: &str = include_str!("../../queries/textobjects.scm");
/// The symbol-tagging query (for nav / ctags-style indexing).
pub const TAGS_QUERY: &str = include_str!("../../queries/tags.scm");
/// The bracket-matching query.
pub const BRACKETS_QUERY: &str = include_str!("../../queries/brackets.scm");

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading AScript parser");
    }
}
```

- [ ] **Step 3: Write the binding `build.rs`**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/rust/build.rs`:

```rust
fn main() {
    let src_dir = std::path::Path::new("src");
    let mut c_config = cc::Build::new();
    c_config.std("c11").include(src_dir);
    let parser_path = src_dir.join("parser.c");
    c_config.file(&parser_path);
    println!("cargo:rerun-if-changed={}", parser_path.to_str().unwrap());
    c_config.compile("tree-sitter-ascript");
}
```

- [ ] **Step 4: Verify the cargo crate builds standalone**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && cargo build
```
Expected: the standalone crate compiles + links `src/parser.c`. (If `tree-sitter-language`/`tree-sitter` are not cached offline, this step may need network; if unavailable in the sandbox, note it and proceed — Step 5 still guards the repo build.)

- [ ] **Step 5: Confirm the repo-root build is unaffected**

```bash
cargo build && cargo test --test treesitter_conformance
```
Expected: repo build clean; existing conformance test still passes (the new `Cargo.toml` does not pull the grammar dir into the repo workspace — verify `cargo build` from the repo root does NOT try to build `tree-sitter-ascript`).

- [ ] **Step 6: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/Cargo.toml docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/rust
git commit -m "build(grammar): cargo-installable tree-sitter-ascript crate + Rust binding

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Add the node bindings (`binding.gyp` + N-API)

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/binding.gyp`
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/node/binding.cc`
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/node/index.js`

- [ ] **Step 1: Write `binding.gyp`**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/binding.gyp`:

```python
{
  "targets": [
    {
      "target_name": "tree_sitter_ascript_binding",
      "dependencies": ["<!(node -p \"require('node-addon-api').gyp\")"],
      "include_dirs": ["src"],
      "sources": [
        "bindings/node/binding.cc",
        "src/parser.c"
      ],
      "conditions": [
        ["OS!='win'", { "cflags_c": ["-std=c11"] }]
      ]
    }
  ]
}
```

- [ ] **Step 2: Write the N-API source**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/node/binding.cc`:

```cpp
#include <napi.h>

typedef struct TSLanguage TSLanguage;

extern "C" TSLanguage *tree_sitter_ascript();

// "tree-sitter", "language" hashed with BLAKE2.
namespace {

napi_value language(napi_env env, napi_callback_info info) {
  napi_value result;
  napi_create_external(env, tree_sitter_ascript(), nullptr, nullptr, &result);
  return result;
}

napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor descriptor = {
      "language", nullptr, nullptr, nullptr, nullptr, language,
      napi_default, nullptr};
  napi_define_properties(env, exports, 1, &descriptor);
  return exports;
}

} // namespace

NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)
```

- [ ] **Step 3: Write the node entrypoint**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/node/index.js`:

```js
const root = require("path").join(__dirname, "..", "..");

const binding = require("node-gyp-build")(root);

try {
  module.exports = binding.language;
} catch (_) {}

try {
  module.exports.nodeTypeInfo = require("../../src/node-types.json");
} catch (_) {}
```

- [ ] **Step 4: Verify `binding.gyp` parses + the C source compiles via the existing repo build**

`binding.gyp` is Python-literal syntax; validate it parses and confirm the C `parser.c` it lists still compiles in the repo (the repo `build.rs` already compiles `src/parser.c`, so a clean `cargo build` confirms the C source is well-formed):

```bash
python3 -c "import ast; ast.literal_eval(open('docs/superpowers/specs/grammar/tree-sitter-ascript/binding.gyp').read()); print('binding.gyp OK')"
cargo build
```
Expected: `binding.gyp OK`; `cargo build` clean. (A full `npm install`/`node-gyp rebuild` needs network + a node toolchain; it is a Phase-6 CI concern. If a node toolchain is present and offline-capable, optionally run `cd docs/superpowers/specs/grammar/tree-sitter-ascript && npm install --ignore-scripts && npx node-gyp rebuild` and note the result.)

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/binding.gyp docs/superpowers/specs/grammar/tree-sitter-ascript/bindings/node
git commit -m "build(grammar): node N-API bindings (binding.gyp + node-gyp-build)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `injections.scm` — re-inject template `${…}` as AScript

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/injections.scm`

The only ground-truth injection target today is the template-string substitution. `template_substitution` is `${ <expr> }` (grammar.js:689); its inner expression is already AScript, so the injection re-parses the substitution content as `ascript`. SQL/HTML/CSS-in-template-string injections are FUTURE (no such stdlib surface exists yet) — left as commented stubs per §5.0.

- [ ] **Step 1: Write the injection query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/injections.scm`:

```scheme
; Template `${ … }` substitutions are AScript expressions — re-inject them so the
; full highlight/locals machinery applies inside interpolations (including nested
; templates).
((template_substitution) @injection.content
  (#set! injection.language "ascript")
  (#set! injection.include-children))

; Comments carry no sub-language by default; a `// tree-sitter: <lang>` style
; directive could be added here later.

; ----- Future stubs (no stdlib surface yet) ---------------------------------
; When tagged-template / SQL / HTML / CSS string DSLs land, register them here by
; matching the tag identifier of the call and injecting into the template string,
; e.g.:
;   ((call_expression
;      function: (identifier) @_tag (#eq? @_tag "sql")
;      arguments: (arguments (template_string) @injection.content))
;     (#set! injection.language "sql"))
; (Left commented: `sql`/`html`/`css` tagged templates are not in the language yet.)
```

- [ ] **Step 2: Verify the query compiles + captures on a real file**

Pick a corpus file containing a template literal. `examples/hello.as` or `examples/strings.as` use `${…}`; confirm with grep, then run the query:

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
grep -rl '\${' examples 2>/dev/null | head -1   # if examples are not symlinked here, use the repo path below
# From the repo root the corpus lives at ./examples — run the query against one:
tree-sitter query queries/injections.scm "$(git rev-parse --show-toplevel)/examples/all_features.as"
```
Expected: the query COMPILES (no "Query error: Invalid node type / Invalid capture") and prints one or more `injection.content` captures over `template_substitution` ranges. If `all_features.as` has no `${…}`, pick any file from `grep -rl '\${' "$(git rev-parse --show-toplevel)/examples"`. A clean compile with zero matches is still a PASS for the drift guard (the node type resolved).

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/injections.scm
git commit -m "feat(grammar): injections.scm — re-inject template \${…} as AScript

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: `locals.scm` — scopes, definitions, references

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/locals.scm`

Ground every capture in real nodes/fields: scopes are `function_declaration`/`method_definition`/`arrow_function`/`block`/`for_statement`/`match_arm`; definitions are the `name:`/`binding:` identifier of declarations/params; references are bare `identifier` uses. The `_binding_target` of a `let`/`const` can be an `identifier`, `array_pattern`, or `object_pattern` — capture the simple `identifier` form (pattern bindings are handled by the LSP resolver, not the local-only query).

- [ ] **Step 1: Write the locals query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/locals.scm`:

```scheme
; ----- Scopes ----------------------------------------------------------------
[
  (function_declaration)
  (method_definition)
  (arrow_function)
  (block)
  (for_statement)
  (while_statement)
  (if_statement)
  (match_arm)
] @local.scope

; ----- Definitions -----------------------------------------------------------
; let / const bindings (simple identifier target).
(let_declaration name: (identifier) @local.definition.var)
(const_declaration name: (identifier) @local.definition.var)

; Function / method / class / enum names.
(function_declaration name: (identifier) @local.definition.function)
(method_definition name: (identifier) @local.definition.method)
(class_declaration name: (identifier) @local.definition.type)
(enum_declaration name: (identifier) @local.definition.type)
(enum_variant name: (identifier) @local.definition.constant)

; Parameters (normal + rest) and the for-loop binding.
(parameter name: (identifier) @local.definition.parameter)
(rest_parameter name: (identifier) @local.definition.parameter)
(for_statement binding: (identifier) @local.definition.var)

; Import bindings.
(import_specifier (identifier) @local.definition.import)
(import_declaration namespace: (identifier) @local.definition.import)

; Destructuring rest collector.
(rest_element name: (identifier) @local.definition.var)

; ----- References ------------------------------------------------------------
; Any bare identifier use is a reference; the local resolver pairs it to the
; nearest in-scope definition above.
(identifier) @local.reference
```

- [ ] **Step 2: Verify the query compiles + captures**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/locals.scm "$(git rev-parse --show-toplevel)/examples/factorial.as"
```
Expected: COMPILES; prints `local.scope` over the `fn`/`block`, `local.definition.function` on `factorial`, `local.definition.parameter` on the param, and `local.reference` on identifier uses. (`examples/factorial.as` is a small recursive function — ideal coverage.) A "Query error: Invalid node type 'X'" means a wrong node name — re-check against `grammar.js`.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/locals.scm
git commit -m "feat(grammar): locals.scm — scopes/definitions/references

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: `folds.scm` — foldable regions

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/folds.scm`

Fold the multi-line container nodes: `block`, `class_body`, the `enum_declaration` body, object/map/array literals, the `match_expression`, `parameter_list`/`arguments`, and block/line-comment runs. The whole node is captured `@fold`; the editor folds from the node's first line to its last.

- [ ] **Step 1: Write the folds query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/folds.scm`:

```scheme
; Bodies and blocks.
(block) @fold
(class_body) @fold
(enum_declaration) @fold

; Containers / literals.
(object_literal) @fold
(map_literal) @fold
(array_literal) @fold
(parameter_list) @fold
(arguments) @fold

; Control-flow expression bodies.
(match_expression) @fold

; Multi-line comments.
(block_comment) @fold
```

- [ ] **Step 2: Verify the query compiles + captures**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/folds.scm "$(git rev-parse --show-toplevel)/examples/classes.as"
```
Expected: COMPILES; prints `fold` captures over the class body and method blocks. (If `examples/classes.as` does not exist, substitute any file with a class/block — e.g. `examples/all_features.as`.) Confirm with `ls examples/*.as` first.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/folds.scm
git commit -m "feat(grammar): folds.scm — foldable blocks/bodies/literals

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: `indents.scm` — indentation rules

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/indents.scm`

Use the nvim-treesitter indent capture convention (`@indent.begin` on the opener node, `@indent.end`/`@indent.branch` on the closing bracket token). Brace/bracket/paren openers begin an indent; the matching close token ends it. `else` is an indent branch. Reference the anon `{`/`}`/`[`/`]`/`(`/`)` tokens that exist inside the real nodes.

- [ ] **Step 1: Write the indents query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/indents.scm`:

```scheme
; Nodes whose body should be indented one level.
[
  (block)
  (class_body)
  (object_literal)
  (map_literal)
  (array_literal)
  (parameter_list)
  (arguments)
  (match_expression)
  (enum_declaration)
] @indent.begin

; The closing bracket of each of those dedents back.
[
  "}"
  "]"
  ")"
] @indent.end

; `else` continues the if-construct at the same level (branch, not a new indent).
(if_statement
  "else" @indent.branch)

; Keep a line that is only a closing bracket aligned with its opener.
[
  "}"
  "]"
  ")"
] @indent.align
```

- [ ] **Step 2: Verify the query compiles**

Indent queries match anon tokens (`"}"` etc.), which `tree-sitter query` reports as captures too:

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/indents.scm "$(git rev-parse --show-toplevel)/examples/factorial.as"
```
Expected: COMPILES; prints `indent.begin` over `block`/`parameter_list` and `indent.end`/`indent.align` over the `}` / `)` tokens. A "Query error" on `"else"` means the `if_statement` does not expose that anon token under that node — re-check grammar.js:279 (it does: `if_statement` has the literal `"else"`).

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/indents.scm
git commit -m "feat(grammar): indents.scm — bracket-driven indentation rules

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: `textobjects.scm` — function/class/parameter inner & outer

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/textobjects.scm`

Standard nvim-treesitter textobject captures: `@function.outer` = the whole declaration, `@function.inner` = its `body:` block; `@class.outer` = the class decl, `@class.inner` = its `body:` (`class_body`); `@parameter.outer` = a `parameter`, `@parameter.inner` = the parameter's `name:` identifier. Ground every field name in grammar.js (`function_declaration body:` line 209, `class_declaration body:` 237, `parameter name:` 221).

- [ ] **Step 1: Write the textobjects query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/textobjects.scm`:

```scheme
; ----- Functions (declarations, methods, arrows) -----------------------------
(function_declaration body: (block) @function.inner) @function.outer
(method_definition body: (block) @function.inner) @function.outer
(arrow_function body: (block) @function.inner) @function.outer
(arrow_function body: (_) @function.inner) @function.outer

; ----- Classes ---------------------------------------------------------------
(class_declaration body: (class_body) @class.inner) @class.outer

; ----- Enums (treated as a class-like type body for navigation) --------------
(enum_declaration) @class.outer

; ----- Parameters ------------------------------------------------------------
(parameter name: (identifier) @parameter.inner) @parameter.outer
(rest_parameter name: (identifier) @parameter.inner) @parameter.outer

; ----- Call arguments (as "parameter"-class textobjects at call sites) --------
(arguments (_) @parameter.inner)

; ----- Comments --------------------------------------------------------------
(line_comment) @comment.outer
(block_comment) @comment.outer
```

- [ ] **Step 2: Verify the query compiles + captures**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/textobjects.scm "$(git rev-parse --show-toplevel)/examples/factorial.as"
```
Expected: COMPILES; prints `function.outer` over the `function_declaration` and `function.inner` over its `block`, plus `parameter.outer`/`parameter.inner`. A "Query error: Invalid field name 'body'" would mean a wrong field — but `body:` is real on all three (grammar.js 209/259/501).

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/textobjects.scm
git commit -m "feat(grammar): textobjects.scm — function/class/parameter inner+outer

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: `tags.scm` — symbol tags for nav / ctags

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/tags.scm`

The tree-sitter `tags` convention pairs a `@definition.<kind>` (the name identifier) with a `@name` capture and the enclosing definition node, plus `@reference.call` on call targets. Grounded in real fields: `function_declaration name:`, `method_definition name:`, `class_declaration name:`, `enum_declaration name:`, and `call_expression function:`.

- [ ] **Step 1: Write the tags query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/tags.scm`:

```scheme
; ----- Definitions -----------------------------------------------------------
(function_declaration
  name: (identifier) @name) @definition.function

(method_definition
  name: (identifier) @name) @definition.method

(class_declaration
  name: (identifier) @name) @definition.class

(enum_declaration
  name: (identifier) @name) @definition.enum

(enum_variant
  name: (identifier) @name) @definition.constant

; ----- References ------------------------------------------------------------
; Direct call:  foo(...)
(call_expression
  function: (identifier) @name) @reference.call

; Method / namespaced call:  obj.foo(...)  /  math.abs(...)
(call_expression
  function: (member_expression
    property: (identifier) @name)) @reference.call

; Class instantiation / type mention:  C(...)  (Capitalized callee)
(call_expression
  function: (identifier) @name (#match? @name "^[A-Z]")) @reference.class
```

- [ ] **Step 2: Verify the query compiles + captures**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/tags.scm "$(git rev-parse --show-toplevel)/examples/factorial.as"
```
Expected: COMPILES; prints `definition.function` + `name` on `factorial`, and `reference.call` on the recursive `factorial(...)` call. The `#match?` predicate on `@reference.class` must not error (it's a standard predicate).

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/tags.scm
git commit -m "feat(grammar): tags.scm — definition/reference symbol tags

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: `brackets.scm` — bracket pairs

**Files:**
- Create: `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/brackets.scm`

Capture the literal bracket tokens so editors can match/rainbow them. AScript's bracket tokens (from `highlights.scm` line 39 + grammar.js): `(` `)` `[` `]` `{` `}`, plus the template-substitution delimiters `${` and `}`.

- [ ] **Step 1: Write the brackets query**

Create `docs/superpowers/specs/grammar/tree-sitter-ascript/queries/brackets.scm`:

```scheme
; All structural brackets — matched as pairs by the editor's bracket engine.
[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

; Template interpolation delimiters `${ … }` are a bracket pair too.
(template_substitution
  "${" @punctuation.special
  "}" @punctuation.special)
```

- [ ] **Step 2: Verify the query compiles + captures**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter query queries/brackets.scm "$(git rev-parse --show-toplevel)/examples/factorial.as"
```
Expected: COMPILES; prints `punctuation.bracket` over the `(`/`)`/`{`/`}` tokens. The `${`/`}` arm mirrors the proven `highlights.scm` pattern (lines 13–15), so it must compile.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries/brackets.scm
git commit -m "feat(grammar): brackets.scm — structural + template bracket pairs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Drift guard — extend `tests/treesitter_conformance.rs` to compile every query

**Files:**
- Modify: `tests/treesitter_conformance.rs`

Add a test that (a) re-asserts the corpus parses with no ERROR (the existing `treesitter_parses_all_examples_without_errors` already does this — keep it) and (b) loads every `queries/*.scm` and constructs a `tree_sitter::Query` against the grammar — a `Query::new` failure (unknown node type, unknown field, bad capture, bad predicate) is a compile error, so a grammar change that breaks a query fails CI.

- [ ] **Step 1: Write the failing drift-guard test**

Append to `tests/treesitter_conformance.rs`:

```rust
/// Drift guard: every `queries/*.scm` must compile against the grammar. A grammar
/// change that renames/removes a node or field (without updating the queries) makes
/// `Query::new` fail here — so query drift breaks CI, not an editor at runtime.
#[test]
fn queries_compile_against_grammar() {
    let lang = language();
    let query_dir = std::path::Path::new(
        "docs/superpowers/specs/grammar/tree-sitter-ascript/queries",
    );

    // The full query set this phase ships. Each MUST exist and compile.
    let expected = [
        "highlights.scm",
        "injections.scm",
        "locals.scm",
        "folds.scm",
        "indents.scm",
        "textobjects.scm",
        "tags.scm",
        "brackets.scm",
    ];

    let mut missing = Vec::new();
    let mut failed = Vec::new();
    for name in expected {
        let path = query_dir.join(name);
        let Ok(src) = fs::read_to_string(&path) else {
            missing.push(name);
            continue;
        };
        if let Err(e) = tree_sitter::Query::new(&lang, &src) {
            failed.push(format!("{name}: {e:?}"));
        }
    }

    assert!(missing.is_empty(), "missing query files: {missing:?}");
    assert!(
        failed.is_empty(),
        "queries failed to compile against the grammar: {failed:?}"
    );
}
```

- [ ] **Step 2: Run the drift guard**

```bash
cargo test --test treesitter_conformance queries_compile_against_grammar
```
Expected: PASS — all 8 query files exist and compile. If a query fails, the error names the offending capture/node — fix that query (re-check the node/field against `grammar.js`/`node-types.json`), NOT the test. If `Query::new`'s signature differs in the pinned `tree-sitter` crate version (older crates take `lang` by value), adapt the call (`Query::new(lang, &src)` / `&lang`) — confirm against the existing `parser.set_language(&lang)` usage at the top of the file (it takes `&lang`, so the crate is new enough for `Query::new(&lang, ..)`).

- [ ] **Step 3: Run the full conformance suite + the corpus no-ERROR assertion**

```bash
cargo test --test treesitter_conformance
```
Expected: PASS — `treesitter_parses_all_examples_without_errors`, the existing guard tests, AND the new `queries_compile_against_grammar` all green.

- [ ] **Step 4: Commit**

```bash
git add tests/treesitter_conformance.rs
git commit -m "test(grammar): drift guard — every queries/*.scm must compile against the grammar

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: Whole-corpus query sweep + final gate

Run every query over every example to catch any node/field that compiles but never matches due to a typo'd predicate, and confirm the Rust build + both clippy configs stay clean.

**Files:** none (verification only).

- [ ] **Step 1: Parse the whole corpus with no ERROR (grammar still valid)**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
tree-sitter parse "$(git rev-parse --show-toplevel)"/examples/*.as --quiet
```
Expected: exit 0, no `(ERROR …)` reported. (`tree-sitter parse --quiet` prints only failures.)

- [ ] **Step 2: Run every query over a representative spread of corpus files**

```bash
root="$(git rev-parse --show-toplevel)"
cd "$root/docs/superpowers/specs/grammar/tree-sitter-ascript"
for q in queries/*.scm; do
  echo "=== $q ==="
  tree-sitter query "$q" "$root"/examples/all_features.as >/dev/null && echo "  OK"
done
```
Expected: each query prints `OK` (compiles + runs). `examples/all_features.as` exercises the broadest surface; a "Query error" here is a real capture bug — fix the query.

- [ ] **Step 3: Confirm the Rust build + conformance + clippy**

```bash
cargo build
cargo test --test treesitter_conformance
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all green/clean — `build.rs` still compiles the in-place `parser.c`; the drift guard passes; clippy clean in both feature configs (this phase touches no `src/` Rust beyond the test file).

- [ ] **Step 4: Confirm `parser.c` is byte-identical to `main` (no accidental grammar edit)**

```bash
git diff --stat main -- docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js docs/superpowers/specs/grammar/tree-sitter-ascript/src/parser.c
```
Expected: EMPTY — this phase ships manifests + queries + a test only; `grammar.js`/`src/parser.c` are untouched.

- [ ] **Step 5: Commit (if any sweep fix was needed; otherwise skip)**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/queries
git commit -m "fix(grammar): query sweep corrections from whole-corpus run

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 5 Done — Gate

- [ ] The grammar dir at `docs/superpowers/specs/grammar/tree-sitter-ascript/` is a publishable artifact: `package.json` (npm), `Cargo.toml` + `bindings/rust/` (cargo), and `binding.gyp` + `bindings/node/` (node) all present and valid.
- [ ] The full query set exists — `highlights`, `injections`, `locals`, `folds`, `indents`, `textobjects`, `tags`, `brackets` — every capture grounded in real `grammar.js` node/field names.
- [ ] `tree-sitter generate --abi 14` succeeds and leaves `src/parser.c` byte-identical (no grammar rule changed this phase).
- [ ] `tree-sitter parse examples/*.as --quiet` reports no ERROR nodes; each `tree-sitter query queries/<file>.scm` compiles + runs over the corpus.
- [ ] `cargo test --test treesitter_conformance` passes — including the new `queries_compile_against_grammar` drift guard (a grammar change that breaks any query fails CI).
- [ ] `cargo build` is clean (repo `build.rs` still finds `parser.c`); `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` are clean.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase6-editor-extensions.md` (VS Code, Zed, Neovim extensions + binary distribution; consumes this promoted grammar + shared `queries/`).
