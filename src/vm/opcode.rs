//! The bytecode instruction set for the AScript register-of-stack VM.
//!
//! Each [`Op`] is a single opcode byte. Inline operands (if any) follow the
//! opcode byte in the chunk's `code` stream, little-endian. The number of inline
//! operand bytes is given by [`Op::operand_width`]; the disassembler and the VM
//! decode loop use it to advance the instruction pointer.
//!
//! Operand encodings used below:
//! - `u16` — a 2-byte little-endian index (into the const pool, proto table, or
//!   a local/upvalue slot).
//! - `u8` — a single byte (call argument count).
//! - `i16` — a signed 2-byte little-endian jump displacement, measured from the
//!   byte *after* the operand to the jump target.
//!
//! Inline caches: a handful of ops are *specializable* (see
//! [`Op::has_inline_cache`]). Their per-site cache state is NOT stored inline in
//! the code stream; V11 will keep a parallel `Vec<InlineCache>` indexed by a slot
//! reserved at compile time. Therefore [`Op::operand_width`] reflects only the
//! real inline operands and never reserves space for IC bytes.

/// A single bytecode instruction.
///
/// `#[repr(u8)]` so the discriminant is exactly the opcode byte written into a
/// [`crate::vm::chunk::Chunk`]'s `code` vector. Exec arms for these ops are
/// implemented incrementally across VM plan slices V2–V10; until then several
/// variants are constructed only via [`Op::from_u8`] / `op as u8`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    // ---- stack / constants ------------------------------------------------
    /// `CONST(u16)` — push `consts[idx]`.
    Const,
    /// Push `nil`.
    Nil,
    /// Push `true`.
    True,
    /// Push `false`.
    False,
    /// Pop and discard the top of stack.
    Pop,
    /// Duplicate the top of stack.
    Dup,
    /// `a b -- b a` — swap the top two stack values. Used by assignment lowering
    /// to reorder a value-then-receiver evaluation (tree-walker order) into the
    /// `[receiver, value]` layout `SET_PROP` consumes.
    Swap,
    /// `a b c -- b c a` — rotate the top three values left (the value 3rd from the
    /// top moves to the top). Used by index-assignment lowering to reorder a
    /// value-then-receiver-then-index evaluation (tree-walker order) into the
    /// `[receiver, index, value]` layout `SET_INDEX` consumes.
    Rot3,

    // ---- locals / upvalues ------------------------------------------------
    /// `GET_LOCAL(u16)` — push the value in local slot `n`.
    GetLocal,
    /// `SET_LOCAL(u16)` — store TOS into local slot `n` (leaves TOS in place).
    SetLocal,
    /// `GET_UPVALUE(u16)` — push the captured upvalue `n`.
    GetUpvalue,
    /// `SET_UPVALUE(u16)` — store TOS into captured upvalue `n`.
    SetUpvalue,
    /// `CLOSE_UPVALUE(u16)` — close (hoist to the heap) the upvalue cell for
    /// local slot `n` as it leaves scope.
    CloseUpvalue,

    // ---- globals ----------------------------------------------------------
    /// `GET_GLOBAL(u16)` — push the global named by `consts[idx]` (a `Str`).
    /// The VM's globals are the bare builtins (`crate::interp::BUILTIN_NAMES`);
    /// the result is the corresponding `Value::Builtin`.
    GetGlobal,
    /// `DEFINE_GLOBAL(u16 name, u8 mutable)` — pop TOS and bind it as the MODULE-SCOPE
    /// user-global named by `consts[name]` (a `Str`), creating or overwriting the entry
    /// (and recording its REASSIGNABILITY: `mutable == 1` for a `let`, `0` for an
    /// immutable `const`/`fn`/`class`/`enum`/`import`) and bumping the global version.
    /// Emitted by every DIRECT-child top-level binding define-site. Pairs with
    /// [`Op::GetGlobal`] (which consults the user-globals table before the builtins),
    /// so a function/thunk body referencing a top-level binding declared LATER
    /// late-binds at run time — matching the tree-walker's single shared module
    /// `Environment`. The recorded mutability lets [`Op::SetGlobal`] reject a CROSS-CHUNK
    /// reassignment of an immutable global (REPL line-to-line; a main module reassigning
    /// an import) at run time, which the compile-time [`Op::ImmutableError`] cannot see.
    DefineGlobal,
    /// `SET_GLOBAL(u16)` — store TOS into the EXISTING module-scope user-global named
    /// by `consts[idx]` (a `Str`). Emitted by a top-level REASSIGNMENT `x = …` whose
    /// target resolves to a module global. RUNTIME mutability check (the single source
    /// of truth for GLOBAL assignment targets): if the global is IMMUTABLE
    /// (`const`/`fn`/`class`/`enum`/`import`), raise `cannot assign to immutable binding
    /// '<name>'` at the target span — even when the immutable decl is in an EARLIER,
    /// separately-compiled chunk (REPL/imports), where the compile-time
    /// [`Op::ImmutableError`] would not fire. Assigning to a name not present at all
    /// raises `cannot assign to undefined variable '<name>'`. (A builtin global is
    /// immutable and never reached — the tree-walker rejects `print = 5` earlier.)
    SetGlobal,
    /// `IMMUTABLE_ERROR(u16 name)` — UNCONDITIONALLY raise the Tier-2 panic
    /// `cannot assign to immutable binding '<name>'` (name = `consts[idx]`, a `Str`)
    /// anchored at this op's span (the assignment TARGET's span). Emitted by the
    /// assignment lowering IN PLACE of a store when the target's resolved binding is
    /// IMMUTABLE (`const`/`fn`/`class`/`enum`/`import`/loop-var, or a const-pattern
    /// bind). Because it is emitted at the store position AFTER the value is
    /// evaluated, it fires with the SAME runtime timing as the tree-walker's
    /// `Environment::assign` immutable error: the RHS side-effects run first, dead /
    /// never-reached assignments never trigger it, and the message + span match
    /// byte-for-byte. No stack contract (it always diverges).
    ImmutableError,

    // ---- arithmetic / logic ----------------------------------------------
    /// `a b -- (a + b)`.
    Add,
    /// `a b -- (a - b)`.
    Sub,
    /// `a b -- (a * b)`.
    Mul,
    /// `a b -- (a / b)`.
    Div,
    /// `a b -- (a % b)`.
    Mod,
    /// `a b -- (a ** b)`.
    Pow,
    /// `a -- (-a)`.
    Neg,
    /// `a -- (!a)`.
    Not,
    /// `a b -- (a == b)`.
    Eq,
    /// `a b -- (a != b)`.
    Ne,
    /// `a b -- (a < b)`.
    Lt,
    /// `a b -- (a <= b)`.
    Le,
    /// `a b -- (a > b)`.
    Gt,
    /// `a b -- (a >= b)`.
    Ge,
    /// `a b -- [a, a+1, .. b)` — eager half-open `array<number>` (step 1). Mirrors
    /// the tree-walker's `BinOp::Range`; both bounds must be `Number`.
    Range,
    /// `a b -- [a, a+1, ..= b]` — eager INCLUSIVE `array<number>` (step 1).
    /// Mirrors the tree-walker's value-position `..=` materialization; both bounds
    /// must be `Number`. Ascending only (direction inference is a later phase, so
    /// `a > b` yields `[]`).
    RangeInclusive,
    /// `a b -- a b` (peek-only) — verify the top TWO stack values are both
    /// `Value::Float`, otherwise raise the Tier-2 panic carried at this op's span.
    /// Used to lower the for-range bounds check eagerly (before the loop) so the
    /// VM reports `for-range bounds must be numbers` at the START bound's span,
    /// byte-identically to the tree-walker's `Stmt::ForRange`. Leaves both operands
    /// in place so the surrounding lowering can store them into slots.
    CheckNumbers,
    /// `RANGE_STEP_VALUE(u8 flags)` — `lo hi step -- array<number>` (step on top).
    /// Materialize a stepped value range honoring the `step`'s sign as direction.
    /// `flags` bit0 = inclusive (`..=`); bit1 = step PRESENT (1) vs OMITTED (0).
    /// When omitted, `step` on the stack is an ignored placeholder and the
    /// omitted-default (`1.0` this phase) is used. Delegates to the SHARED
    /// `interp::materialize_range_stepped`/`resolve_step` so it is byte-identical
    /// to the tree-walker (incl. the zero/non-finite and direction-mismatch panics).
    RangeStepValue,
    /// `RANGE_RESOLVE_STEP(u8 present)` — `lo hi step -- lo hi resolved_step`. The
    /// for-range loop SETUP: peek `lo`/`hi`, take `step` (top), run the SHARED
    /// `resolve_step` (panic on zero/non-finite/mismatch), and push the resolved
    /// effective step back (replacing the input `step`). `present` = 1 when a `step`
    /// expr was written, 0 when omitted (the placeholder is ignored and the
    /// omitted-default is resolved). `lo`/`hi` must already be verified numbers
    /// (`CHECK_NUMBERS` runs first); the panic span is the START bound's, matching
    /// the tree-walker.
    RangeResolveStep,
    /// `RANGE_HAS_NEXT(u8 inclusive)` — `i hi step -- ok:bool` (step on top). The
    /// for-range loop CONDITION: push `true` iff the loop should continue, via the
    /// SHARED direction-aware predicate `interp::range_has_next` (positive step:
    /// `i < hi`/`i <= hi`; negative step: `i > hi`/`i >= hi`). `inclusive` = 1 for
    /// `..=`. Never panics (validation already happened in `RANGE_RESOLVE_STEP`).
    RangeHasNext,

    // ---- control flow -----------------------------------------------------
    /// `JUMP(i16)` — unconditional relative jump.
    Jump,
    /// `JUMP_IF_FALSE(i16)` — pop TOS; jump if falsy.
    JumpIfFalse,
    /// `JUMP_IF_TRUE(i16)` — pop TOS; jump if truthy.
    JumpIfTrue,
    /// `JUMP_IF_NOT_NIL(i16)` — pop TOS; jump if it is NOT `nil`. Used to lower
    /// the nil-coalescing operator `??` (jump = keep the non-nil left operand),
    /// mirroring how `JUMP_IF_FALSE`/`JUMP_IF_TRUE` lower `&&`/`||`. Reusable by
    /// later control-flow slices.
    JumpIfNotNil,
    /// `LOOP(i16)` — unconditional backward relative jump (negative displacement).
    Loop,
    /// `JUMP_IF_ARG_SUPPLIED(u16 param_index, i16 offset)` — default-parameter
    /// prologue guard. If the current frame's `argc` (count of SUPPLIED positional
    /// args) is `> param_index`, the caller passed this param, so jump FORWARD by
    /// `offset` to skip its default-eval code. Otherwise fall through and run the
    /// default. Touches no operand stack. Emitted only in a function prologue.
    JumpIfArgSupplied,
    /// `CHECK_PARAM(u16 param_index)` — contract-check the value on TOS (the just-
    /// evaluated default) against `proto.params[param_index]`'s declared type. A
    /// mismatch is a Tier-2 panic anchored at the frame's call span (`ret_span`),
    /// byte-identical to the tree-walker's default contract check. Leaves TOS in
    /// place (the following `SET_LOCAL` stores it). A no-op-emit when the param is
    /// untyped (the compiler skips it).
    CheckParam,

    // ---- calls / returns --------------------------------------------------
    /// `CALL(u8)` — call with `argc` arguments already on the stack above the
    /// callee.
    Call,
    /// `CALL_METHOD(u16 name, u8 argc)` — a method call `recv.<name>(args)`.
    /// The receiver sits below its `argc` arguments on the stack
    /// (`[..., recv, arg0, .., arg{argc-1}]`); `name` is `consts[name]` (a `Str`).
    /// Mirrors the tree-walker's `eval_chain` Call arm for a `Member` callee
    /// EXACTLY: the schema fluent-method hook (receiver is a schema value AND
    /// `name` is a schema method → `call_schema`), else the fallback
    /// `read_member(recv, name)` (which can error first — nil receiver, …) → THEN
    /// `call_value`. Two inline operands: a `u16` const index then a `u8` argc.
    CallMethod,
    /// `CALL_METHOD_SPREAD(u16 name)` — the dynamic-arity counterpart of
    /// [`Op::CallMethod`]: a method call `recv.<name>(...args)` whose argument list
    /// contains a `...spread`, so the argc cannot be known statically. The arguments
    /// arrived as a single runtime `Value::Array` (built by the array/spread builder
    /// ops) sitting on top of the receiver `[..., recv, argsArray]`. The op pops the
    /// args array, flattens its elements, pops the receiver, and dispatches EXACTLY
    /// like [`Op::CallMethod`] (the schema fluent-method hook, else the METHOD
    /// inline-cache compiled-method fast path, else `read_member` → `call_value`),
    /// applying arity + per-param contracts to the FLATTENED arg list via the shared
    /// `check_call_args`. The single `u16` operand is the method-name const index
    /// (`consts[name]`, a `Str`); there is no inline argc operand (it is dynamic).
    /// Like `CALL_METHOD`, it has its own method IC keyed by this op's bytecode
    /// offset, so the compiled-method fast path applies here too.
    CallMethodSpread,
    /// Return TOS from the current frame.
    Return,
    /// `CLOSURE(u16)` — build a closure from `protos[idx]`, capturing upvalues.
    Closure,

    // ---- collections ------------------------------------------------------
    /// `NEW_ARRAY(u16)` — pop `n` elements, push a new array.
    NewArray,
    /// `NEW_OBJECT(u16)` — pop `n` key/value pairs, push a new object.
    NewObject,
    /// `NEW_MAP` — push a new, empty `Value::Map`. The `#{…}` map-literal builder
    /// starts here, then runs one [`Op::MapEntry`] per entry. (`#{}` is just this
    /// op.)
    NewMap,
    /// `[map, key, val] -- [map]` — convert `key` to a `MapKey` and insert
    /// `key -> val` into the under-construction `map` (which sits below the
    /// key/value). Mirrors the tree-walker's `ExprKind::Map`: an unhashable `key`
    /// (a container/function/instance — `MapKey::from_value` returns `None`) raises
    /// the SAME Tier-2 panic `cannot use {type} as a map key`, anchored at this op's
    /// span (the entry's trivia-trimmed code span = the key span). A later duplicate
    /// key OVERWRITES the value but KEEPS the first-seen position (IndexMap insert).
    MapEntry,
    /// `[arr, v] -- [arr]` — flatten the spread operand `v` into the
    /// under-construction array `arr` (which sits just below `v` on the stack).
    /// Used for BOTH array-literal spreads `[...a]` AND call-argument spreads
    /// `f(...a)` (call args are built into a scratch array, then dispatched via
    /// [`Op::CallSpread`]). `v` MUST be a `Value::Array`; its elements are appended
    /// (cloned) to `arr` in order. Any other value raises the SAME Tier-2 panic the
    /// tree-walker's `ExprKind::Array` spread arm raises — `can only spread an array
    /// into an array, got {type}` — anchored at this op's span (the spread operand
    /// expression's trivia-trimmed code span). The call-args lowering rewrites the
    /// message to `can only spread an array as call arguments, got {type}` by
    /// emitting [`Op::SpreadArgs`] instead (same mechanism, different wording).
    Spread,
    /// `[arr, v] -- [arr]` — IDENTICAL to [`Op::Spread`] (flatten the array `v`
    /// into `arr`), EXCEPT the non-array panic message is `can only spread an array
    /// as call arguments, got {type}` to match the tree-walker's `eval_call_args`
    /// spread arm. Emitted only by the call-argument builder.
    SpreadArgs,
    /// `[arr, item] -- [arr]` — append a single `item` to the under-construction
    /// array `arr` (which sits just below `item`). Used by the array-literal and
    /// call-argument builders for a non-spread element. Never panics (the value
    /// below is always a compiler-produced builder array).
    AppendArray,
    /// `[obj, key, val] -- [obj]` — insert `key -> val` into the under-construction
    /// object `obj` (which sits below the key/value). `key` is a `Value::Str`.
    /// Mirrors the tree-walker's `ExprKind::Object` `IndexMap::insert`: a later
    /// duplicate key OVERWRITES the value but KEEPS the first-seen position. Used by
    /// the object-literal builder for a non-spread `key: value` entry.
    AppendObject,
    /// `[obj, v] -- [obj]` — flatten the object spread operand `v` into the
    /// under-construction object `obj`. `v` MUST be a `Value::Object`; each of its
    /// entries is inserted (later-wins, first-position, like [`Op::AppendObject`]).
    /// Any other value raises the SAME Tier-2 panic the tree-walker's
    /// `ExprKind::Object` spread arm raises — `can only spread an object into an
    /// object, got {type}` — anchored at this op's span (the spread operand's
    /// trivia-trimmed code span).
    SpreadObject,
    /// `[callee, args] -- [result]` — call `callee` with the runtime-length argument
    /// list held in the `args` array (built by the array/spread builder ops). The
    /// dynamic-arity counterpart of [`Op::Call`]: it dispatches EXACTLY like
    /// `Op::Call` (closure / async-fn / generator-fn / native via `call_value`),
    /// applying arity + per-param contracts to the FLATTENED arg list via the shared
    /// `check_call_args`. Emitted whenever a call's argument list contains a spread,
    /// so the argc cannot be known statically.
    CallSpread,
    /// `obj key -- obj[key]`.
    GetIndex,
    /// `obj key val -- val` — store `obj[key] = val`.
    SetIndex,
    /// `GET_PROP(u16)` — `obj -- obj.<name>` (name = `consts[idx]`).
    GetProp,
    /// `SET_PROP(u16)` — `obj val -- val` — store `obj.<name> = val`.
    SetProp,
    /// `GET_PROP_OPT(u16)` — `obj -- obj?.<name>` (nil-short-circuiting).
    GetPropOpt,

    // ---- classes / enums --------------------------------------------------
    /// `CLASS(u16)` — push a new class named by `consts[idx]`.
    Class,
    /// `METHOD(u16)` — attach TOS (a closure) as method `consts[idx]` on the
    /// class below it.
    Method,
    /// `GET_SUPER(u16)` — resolve super-method `consts[idx]`.
    GetSuper,
    /// `inst cls -- bool` — `inst instanceof cls`.
    InstanceOf,

    // ---- strings ----------------------------------------------------------
    /// `TEMPLATE(u16)` — pop `n` parts, concatenate into a string.
    Template,

    // ---- AScript-specific -------------------------------------------------
    /// Await the future on TOS.
    Await,
    /// Yield TOS from the current generator.
    Yield,
    /// Wrap the current frame as a generator object.
    MakeGenerator,
    /// `?` — propagate a `[value, err]` Result pair (early-return on error).
    Propagate,
    /// `!` — force-unwrap a `[value, err]` pair (recoverable panic on error).
    Unwrap,
    /// `IMPORT(u16)` — import the module named by `consts[idx]`.
    Import,

    // ---- iteration --------------------------------------------------------
    /// `iterable -- iterable` — validate that TOS is async-iterable for a
    /// `for await` loop and leave it in place (to be stashed in a slot and driven
    /// lazily by [`Op::IterNext`]). Mirrors the tree-walker's `exec_for_await`
    /// dispatch (`src/interp.rs`): a `Value::Generator` and a native stream handle
    /// (WebSocket `recv` / SSE `next`, via `native_stream_method`) are accepted;
    /// ANY OTHER value raises the Tier-2 panic `value of type {t} is not
    /// async-iterable` (`t` = `interp::type_name`) anchored at this op's span (the
    /// iterable expression's trivia-trimmed code span).
    GetIter,
    /// `iterable -- value done:bool` — drive one lazy `for await` step over the
    /// async-iterable on TOS, pushing the produced `value` (below) and a `done`
    /// boolean (on top). Mirrors `exec_for_await` exactly: a `Value::Generator` is
    /// driven by an (awaiting) `resume(nil)` — `Some(v) -> v,false`, `None ->
    /// nil,true`; a native stream calls its `recv`/`next` method for a `[value,
    /// err]` pair — a non-nil `err` is the Tier-2 panic `for await stream error:
    /// {msg}`, a `nil` value ends the stream (`nil,true`), else `value,false`. An
    /// async generator's body may `await` internally: `resume` drives the backing
    /// Fiber through `Op::Await` before producing the yielded value, so await+yield
    /// fuse transparently. The op is async (it awaits the step).
    IterNext,
    /// `iterable --` — close the async-iterable on TOS. Mirrors the tree-walker's
    /// `g.close()` on a `break`/early-`return` out of a `for await` over a
    /// generator (drops the backing Fiber / marks it done so it is reclaimed
    /// promptly); a no-op for a native stream handle (it is reclaimed at scope
    /// end). Emitted only on the `break`/`return` exits of a `for await` loop, never
    /// on natural exhaustion.
    IterClose,
    /// `iterable -- snapshot:array` — materialize the SYNC for-of snapshot.
    /// Mirrors the tree-walker's `Stmt::ForOf` (sync) `items` build exactly:
    /// an `Array` yields a *clone* of its current elements (so later mutation of
    /// the source array does not change the iteration), a `Str` yields its chars
    /// each as a 1-char string, and ANY OTHER value raises the Tier-2 panic
    /// `value of type {t} is not iterable` (`t` = `interp::type_name`) anchored at
    /// this op's span (the iterable expression's trivia-trimmed code span). Object/
    /// Map/Set are NOT iterable in sync for-of — they hit the "not iterable" panic,
    /// byte-identically to the tree-walker.
    IterSnapshot,
    /// `array -- len:number` — pop an `Array` and push its element count as a
    /// `Number`. Used to hoist a for-of snapshot's (fixed) length into a scratch
    /// slot once, so the loop condition re-tests `idx < len` without rebuilding it.
    /// The operand is always a compiler-produced snapshot array (never user input),
    /// so a non-array is a compiler bug, surfaced as a Tier-2 panic.
    ArrayLen,

    // ---- cell-backed locals (by-reference capture, V4-T3) -----------------
    /// `GET_LOCAL_CELL(u16)` — push the value held in the heap cell for local
    /// slot `n`. Emitted (instead of `GET_LOCAL`) for any slot the resolver
    /// marked as a *cell slot* (a captured local). The cell is an
    /// `Rc<RefCell<Value>>` allocated at frame entry so a closure that captured
    /// it by reference observes later mutation.
    GetLocalCell,
    /// `SET_LOCAL_CELL(u16)` — store TOS (popped) into the heap cell for local
    /// slot `n`. The cell-slot counterpart of `SET_LOCAL`.
    SetLocalCell,
    /// `FRESH_CELL(u16)` — install a BRAND-NEW heap cell (`Rc<RefCell<Value::Nil>>`)
    /// into the current frame's slot `n`, dropping the frame's strong ref to the
    /// PREVIOUS cell. Any closure that captured the previous cell keeps it alive (an
    /// `Rc` clone) with its own value, so that closure is unaffected. Emitted at the
    /// TOP of each loop iteration for every cell slot that must be fresh per
    /// iteration — the loop variable (for-range/for-of) and any captured `let`
    /// declared inside the loop body — so a closure created in iteration N captures
    /// THAT iteration's cell and observes only that iteration's value
    /// (per-iteration capture freshness, matching the tree-walker's fresh-binding-
    /// per-iteration semantics). `slot` is always a resolver cell slot, so the
    /// frame's `cells[slot]` exists; a non-cell slot would be a compiler bug.
    FreshCell,

    // ---- destructuring let (V10-T1) ---------------------------------------
    /// `src -- src` (peek-only) — verify the value on TOS is a `Value::Array`,
    /// else raise the Tier-2 panic `cannot destructure a non-array value of type
    /// {t}` (`t` = `interp::type_name`) at this op's span (the RHS expression's
    /// trivia-trimmed code span). Mirrors the tree-walker's `Stmt::LetDestructure`
    /// type check, which validates ONCE before binding any name. Leaves the source
    /// in place so the surrounding lowering can store it into a temp slot.
    CheckArrayDestructure,
    /// `src -- src` (peek-only) — verify the value on TOS is a `Value::Object` or
    /// `Value::Instance`, else raise the Tier-2 panic `cannot destructure a
    /// non-object value of type {t}` (`t` = `interp::type_name`) at this op's span.
    /// Mirrors the tree-walker's `Stmt::LetDestructureObject` type check. Leaves
    /// the source in place.
    CheckObjectDestructure,
    /// `ARRAY_ELEM(u16 index)` — `src -- src[index]`. Pop a `Value::Array` and push
    /// the element at `index`, or `nil` if the index is out of bounds (positions
    /// past the array's length bind nil, NOT an out-of-bounds panic — matching the
    /// tree-walker's `items.get(i).cloned().unwrap_or(Value::Nil)`). The source has
    /// already been validated as an array by `CheckArrayDestructure`; a non-array
    /// here is a compiler bug surfaced as a Tier-2 panic.
    ArrayElem,
    /// `OBJECT_KEY(u16 const)` — `src -- src[key]` where `key = consts[const]` (a
    /// `Str`). Pop a `Value::Object` or `Value::Instance` and push the value stored
    /// under `key`, or `nil` if the key is absent. Mirrors the tree-walker's
    /// destructure `get` closure EXACTLY: an Object reads its map entry, an Instance
    /// reads its `fields` entry (it does NOT fall back to methods, unlike
    /// `read_member`). The source has already been validated by
    /// `CheckObjectDestructure`; any other value here is a compiler bug.
    ObjectKey,
    /// `ARRAY_REST(u16 start)` — `src -- src[start..]`. Pop a `Value::Array` and
    /// push a NEW array of its elements from index `start` to the end (the trailing
    /// collector `...rest`), matching the tree-walker's
    /// `items.iter().skip(names.len())`. An empty tail yields an empty array. The
    /// source has already been validated as an array; a non-array is a compiler bug.
    ArrayRest,
    /// `OBJECT_REST(u16 const)` — `src -- leftover` where `consts[const]` is a
    /// `Value::Array` of `Str` keys that were explicitly bound. Pop a
    /// `Value::Object`/`Value::Instance` and push a NEW object of its entries whose
    /// key is NOT in the bound-keys set, preserving source order — matching the
    /// tree-walker's leftover-keys collection (`...rest` excludes the already-bound
    /// SOURCE keys). The source has already been validated; any other value is a
    /// compiler bug.
    ObjectRest,

    // ---- match pattern tests (V10-T3/T4) ----------------------------------
    /// `MATCH_ARRAY(u16 len, u8 exact)` — `subject -- ok:bool`. Pop the subject and
    /// push `true` iff it is a `Value::Array` whose length is exactly `len` (when
    /// `exact == 1`) or at least `len` (when `exact == 0`, i.e. the pattern has a
    /// `...rest`). Any non-array subject pushes `false`. Mirrors the tree-walker's
    /// `Pattern::Array` length/type guard (a non-array or wrong-length array is a
    /// structural mismatch, never a panic). The matched sub-elements are read
    /// separately by reloading the subject temp and applying `ARRAY_ELEM`/`ARRAY_REST`.
    MatchArray,
    /// `MATCH_OBJECT` — `subject -- ok:bool`. Pop the subject and push `true` iff it
    /// is a `Value::Object` or `Value::Instance`. Any other value pushes `false`.
    /// Mirrors the type guard at the head of the tree-walker's `Pattern::Object`.
    MatchObject,
    /// `MATCH_HAS_KEY(u16 const)` — `subject -- ok:bool`. Pop the subject (an
    /// Object/Instance per `MATCH_OBJECT`) and push `true` iff it has the field
    /// `consts[const]` (a `Str`). Mirrors the per-entry `fields.get(key)` presence
    /// check (a missing key is a structural mismatch). The subject is popped (not
    /// peeked) so a missing-key fail-jump leaves NO orphaned value on the stack; the
    /// matched-key path reloads the subject temp for the sub-value read.
    MatchHasKey,
    /// `MATCH_RANGE(u8 flags)` — `subject lo hi step -- ok:bool` (step on top).
    /// `flags` bit0 = inclusive, bit1 = step PRESENT. Pop the four operands and push
    /// `true` iff the subject is a `Value::Float` `n` matching the range, with `lo`
    /// and `hi` `Value::Float`s. With step OMITTED (a `nil` placeholder) this is
    /// the plain in-bounds test `n >= lo && (n <= hi if inclusive else n < hi)`
    /// (bounds-inferred direction). With step PRESENT it is strided membership
    /// (spec §3.7) anchored at `lo`, via the SHARED `interp::resolve_step` (validates
    /// → PANICS on zero/non-finite/mismatch, byte-identical to iteration) +
    /// `interp::range_pattern_contains`. Any non-number subject/bound pushes `false`
    /// (a non-panic mismatch). Mirrors the tree-walker's `Pattern::Range` EXACTLY.
    MatchRange,
    /// `MATCH_NO_ARM` — unconditionally raise the Tier-2 panic `no matching arm in
    /// match expression` at this op's span (the `MatchExpr`'s code span). Emitted at
    /// the fall-through end of a `match` when no arm matched, byte-identical to the
    /// tree-walker's `AsError::at("no matching arm in match expression", expr.span)`.
    MatchNoArm,

    // ---- module exports (V12-T4) ------------------------------------------
    /// `DEFINE_EXPORT(u16 const)` — `value -- ` (pops one). Record `consts[const]`
    /// (a `Str` export name) → the popped value into the VM's CURRENT module-export
    /// map. Emitted by `export <decl>` after the decl has bound its name into a
    /// local slot: the compiler pushes the bound value (`GET_LOCAL`/`GET_LOCAL_CELL`)
    /// then `DEFINE_EXPORT name`. Mirrors the tree-walker's `Stmt::Export`, which
    /// records the exported name into `Interp::current_exports`. When the top-level
    /// chunk is the entry program (not an imported module) the recorded exports are
    /// simply unused, exactly as the tree-walker discards a main program's exports.
    DefineExport,
}

impl Op {
    /// Decode an opcode byte. Returns `None` if `b` is not a valid discriminant.
    ///
    /// This match is exhaustive over every [`Op`] variant; adding a variant
    /// without a corresponding arm is a compile error.
    pub fn from_u8(b: u8) -> Option<Op> {
        use Op::*;
        // The wildcard arm only catches bytes outside the declared range; the
        // listed arms are exhaustive over the enum (compiler-verified by the
        // round-trip unit test).
        Some(match b {
            x if x == Const as u8 => Const,
            x if x == Nil as u8 => Nil,
            x if x == True as u8 => True,
            x if x == False as u8 => False,
            x if x == Pop as u8 => Pop,
            x if x == Dup as u8 => Dup,
            x if x == Swap as u8 => Swap,
            x if x == Rot3 as u8 => Rot3,

            x if x == GetLocal as u8 => GetLocal,
            x if x == SetLocal as u8 => SetLocal,
            x if x == GetUpvalue as u8 => GetUpvalue,
            x if x == SetUpvalue as u8 => SetUpvalue,
            x if x == CloseUpvalue as u8 => CloseUpvalue,

            x if x == GetGlobal as u8 => GetGlobal,
            x if x == DefineGlobal as u8 => DefineGlobal,
            x if x == SetGlobal as u8 => SetGlobal,
            x if x == ImmutableError as u8 => ImmutableError,

            x if x == Add as u8 => Add,
            x if x == Sub as u8 => Sub,
            x if x == Mul as u8 => Mul,
            x if x == Div as u8 => Div,
            x if x == Mod as u8 => Mod,
            x if x == Pow as u8 => Pow,
            x if x == Neg as u8 => Neg,
            x if x == Not as u8 => Not,
            x if x == Eq as u8 => Eq,
            x if x == Ne as u8 => Ne,
            x if x == Lt as u8 => Lt,
            x if x == Le as u8 => Le,
            x if x == Gt as u8 => Gt,
            x if x == Ge as u8 => Ge,
            x if x == Range as u8 => Range,
            x if x == RangeInclusive as u8 => RangeInclusive,
            x if x == CheckNumbers as u8 => CheckNumbers,
            x if x == RangeStepValue as u8 => RangeStepValue,
            x if x == RangeResolveStep as u8 => RangeResolveStep,
            x if x == RangeHasNext as u8 => RangeHasNext,

            x if x == Jump as u8 => Jump,
            x if x == JumpIfFalse as u8 => JumpIfFalse,
            x if x == JumpIfTrue as u8 => JumpIfTrue,
            x if x == JumpIfNotNil as u8 => JumpIfNotNil,
            x if x == Loop as u8 => Loop,
            x if x == JumpIfArgSupplied as u8 => JumpIfArgSupplied,
            x if x == CheckParam as u8 => CheckParam,

            x if x == Call as u8 => Call,
            x if x == CallMethod as u8 => CallMethod,
            x if x == CallMethodSpread as u8 => CallMethodSpread,
            x if x == Return as u8 => Return,
            x if x == Closure as u8 => Closure,

            x if x == NewArray as u8 => NewArray,
            x if x == NewObject as u8 => NewObject,
            x if x == NewMap as u8 => NewMap,
            x if x == MapEntry as u8 => MapEntry,
            x if x == Spread as u8 => Spread,
            x if x == SpreadArgs as u8 => SpreadArgs,
            x if x == AppendArray as u8 => AppendArray,
            x if x == AppendObject as u8 => AppendObject,
            x if x == SpreadObject as u8 => SpreadObject,
            x if x == CallSpread as u8 => CallSpread,
            x if x == GetIndex as u8 => GetIndex,
            x if x == SetIndex as u8 => SetIndex,
            x if x == GetProp as u8 => GetProp,
            x if x == SetProp as u8 => SetProp,
            x if x == GetPropOpt as u8 => GetPropOpt,

            x if x == Class as u8 => Class,
            x if x == Method as u8 => Method,
            x if x == GetSuper as u8 => GetSuper,
            x if x == InstanceOf as u8 => InstanceOf,

            x if x == Template as u8 => Template,

            x if x == Await as u8 => Await,
            x if x == Yield as u8 => Yield,
            x if x == MakeGenerator as u8 => MakeGenerator,
            x if x == Propagate as u8 => Propagate,
            x if x == Unwrap as u8 => Unwrap,
            x if x == Import as u8 => Import,

            x if x == GetIter as u8 => GetIter,
            x if x == IterNext as u8 => IterNext,
            x if x == IterClose as u8 => IterClose,
            x if x == IterSnapshot as u8 => IterSnapshot,
            x if x == ArrayLen as u8 => ArrayLen,

            x if x == GetLocalCell as u8 => GetLocalCell,
            x if x == SetLocalCell as u8 => SetLocalCell,
            x if x == FreshCell as u8 => FreshCell,

            x if x == CheckArrayDestructure as u8 => CheckArrayDestructure,
            x if x == CheckObjectDestructure as u8 => CheckObjectDestructure,
            x if x == ArrayElem as u8 => ArrayElem,
            x if x == ObjectKey as u8 => ObjectKey,
            x if x == ArrayRest as u8 => ArrayRest,
            x if x == ObjectRest as u8 => ObjectRest,

            x if x == MatchArray as u8 => MatchArray,
            x if x == MatchObject as u8 => MatchObject,
            x if x == MatchHasKey as u8 => MatchHasKey,
            x if x == MatchRange as u8 => MatchRange,
            x if x == MatchNoArm as u8 => MatchNoArm,

            x if x == DefineExport as u8 => DefineExport,

            _ => return None,
        })
    }

    /// Number of inline operand bytes that follow this opcode byte in the code
    /// stream. Does NOT include any inline cache slot (see [`Op::has_inline_cache`]).
    pub fn operand_width(self) -> usize {
        use Op::*;
        match self {
            // u16-operand ops.
            Const | GetLocal | SetLocal | GetLocalCell | SetLocalCell | FreshCell | GetUpvalue
            | SetUpvalue | CloseUpvalue | GetGlobal | SetGlobal | ImmutableError | Closure
            | NewArray | NewObject | GetProp | SetProp | GetPropOpt | Class | Method | GetSuper
            | Template | Import | ArrayElem | ObjectKey | ArrayRest | ObjectRest | MatchHasKey
            | CallMethodSpread | DefineExport | CheckParam => 2,

            // i16-operand (jump) ops.
            Jump | JumpIfFalse | JumpIfTrue | JumpIfNotNil | Loop => 2,

            // u8-operand ops.
            Call | MatchRange | RangeStepValue | RangeResolveStep | RangeHasNext => 1,

            // u16 + u8 operand op.
            // DEFINE_GLOBAL: u16 name-const index + u8 mutability flag (1 = `let`,
            // 0 = immutable `const`/`fn`/`class`/`enum`/`import`).
            CallMethod | MatchArray | DefineGlobal => 3,

            // u16 + i16 operand op.
            // JUMP_IF_ARG_SUPPLIED: u16 param-index + i16 forward jump offset.
            JumpIfArgSupplied => 4,

            // Zero-operand ops.
            Nil
            | True
            | False
            | Pop
            | Dup
            | Swap
            | Rot3
            | Add
            | Sub
            | Mul
            | Div
            | Mod
            | Pow
            | Neg
            | Not
            | Eq
            | Ne
            | Lt
            | Le
            | Gt
            | Ge
            | Range
            | RangeInclusive
            | CheckNumbers
            | Return
            | Spread
            | SpreadArgs
            | AppendArray
            | AppendObject
            | SpreadObject
            | CallSpread
            | GetIndex
            | SetIndex
            | InstanceOf
            | Await
            | Yield
            | MakeGenerator
            | Propagate
            | Unwrap
            | GetIter
            | IterNext
            | IterClose
            | IterSnapshot
            | ArrayLen
            | CheckArrayDestructure
            | CheckObjectDestructure
            | MatchObject
            | NewMap
            | MapEntry
            | MatchNoArm => 0,
        }
    }

    /// Whether this op is *specializable* via an inline cache.
    ///
    /// The cache state lives in a separate parallel array (built in V11), indexed
    /// by a slot reserved per call site at compile time — it is NOT encoded in the
    /// code stream, so [`Op::operand_width`] is unaffected. This predicate only
    /// marks which ops participate.
    pub fn has_inline_cache(self) -> bool {
        use Op::*;
        matches!(self, Add | GetGlobal | GetProp | SetProp | Call)
    }
}

#[cfg(test)]
mod tests {
    use super::Op;

    /// Every declared opcode, in one place, so the round-trip test is exhaustive.
    /// If you add an `Op` variant, add it here too (and the `from_u8` arm).
    const ALL: &[Op] = &[
        Op::Const,
        Op::Nil,
        Op::True,
        Op::False,
        Op::Pop,
        Op::Dup,
        Op::Swap,
        Op::Rot3,
        Op::GetLocal,
        Op::SetLocal,
        Op::GetUpvalue,
        Op::SetUpvalue,
        Op::CloseUpvalue,
        Op::GetGlobal,
        Op::DefineGlobal,
        Op::SetGlobal,
        Op::ImmutableError,
        Op::Add,
        Op::Sub,
        Op::Mul,
        Op::Div,
        Op::Mod,
        Op::Pow,
        Op::Neg,
        Op::Not,
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::Range,
        Op::RangeInclusive,
        Op::CheckNumbers,
        Op::RangeStepValue,
        Op::RangeResolveStep,
        Op::RangeHasNext,
        Op::Jump,
        Op::JumpIfFalse,
        Op::JumpIfTrue,
        Op::JumpIfNotNil,
        Op::Loop,
        Op::JumpIfArgSupplied,
        Op::CheckParam,
        Op::Call,
        Op::CallMethod,
        Op::CallMethodSpread,
        Op::Return,
        Op::Closure,
        Op::NewArray,
        Op::NewObject,
        Op::NewMap,
        Op::MapEntry,
        Op::Spread,
        Op::SpreadArgs,
        Op::AppendArray,
        Op::AppendObject,
        Op::SpreadObject,
        Op::CallSpread,
        Op::GetIndex,
        Op::SetIndex,
        Op::GetProp,
        Op::SetProp,
        Op::GetPropOpt,
        Op::Class,
        Op::Method,
        Op::GetSuper,
        Op::InstanceOf,
        Op::Template,
        Op::Await,
        Op::Yield,
        Op::MakeGenerator,
        Op::Propagate,
        Op::Unwrap,
        Op::Import,
        Op::GetIter,
        Op::IterNext,
        Op::IterClose,
        Op::IterSnapshot,
        Op::ArrayLen,
        Op::GetLocalCell,
        Op::SetLocalCell,
        Op::FreshCell,
        Op::CheckArrayDestructure,
        Op::CheckObjectDestructure,
        Op::ArrayElem,
        Op::ObjectKey,
        Op::ArrayRest,
        Op::ObjectRest,
        Op::MatchArray,
        Op::MatchObject,
        Op::MatchHasKey,
        Op::MatchRange,
        Op::MatchNoArm,
        Op::DefineExport,
    ];

    #[test]
    fn from_u8_round_trips_every_variant() {
        for &op in ALL {
            assert_eq!(
                Op::from_u8(op as u8),
                Some(op),
                "round-trip failed for {op:?}"
            );
        }
    }

    #[test]
    fn discriminants_are_unique_and_dense() {
        // ALL must list each variant exactly once: discriminants 0..ALL.len().
        for (i, &op) in ALL.iter().enumerate() {
            assert_eq!(op as usize, i, "discriminant gap/dup at {op:?}");
        }
    }

    #[test]
    fn from_u8_rejects_out_of_range() {
        assert_eq!(Op::from_u8(ALL.len() as u8), None);
        assert_eq!(Op::from_u8(u8::MAX), None);
    }

    #[test]
    fn operand_width_for_representative_ops() {
        // u16 operands.
        assert_eq!(Op::Const.operand_width(), 2);
        assert_eq!(Op::GetLocal.operand_width(), 2);
        assert_eq!(Op::GetGlobal.operand_width(), 2);
        assert_eq!(Op::Closure.operand_width(), 2);
        assert_eq!(Op::GetProp.operand_width(), 2);
        // i16 jump operands.
        assert_eq!(Op::Jump.operand_width(), 2);
        assert_eq!(Op::JumpIfFalse.operand_width(), 2);
        assert_eq!(Op::Loop.operand_width(), 2);
        // u8 operand.
        assert_eq!(Op::Call.operand_width(), 1);
        // zero-operand ops.
        assert_eq!(Op::Nil.operand_width(), 0);
        assert_eq!(Op::Add.operand_width(), 0);
        assert_eq!(Op::Return.operand_width(), 0);
        assert_eq!(Op::GetIndex.operand_width(), 0);
        assert_eq!(Op::Await.operand_width(), 0);
    }

    #[test]
    fn inline_cache_marks_only_specializable_ops() {
        assert!(Op::Add.has_inline_cache());
        assert!(Op::GetGlobal.has_inline_cache());
        assert!(Op::GetProp.has_inline_cache());
        assert!(Op::SetProp.has_inline_cache());
        assert!(Op::Call.has_inline_cache());
        // A sampling of non-specializable ops.
        assert!(!Op::Const.has_inline_cache());
        assert!(!Op::Sub.has_inline_cache());
        assert!(!Op::GetIndex.has_inline_cache());
        assert!(!Op::Jump.has_inline_cache());
    }
}
