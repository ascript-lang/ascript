# VM Plan V1 — Foundation: typed-AST accessors, Chunk/opcodes, Fiber, disassembler, run-loop skeleton, differential harness

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Stand up the bytecode runtime skeleton **alongside** the tree-walker (which keeps running the binary): the typed-AST accessor codegen the compiler will consume, the `Chunk`/opcode/`Value`-stack/`Fiber`/`CallFrame`/`Closure` data model, a disassembler, a span table → ariadne diagnostic path, the async `run` loop proven on a trivial opcode subset, and the **differential-test harness** (VM output == tree-walker output) that every later slice extends.

**Architecture:** New crates-internal modules `src/compile/` (CST+resolver → `Chunk`) and `src/vm/` (async dispatch loop over `Fiber`s). Both are `#[cfg]`-free, `!Send`, reuse `src/value.rs` and the stdlib unchanged. NOTHING is wired into the `ascript` binary yet — the only entry is a test-only `vm_eval_source(src) -> Result<String, AsError>` used by the differential harness. The compiler consumes the **CST typed AST + `ResolveResult`** (decision locked 2026-06-02); the legacy AST/parser/interp stay until cutover (they ARE the differential oracle).

**Tech Stack:** Rust; the Plan 1–3 CST pipeline (`src/syntax/{parser,tree_builder,resolve,ast}`), `src/value.rs`, `ariadne` (diagnostics), the existing `#[tokio::main(flavor="current_thread")]` + `LocalSet` harness.

**Spec:** `docs/superpowers/specs/2026-06-02-bytecode-vm-design.md` (Architecture, Fiber model, Compiler, Instruction set, Testing-four-oracles). This plan implements only the skeleton + oracle #1 substrate; opcode coverage lands in V2–V10.

---

## Surveyed foundation APIs (ground truth — do not re-derive)

- `Value` (24 variants) — `src/value.rs:272`. `Object(Rc<RefCell<IndexMap<String,Value>>>)`, `Array(Rc<RefCell<Vec<Value>>>)`, `Function(Rc<Function>)`, `Future(crate::task::SharedFuture)`, `Generator(Rc<crate::coro::GeneratorHandle>)`, primitives `Nil/Bool/Number(f64)/Decimal/Str(Rc<str>)`, `Builtin(Rc<str>)`. Mutable containers compare by `Rc` pointer identity. No `shape_id` yet (added in V11).
- `ResolveResult { uses: HashMap<TextRange,Resolution>, frames: HashMap<(SyntaxKind,TextRange),FrameInfo>, bindings: Vec<Binding>, diagnostics }` — `src/syntax/resolve/types.rs:62`. `Resolution::{Local(u32),Upvalue(u32),Global(String),Unresolved}`. `FrameInfo { slot_count:u32, upvalues:Vec<UpvalueDescriptor>, cell_slots:Vec<u32> }`. `UpvalueDescriptor::{ParentLocal(u32),ParentUpvalue(u32)}`.
- CST typed AST — `src/syntax/ast/mod.rs` (generated `cast()`/`syntax()` only today; **Task 1 adds accessors**). `ResolvedNode` (`src/syntax/cst.rs:14`): `.kind()`, `.text_range() -> cstree::text::TextRange`, `.children()`, `.children_with_tokens()`, `.text()`.
- `Control { Panic(AsError), Propagate(Value), Exit(i32) }` — `src/interp.rs:35`. `Flow { Normal, Return(Value), Break, Continue }`.
- Entry points — `src/lib.rs`: `run_source_exit(src)->Result<(String,Option<i32>),AsError>` (capture mode), `run_file`, `run_tests`. `#[tokio::main(flavor="current_thread")]` + `LocalSet` in `src/main.rs`.
- `Span` — `src/span.rs` (byte offsets + line/col). CST `TextRange` is byte offsets into the same source → `Span::start == usize::from(range.start())`.

---

## File Structure

- `build.rs` — extend `generate_ast_nodes` to emit typed child accessors.
- `src/vm/mod.rs` — `pub mod`s; re-exports.
- `src/vm/opcode.rs` — `Op` enum (`#[repr(u8)]`), the exhaustive opcode table, `Op::from_u8`.
- `src/vm/chunk.rs` — `Chunk`, `FnProto`, `UpvalueDescriptor` (re-export resolver's), const pool, span table, `add_op`/`add_const`/etc.
- `src/vm/value_ext.rs` — `Closure`, `RunOutcome`, `FiberState` (VM-only runtime types; NOT in value.rs yet — V4/V5/V7 fold the closure into `Value`).
- `src/vm/fiber.rs` — `Fiber`, `CallFrame`.
- `src/vm/disasm.rs` — `disasm(&Chunk) -> String`.
- `src/vm/run.rs` — `Vm` + `async fn run(&self, fiber:&mut Fiber) -> Result<RunOutcome,Control>` (minimal opcode subset).
- `src/compile/mod.rs` — `compile_source(src) -> Result<Chunk, CompileError>` skeleton (literals + arithmetic only this plan).
- `src/lib.rs` — add `mod vm; mod compile;` and a `#[cfg(test)]`/`pub(crate)` `vm_eval_source` for the harness.
- `tests/vm_differential.rs` — the differential harness + first smoke cases.

---

## Task 1: Typed-AST child accessors in the codegen

**Files:** `build.rs`; verify against `src/syntax/ast/mod.rs`.

The compiler must read structured children from CST nodes ergonomically. Today `build.rs` emits only `cast()`/`syntax()`. Add typed accessors so e.g. `BinaryExpr` exposes its operands and operator token, `LetStmt` its name/type/initializer, etc.

- [ ] **Step 1: Read the current codegen + ungrammar** Read `build.rs` `generate_ast_nodes` and `docs/superpowers/specs/grammar/tree-sitter-ascript/...`/`src/syntax/ast/ascript.ungram`. Note each `Rule::Node`/`Rule::Token`/labelled field (`name:Expr`) per node. The ungrammar already encodes the shape — accessors are mechanical from it.

- [ ] **Step 2: Emit accessors (rust-analyzer style).** For each concrete struct node, for each field in its rule:
  - A single child node of type `T` (`field:T`) → `pub fn <field>(&self) -> Option<T> { self.0.children().filter_map(|c| T::cast(c.clone())).next() }` (label-named, else type-named lowercased).
  - Repeated child nodes (`T*` / appears in a list) → `pub fn <field>s(&self) -> impl Iterator<Item=T> + '_`.
  - A token (`'+' | '-'` operator, or a keyword/ident) → `pub fn <name>_token(&self) -> Option<crate::syntax::cst::SyntaxToken> { … first non-trivia token whose kind ∈ set … }`; for an operator alternation emit `pub fn op(&self) -> Option<SyntaxKind>` returning the operator token's kind.
  - Enum nodes (Expr/Stmt/Pat/Type) keep `cast()`; ALSO emit a positional helper where the ungrammar labels them (e.g. `BinaryExpr.lhs:Expr`, `.rhs:Expr` — distinguish by child order when both are `Expr`: `nth_of::<Expr>(0)`/`(1)`).
  Implement a small generic helper module the generated code can call: `pub(crate) fn child<T:AstNode>(n:&ResolvedNode)->Option<T>`, `children::<T>`, `nth_child::<T>(n,i)`, `token(n, kind)`. Introduce a tiny `pub trait AstNode { fn cast(ResolvedNode)->Option<Self>; fn syntax(&self)->&ResolvedNode; }` implemented by every generated node (emit the impl in codegen). Put the helpers in `src/syntax/ast/support.rs` (hand-written, `include!`-adjacent) and `use` them from generated code.

- [ ] **Step 3: Drop the blanket `#[allow(dead_code)]`** on `mod generated` in `src/syntax/ast/mod.rs` once accessors are referenced by tests; if some accessors are still unused this plan, keep a NARROW `#[allow(dead_code)]` only where needed (the compiler in V2+ consumes the rest). Prefer `#[cfg_attr(not(test), allow(dead_code))]` over a blanket allow.

- [ ] **Step 4: Accessor unit tests** (in `src/syntax/ast/mod.rs` tests): `BinaryExpr` from `1 + 2` → `.lhs()`/`.rhs()` are `Expr::Literal`, `.op() == Some(SyntaxKind::Plus)`. `LetStmt` from `let x = 1` → name token text `"x"`, initializer is an `Expr`. `CallExpr` from `f(1,2)` → callee + 2 args. A `FnDecl` → name + `ParamList` + body `Block`. Cover the node shapes V2–V4 need first (Literal, NameRef, BinaryExpr, UnaryExpr, ParenExpr, CallExpr, LetStmt, ExprStmt, Block, IfStmt, WhileStmt, ReturnStmt, FnDecl, ArrowExpr).

- [ ] **Step 5: Gates + commit** `cargo build` (codegen compiles), `cargo test --lib syntax::ast`, `cargo clippy --all-targets` + `--no-default-features --all-targets` clean.
```bash
git add build.rs src/syntax/ast/
git commit -m "feat(ast): typed child accessors in ungrammar codegen (compiler input surface)"
```

---

## Task 2: Opcode set + `Chunk` + span table

**Files:** `src/vm/opcode.rs`, `src/vm/chunk.rs`, `src/vm/mod.rs`, `src/lib.rs` (`mod vm;`).

- [ ] **Step 1: The exhaustive `Op` enum** (`#[repr(u8)]`, `derive(Clone,Copy,PartialEq,Eq,Debug)`). Declare the FULL set now (later slices implement decode/exec arms); each documents its operands. From the spec's table:
```
// stack/consts:    CONST(u16 const-idx), NIL, TRUE, FALSE, POP, DUP
// locals/upvalues:  GET_LOCAL(u16), SET_LOCAL(u16), GET_UPVALUE(u16), SET_UPVALUE(u16), CLOSE_UPVALUE(u16)
// globals:          GET_GLOBAL(u16 name-const), SET_GLOBAL(u16)
// arithmetic/logic: ADD,SUB,MUL,DIV,MOD,POW,NEG,NOT,EQ,NE,LT,LE,GT,GE,AND? (short-circuit via jumps, not an op),COALESCE? (via jumps)
// control flow:     JUMP(i16), JUMP_IF_FALSE(i16), JUMP_IF_TRUE(i16), LOOP(i16)
// calls/returns:    CALL(u8 argc), RETURN, CLOSURE(u16 proto-idx) [+ inline capture descriptors]
// collections:      NEW_ARRAY(u16 n), NEW_OBJECT(u16 n), SPREAD, GET_INDEX, SET_INDEX, GET_PROP(u16 name-const)[+u16 ic], SET_PROP(u16)[+u16 ic], GET_PROP_OPT(u16)
// classes/enums:    CLASS(u16), METHOD(u16), GET_SUPER(u16), INSTANCE_OF
// strings:          TEMPLATE(u16 n)
// AScript:          AWAIT, YIELD, MAKE_GENERATOR, PROPAGATE, UNWRAP, MATCH_* (V10), IMPORT(u16)
```
Use a `match` for `Op::from_u8(u8)->Option<Op>` and `Op::operand_width()` (bytes of inline operands, excluding the opcode byte and IC slot). Document that specializable ops (ADD/GET_GLOBAL/GET_PROP/SET_PROP/CALL) reserve a trailing `u16` IC index (populated in V11; encode 0 now). Unit test: round-trip every `Op` through `from_u8(op as u8)`.

- [ ] **Step 2: `Chunk`** in `chunk.rs`:
```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub consts: Vec<Value>,            // compile-time literals + nested FnProto-as-Value? No: protos separate
    pub protos: Vec<Rc<FnProto>>,      // nested function prototypes (CLOSURE proto-idx)
    pub spans: Vec<(usize, crate::span::Span)>, // (code offset, span), sorted by offset
    pub upvalues: Vec<crate::syntax::resolve::types::UpvalueDescriptor>, // this fn's capture plan
    pub slot_count: u16,               // locals to reserve on call
    pub ic_count: u16,                 // inline-cache slots (V11)
    pub name: Option<String>,
}
pub struct FnProto { pub chunk: Chunk, pub arity: u8, pub has_rest: bool, pub is_async: bool, pub is_generator: bool }
```
Builder methods: `emit(&mut self, op: Op, span: Span)`, `emit_u16(&mut self, op, operand:u16, span)`, `emit_jump(op,span)->usize` (placeholder, returns patch site), `patch_jump(site)`, `add_const(&mut self, Value)->u16` (dedup primitives by structural eq), `add_proto(Rc<FnProto>)->u16`, `span_at(offset)->Span` (binary search the span table; fallback nearest-preceding). Unit test the span table: emit ops at offsets, `span_at` returns the right span (and nearest-preceding for an offset mid-instruction).

- [ ] **Step 3: Gate + commit** `cargo test --lib vm::opcode vm::chunk`, clippy both configs.
```bash
git add src/vm/ src/lib.rs
git commit -m "feat(vm): opcode set + Chunk + span table"
```

---

## Task 3: `Fiber`/`CallFrame`/`Closure` runtime model

**Files:** `src/vm/value_ext.rs`, `src/vm/fiber.rs`.

- [ ] **Step 1: Closure + outcome types** (`value_ext.rs`):
```rust
pub struct Closure { pub proto: Rc<FnProto>, pub upvalues: Vec<Rc<RefCell<Value>>> }
pub enum RunOutcome { Done(Value), Yielded(Value) }
pub enum FiberState { Running, Suspended, Done }
```
> Closure lives in `src/vm/` for now; V4/V5 add a `Value::Closure(Rc<Closure>)` variant to `value.rs` once calls/closures land. Keep it isolated so value.rs is touched minimally and once.

- [ ] **Step 2: Fiber + CallFrame** (`fiber.rs`):
```rust
pub struct CallFrame { pub closure: Rc<Closure>, pub ip: usize, pub slot_base: usize }
pub struct Fiber { pub frames: Vec<CallFrame>, pub stack: Vec<Value>, pub state: FiberState }
impl Fiber {
    pub fn new(top: Rc<Closure>) -> Self { /* one frame, slot_base 0, reserve top.proto.chunk.slot_count Nil slots */ }
    pub fn frame(&self) -> &CallFrame; pub fn frame_mut(&mut self) -> &mut CallFrame;
    pub fn push(&mut self, v: Value); pub fn pop(&mut self) -> Value; pub fn peek(&self, back:usize)->&Value;
}
```
The explicit `frames: Vec<CallFrame>` is what makes recursion heap-bounded (spec). `slot_base` indexes into `stack` for this frame's locals; locals occupy `stack[slot_base .. slot_base+slot_count]`, operands push above.

- [ ] **Step 3: Unit tests** Construct a `Fiber` over a hand-built `Closure` (proto with `slot_count=2`); assert initial stack has 2 `Nil` slots; push/pop/peek behave; `frame()` returns the sole frame. Gate + commit.
```bash
git add src/vm/; git commit -m "feat(vm): Fiber/CallFrame/Closure runtime model"
```

---

## Task 4: Disassembler

**Files:** `src/vm/disasm.rs`.

- [ ] **Step 1:** `pub fn disasm(chunk:&Chunk) -> String` and `disasm_at(chunk,&mut offset)->String` (one instruction). Format `0000 CONST    3 ; 42` (offset, op name padded, operand, `;` const/comment). Decode each op via `Op::from_u8` + `operand_width`; for `CONST`/`GET_GLOBAL` show the const value; for jumps show the absolute target. Recurse into `protos` (`disasm` prints nested protos with an indent + header `== fn <name> ==`). This is the substrate for ALL compiler unit tests (assert emitted opcodes by disassembly) and the primary debug tool.

- [ ] **Step 2: Tests** Hand-build a `Chunk` (`CONST 0; CONST 1; ADD; RETURN` with consts `[1,2]`), assert `disasm` contains `CONST` lines with `; 1`/`; 2` and an `ADD`/`RETURN`. Gate + commit.
```bash
git add src/vm/disasm.rs; git commit -m "feat(vm): disassembler"
```

---

## Task 5: The async `run` loop (trivial opcode subset) + panic→diagnostic

**Files:** `src/vm/run.rs`.

- [ ] **Step 1: `Vm` + run loop.** `Vm` borrows the existing `Interp` for stdlib/`call_value` reuse (hold `interp: Rc<Interp>` — the survey confirms `call_value`/`call_stdlib`/`global_env` live there; the VM delegates native calls). Implement:
```rust
pub enum RunOutcome { Done(Value), Yielded(Value) }
impl Vm {
    pub async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        loop {
            let op = self.read_op(fiber);          // decode at frame.ip, advance ip
            match op {
                Op::Const => { let i = self.read_u16(fiber); let v = chunk.consts[i].clone(); fiber.push(v); }
                Op::Nil => fiber.push(Value::Nil), Op::True => fiber.push(Value::Bool(true)), ...
                Op::Add => { let b=fiber.pop(); let a=fiber.pop(); fiber.push(self.add(a,b, span)?); }
                Op::Sub|Mul|Div|Mod|Pow|Neg|Not|Eq|Ne|Lt|Le|Gt|Ge => { /* numeric/bool per tree-walker semantics */ }
                Op::Pop => { fiber.pop(); }
                Op::Return => { let v = fiber.pop(); return Ok(RunOutcome::Done(v)); }
                other => return Err(self.panic_at(fiber, format!("opcode {other:?} not yet implemented"))),
            }
        }
    }
}
```
Arithmetic/compare semantics MUST mirror the tree-walker EXACTLY (numbers are `f64`; `Decimal` ops; string `+`? — in V2 confirm what the tree-walker does for `ADD` on strings; this plan only needs Number `ADD`/`SUB`/`MUL`/`NEG`/`EQ`/`LT` to prove the loop — leave the rest as `unimplemented`-via-panic until V2). The run loop is `&self` + `&mut Fiber` (Fiber is a `&mut` local, never behind a RefCell across an await — preserves `clippy::await_holding_refcell_ref`).

- [ ] **Step 2: panic→diagnostic.** On a VM panic, build the `Control::Panic(AsError)` with the span from `chunk.span_at(frame.ip_at_fault)` so ariadne points at source IDENTICALLY to the tree-walker. Add `panic_at(&self, fiber, msg) -> Control`.

- [ ] **Step 3: Tests** Hand-build a `Chunk` computing `(1+2)*4` → `RETURN`, wrap in a `Closure`/`Fiber`, `run` it inside a `LocalSet`, assert `Done(Value::Number(12.0))`. A `CONST` of a bad index → panic with a span. Gate + commit.
```bash
git add src/vm/run.rs; git commit -m "feat(vm): async run loop (sync subset) + span-based panic diagnostics"
```

---

## Task 6: Minimal compiler (literals + arithmetic) + `vm_eval_source`

**Files:** `src/compile/mod.rs`, `src/lib.rs`.

- [ ] **Step 1: `compile_source`.** Parse (CST), resolve, then walk the typed AST emitting the subset: number/bool/nil/string `Literal` → `add_const`+`CONST` (parse the literal token text ONCE here — `f64`/string-unescape — per spec "literals parsed once at compile time"); `BinaryExpr` with `+ - * /` over numbers → emit operands then `ADD`/`SUB`/`MUL`/`DIV`; `UnaryExpr` `-`/`!` → `NEG`/`NOT`; `ParenExpr` → compile inner (no op); a top-level `ExprStmt` whose value is the program result → leave on stack then `RETURN` (this plan: a source file that is a single expression statement). Use the V1 accessors. Emit spans from each node's `text_range()` (`Span::from(range)`). Return `Chunk`.
> Full statement/print/locals coverage is V2 — this task only needs enough to evaluate arithmetic expressions for the harness smoke test.

- [ ] **Step 2: `vm_eval_source`** in `src/lib.rs` (`pub(crate)`, used by the harness + later the differential oracle):
```rust
pub(crate) async fn vm_eval_source(src: &str) -> Result<Value, AsError> {
    let chunk = crate::compile::compile_source(src).map_err(|e| /* AsError */)?;
    let proto = Rc::new(FnProto{ chunk, arity:0, has_rest:false, is_async:false, is_generator:false });
    let closure = Rc::new(Closure{ proto, upvalues: vec![] });
    let interp = Rc::new(Interp::new()); interp.install_self();
    let vm = Vm::new(interp);
    let mut fiber = Fiber::new(closure);
    match vm.run(&mut fiber).await { Ok(RunOutcome::Done(v)) => Ok(v), Err(c) => /* map Control→AsError */, _ => unreachable }
}
```
Drive it inside a `LocalSet` (mirror `run_source_exit`). Output capture (`print`) is wired in V2 when `print` lands; this task returns the final `Value`.

- [ ] **Step 3: Tests** `vm_eval_source("1 + 2 * 3")` → `Number(7.0)`; `"-(4)"` → `Number(-4.0)`; `"(1+2)*4"` → `Number(12.0)`. Gate + commit.
```bash
git add src/compile/ src/lib.rs; git commit -m "feat(compile): minimal literal+arithmetic compiler; vm_eval_source"
```

---

## Task 7: The differential-test harness

**Files:** `tests/vm_differential.rs`.

- [ ] **Step 1: The harness.** A helper that, given source, runs it through BOTH the tree-walker (`run_source_exit`) and the VM (`vm_eval_source` / later a VM run-to-output path) and asserts **byte-identical** results. This plan: since the VM only evaluates arithmetic to a `Value`, the harness compares the VM's final `Value` stringified against the tree-walker's captured output for a single-expression program (e.g. wrap as `print(<expr>)` for the tree-walker, compare to VM `Value` formatted with the SAME `Value`→string the tree-walker's `print` uses — reuse `value.rs`'s display). Keep the comparison helper generic so V2+ swaps in full stdout comparison.
```rust
fn assert_vm_matches_treewalker(expr_src: &str) { /* tree-walker print(expr) vs vm_eval_source(expr) formatted */ }
```

- [ ] **Step 2: Smoke cases** `1+2`, `2*3+4`, `-(5)`, `(1+2)*4`, `10/4`, `7 % 3` (only ops the VM implements this plan). Each asserted identical. Document in a comment that the corpus-wide differential gate (whole `examples/`) turns on once the VM covers statements/print (V2) and grows per slice.

- [ ] **Step 3: Gate + commit** `cargo test --test vm_differential`, full `cargo test`, clippy both configs.
```bash
git add tests/vm_differential.rs; git commit -m "test(vm): differential harness (VM == tree-walker) + arithmetic smoke"
```

---

## Done criteria (V1)
- [ ] `cargo test` green; clippy clean in both feature configs; tree-walker still runs the binary unchanged (nothing wired to `main.rs`).
- [ ] Typed-AST accessors generated + tested (the compiler's input surface).
- [ ] `Chunk`/opcode/`Fiber`/`Closure`/disassembler/span-table exist and are unit-tested.
- [ ] The async `run` loop evaluates an arithmetic subset; panics produce span-accurate ariadne diagnostics.
- [ ] `vm_eval_source` + the differential harness prove VM == tree-walker on arithmetic.
- [ ] No `Value` variant added yet (closure stays VM-local); `value.rs` untouched besides nothing.

**Next:** V2 (sync core) — full literals/strings, `print` + output capture, `GET/SET_LOCAL`, `GET_GLOBAL`, `ExprStmt`/`Block` semantics, complete arithmetic/compare/logical (short-circuit via jumps), and the differential gate widened to whole-program stdout on the sync subset of `examples/`.
