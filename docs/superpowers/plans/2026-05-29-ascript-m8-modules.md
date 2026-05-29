# AScript Milestone 8 — Modules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §9 ESM-style modules: `export` declarations, named imports `import { a, b } from "./m"`, namespace imports `import * as ns from "./m"`, file = module, paths resolved relative to the importing file, once-only evaluation with caching, and circular-import partial-init resolution.

**Architecture:** `run_source` becomes a module loader. Each module loads into its own top-level scope (a fresh `global_env()` child, so builtins are available, module-level bindings are isolated). A path-keyed cache (`HashMap<PathBuf, ModuleEntry>` on `Interp`) makes evaluation once-only; the entry is inserted **before** the body runs, so circular imports resolve to the partially-initialized module. Exports are tracked in a shared `Rc<RefCell<HashSet<String>>>` per module. Named import binds selected exports into the importer's scope; namespace import binds an object of all exports. No new `Value` kind.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap, std::fs/std::path. No new crates.

**Starting state (end of M7, on `main`):** Full language through classes/enums/match. `Interp { output: String }`; `run_source(src) -> Result<String, AsError>` lexes/parses/execs a single string. CLI `ascript run <file>` reads the file then calls `run_source`. 120 lib + 8 integration tests.

**Conventions:** spans char offsets; single-threaded `Rc`/`RefCell`; `?Send` async recursion; `Control` error channel.

## Spec §9 semantics decided

- **`export <decl>`:** `export fn f(){}`, `export const X = …`, `export let y = …`, `export class C {}`, `export enum E {}`. Runs the decl (defines it in the module scope) AND marks its name exported.
- **Named import:** `import { a, b } from "./util"` binds `a`, `b` from the module's exports into the importer's scope. Importing a non-exported name → panic "module … has no export 'a'".
- **Namespace import:** `import * as util from "./util"` binds `util` = an object whose keys are the module's exports.
- **No default exports.**
- **Resolution:** the source string is resolved relative to the importing module's directory; a `.as` extension is appended if absent. `std/*` paths are reserved for built-in modules (none yet → "cannot read module"; stdlib milestones add them).
- **Once-only:** a module's body runs exactly once; subsequent imports reuse the cached module (so module-level side effects happen once).
- **Circular:** the module entry is cached before its body runs, so a cycle resolves to the partially-initialized module; a name not yet exported at the point of import → "no export" panic (the spec's load-order error).

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| `std/*` built-in module resolution | No stdlib modules exist yet | **Phase 2 stdlib milestones** |
| `map<K,V>` type / `Map` value kind | Collection work | **first stdlib-collections milestone** |
| Live-binding semantics (ESM mutable exports) | Snapshot-at-import is simpler and spec-adequate; full live bindings are an enhancement | future polish |

---

## Task 1: Module loader, `export`, and named + namespace imports

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`, `src/lib.rs`, `src/main.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Import,` and `Export,` before `Eof`.
- [ ] **Step 2: `src/lexer.rs`** — keywords `"import" => Tok::Import, "export" => Tok::Export,`. (`from`/`as` stay soft keywords — they lex as `Ident`.)

- [ ] **Step 3: `src/ast.rs`** — add module statements:
```rust
    Import { names: ImportNames, source: String },
    Export(Box<Stmt>),
```
and
```rust
#[derive(Clone, Debug)]
pub enum ImportNames {
    Named(Vec<String>),
    Namespace(String),
}
```

- [ ] **Step 4: `src/parser.rs`** — dispatch in `statement`: `Tok::Import => self.import_decl()`, `Tok::Export => self.export_decl()`. Add:

```rust
    fn import_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Import)?;
        let names = if *self.peek() == Tok::Star {
            self.advance();
            // `as`
            if !matches!(self.peek(), Tok::Ident(s) if s == "as") {
                return Err(AsError::at("expected 'as' after '*' in import", self.span()));
            }
            self.advance();
            let alias = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected namespace alias, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            crate::ast::ImportNames::Namespace(alias)
        } else {
            self.eat(&Tok::LBrace)?;
            let mut names = Vec::new();
            if *self.peek() != Tok::RBrace {
                loop {
                    match self.advance() {
                        Tok::Ident(n) => names.push(n),
                        other => return Err(AsError::at(format!("expected import name, found {:?}", other), self.tokens[self.pos - 1].span)),
                    }
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBrace { break; }
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RBrace)?;
            crate::ast::ImportNames::Named(names)
        };
        // `from`
        if !matches!(self.peek(), Tok::Ident(s) if s == "from") {
            return Err(AsError::at("expected 'from' in import", self.span()));
        }
        self.advance();
        let source = match self.advance() {
            Tok::Str(s) => s,
            other => return Err(AsError::at(format!("expected module path string, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        Ok(Stmt::Import { names, source })
    }

    fn export_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Export)?;
        // Only declarations are exportable.
        let inner = match self.peek() {
            Tok::Let => self.let_stmt(true)?,
            Tok::Const => self.let_stmt(false)?,
            Tok::Fn => self.fn_decl()?,
            Tok::Class => self.class_decl()?,
            Tok::Enum => self.enum_decl()?,
            other => return Err(AsError::at(format!("only let/const/fn/class/enum can be exported, found {:?}", other), self.span())),
        };
        Ok(Stmt::Export(Box::new(inner)))
    }
```

- [ ] **Step 5: `src/interp.rs`** — add the module loader.

Add imports: `use std::collections::{HashMap, HashSet};`, `use std::path::{Path, PathBuf};`.

Add a module entry and extend `Interp`:
```rust
#[derive(Clone)]
pub struct ModuleEntry {
    pub env: Environment,
    pub exports: std::rc::Rc<std::cell::RefCell<HashSet<String>>>,
}

pub struct Interp {
    pub output: String,
    modules: HashMap<PathBuf, ModuleEntry>,
    module_dir: PathBuf,
    current_exports: std::rc::Rc<std::cell::RefCell<HashSet<String>>>,
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            output: String::new(),
            modules: HashMap::new(),
            module_dir: PathBuf::from("."),
            current_exports: std::rc::Rc::new(std::cell::RefCell::new(HashSet::new())),
        }
    }
    // ... existing methods ...
}
```

Add the loader + import resolution + the exported-name helper:
```rust
    /// Load (or fetch from cache) the module at `path`, returning its entry.
    #[async_recursion(?Send)]
    pub async fn load_module(&mut self, path: &Path) -> Result<ModuleEntry, Control> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(entry) = self.modules.get(&canon) {
            return Ok(entry.clone()); // cached, or in-progress (circular)
        }
        let src = std::fs::read_to_string(&canon)
            .map_err(|e| Control::Panic(AsError::new(format!("cannot read module {}: {}", canon.display(), e))))?;
        let env = global_env();
        let exports = std::rc::Rc::new(std::cell::RefCell::new(HashSet::new()));
        let entry = ModuleEntry { env: env.clone(), exports: exports.clone() };
        // Cache BEFORE executing so circular imports resolve to this entry.
        self.modules.insert(canon.clone(), entry.clone());

        let dir = canon.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let prev_dir = std::mem::replace(&mut self.module_dir, dir);
        let prev_exports = std::mem::replace(&mut self.current_exports, exports);

        let tokens = lexer::lex(&src).map_err(Control::Panic)?;
        let program = parser::parse(&tokens).map_err(Control::Panic)?;
        let result = self.exec(&program, &env).await;

        self.module_dir = prev_dir;
        self.current_exports = prev_exports;

        result?; // propagate a panic from the module body
        Ok(entry)
    }

    fn resolve_import(&self, source: &str) -> PathBuf {
        let mut p = self.module_dir.join(source);
        if p.extension().is_none() {
            p.set_extension("as");
        }
        p
    }
```

Add a free helper for export names:
```rust
fn exported_name(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::Let { name, .. } => Some(name.clone()),
        Stmt::Fn { name, .. } => Some(name.clone()),
        Stmt::Class { name, .. } => Some(name.clone()),
        Stmt::Enum { name, .. } => Some(name.clone()),
        _ => None,
    }
}
```

Add `Import`/`Export` arms to `exec_stmt`:
```rust
            Stmt::Export(inner) => {
                let flow = self.exec_stmt(inner, env).await?;
                if let Some(name) = exported_name(inner) {
                    self.current_exports.borrow_mut().insert(name);
                }
                Ok(flow)
            }
            Stmt::Import { names, source } => {
                let resolved = self.resolve_import(source);
                let entry = self.load_module(&resolved).await?;
                match names {
                    crate::ast::ImportNames::Named(names) => {
                        for name in names {
                            if !entry.exports.borrow().contains(name) {
                                return Err(AsError::new(format!("module '{}' has no export '{}'", source, name)).into());
                            }
                            let v = entry.env.get(name).unwrap_or(Value::Nil);
                            env.define(name, v, false).map_err(AsError::new)?;
                        }
                    }
                    crate::ast::ImportNames::Namespace(alias) => {
                        let mut map = indexmap::IndexMap::new();
                        for name in entry.exports.borrow().iter() {
                            map.insert(name.clone(), entry.env.get(name).unwrap_or(Value::Nil));
                        }
                        env.define(alias, Value::Object(std::rc::Rc::new(std::cell::RefCell::new(map))), false)
                            .map_err(AsError::new)?;
                    }
                }
                Ok(Flow::Normal)
            }
```

(NOTE: `exec_stmt` matches on `&Stmt`, so the new arms compile once `Stmt::Import`/`Export` exist. The `sexpr`/`binary_span_covers_both_operands` parser tests have `_ => panic!()` arms, so new variants don't break them.)

- [ ] **Step 6: `src/lib.rs`** — add `run_file` and keep `run_source`.
```rust
use std::path::Path;

/// Run a `.as` file as the entry module (with import resolution relative to it).
pub async fn run_file(path: &Path) -> Result<String, AsError> {
    let mut interp = Interp::new();
    match interp.load_module(path).await {
        Ok(_) => Ok(interp.output),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output),
    }
}
```
(`run_source` stays as-is — string eval with `module_dir` defaulting to `.`; imports in a bare string resolve from the cwd.)

- [ ] **Step 7: `src/main.rs`** — call `run_file` instead of reading + `run_source`:
```rust
    match ascript::run_file(std::path::Path::new(path)).await {
        Ok(output) => { print!("{}", output); ExitCode::SUCCESS }
        Err(e) => { eprintln!("error: {}", e); ExitCode::from(1) }
    }
```
(Remove the manual `std::fs::read_to_string` — `run_file` reads the file. Keep the arg parsing / usage message.)

- [ ] **Step 8: Tests** — a lib test using temp files (in `src/interp.rs` tests or a new `tests/modules.rs`). Add `tests/modules.rs`:
```rust
use std::fs;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ascript_mod_{}_{}", tag, std::process::id()));
    let _ = fs::create_dir_all(&d);
    d
}

#[test]
fn named_and_namespace_imports() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("basic");
    fs::write(d.join("util.as"), "export const PI = 3\nexport fn double(x) { return x * 2 }\nfn secret() { return 99 }").unwrap();
    fs::write(d.join("main.as"),
        "import { PI, double } from \"./util\"\nimport * as u from \"./util\"\nprint(PI)\nprint(double(21))\nprint(u.double(5))").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n42\n10\n");
}

#[test]
fn importing_non_export_errors() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("noexport");
    fs::write(d.join("lib.as"), "export const A = 1\nconst B = 2").unwrap();
    fs::write(d.join("app.as"), "import { B } from \"./lib\"\nprint(B)").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("app.as")).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no export 'B'"));
}
```

- [ ] **Step 9: Run** `cargo test` (all 120 lib + 8 integration + the new module tests) and `cargo clippy --all-targets` (clean). **Commit:** `feat: add modules with export, named/namespace import, and a module loader`

---

## Task 2: Once-only evaluation + circular imports

**Files:** `tests/modules.rs` (and any `src/interp.rs` fix the tests reveal).

- [ ] **Step 1: Add caching + circular tests** to `tests/modules.rs`:
```rust
#[test]
fn module_body_runs_once() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("once");
    // side.as prints when loaded; importing it from two places must print once.
    fs::write(d.join("side.as"), "print(\"loaded\")\nexport const V = 1").unwrap();
    fs::write(d.join("a.as"), "import { V } from \"./side\"\nexport const A = V").unwrap();
    fs::write(d.join("main.as"),
        "import { V } from \"./side\"\nimport { A } from \"./a\"\nprint(V)\nprint(A)").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // "loaded" appears exactly once despite two import paths to side.as.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "loaded\n1\n1\n");
}

#[test]
fn circular_import_resolves_partial() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let d = temp_dir("circular");
    // a imports b; b imports a. a defines X before importing b, so b can use it.
    fs::write(d.join("a.as"),
        "export const X = 10\nimport { Y } from \"./b\"\nexport fn sum() { return X + Y }").unwrap();
    fs::write(d.join("b.as"),
        "import { X } from \"./a\"\nexport const Y = X + 5").unwrap();
    fs::write(d.join("main.as"),
        "import { sum } from \"./a\"\nprint(sum())").unwrap();
    let out = std::process::Command::new(bin).arg("run").arg(d.join("main.as")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // a.X=10 is defined before a imports b; b reads X=10, sets Y=15; a.sum()=25.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "25\n");
}
```

- [ ] **Step 2: Run** `cargo test`. If either test fails, the cache/circular logic from Task 1 needs a fix — diagnose and fix in `src/interp.rs` (the entry must be inserted before exec; `load_module` must return the in-progress entry on re-entry; the dir/exports save-restore must be correct). Do NOT weaken the tests. `cargo clippy --all-targets` clean.

- [ ] **Step 3: Commit:** `test: verify once-only module evaluation and circular import partial-init`

---

## Task 3: End-to-end multi-file example + integration test

**Files:** `examples/modules/` (new dir with `main.as` + `geometry.as`), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/modules/geometry.as`**
```
export const PI = 3.14159

export fn circleArea(r) {
  return PI * r * r
}

export class Rect {
  fn init(w, h) { self.w = w; self.h = h }
  fn area() { return self.w * self.h }
}
```

- [ ] **Step 2: Create `examples/modules/main.as`**
```
import { circleArea, Rect } from "./geometry"
import * as geo from "./geometry"

print(circleArea(2))
let r = Rect(3, 4)
print(r.area())
print(geo.PI)
```

- [ ] **Step 3: Add an integration test to `tests/cli.rs`**
```rust
#[test]
fn runs_modules_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg("examples/modules/main.as").output().unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("12.56636")); // circleArea(2) = 3.14159*4
    assert!(out.contains("12\n"));     // Rect(3,4).area()
    assert!(out.contains("3.14159"));  // geo.PI
}
```

- [ ] **Step 4: Run** `cargo test` (incl. `runs_modules_example`), then `cargo run --quiet -- run examples/modules/main.as` (paste output). `cargo clippy --all-targets`. **Commit:** `test: add multi-file modules end-to-end example`

---

## Definition of Done

- `cargo test` passes (all unit + integration + module tests); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/modules/main.as` imports across files (named + namespace), constructs a class from another module, and reads a namespaced export.
- AScript supports: `export` decls; named + namespace imports; relative path resolution with `.as` extension; once-only cached evaluation; circular-import partial-init.

## Hand-off to Milestone 9 ("Tooling") and Phase 2 (stdlib)

M9 adds rich diagnostics (ariadne), a REPL, `ascript fmt`, `ascript test`, and the Tree-sitter conformance test. The Phase-2 stdlib milestones then register `std/*` built-in modules: `load_module`/`resolve_import` is the hook — a `std/` prefix should resolve to a built-in module (its source or native bindings) instead of the filesystem. The `Map` value kind + `map<K,V>` type land with `std/map` in the first collections milestone (extend `Value` + `check_type` arms then).
