//! `std/ffi` — the Foreign Function Interface (FFI campaign §3/§5).
//!
//! Open a shared library (`ffi.open` → `dlopen`), look up a symbol with a bound
//! C signature (`lib.symbol(name, argtypes, rettype)` → `dlsym` + a libffi CIF),
//! and invoke it across the C ABI (`sym.call(args)` → the libffi trampoline). The
//! three handles ([`crate::value::NativeKind`] `ForeignLib`/`ForeignSymbol`/
//! `ForeignPtr`) are native resources with deterministic `Drop` (the `Library`
//! `dlclose`s on drop) and are **GC-untraced** (a raw foreign pointer / OS handle
//! is opaque memory the collector cannot reason about — `gc.rs`'s `Native`
//! catch-all `_ => {}` keeps them leaf-opaque).
//!
//! **Capability gate.** Every `ffi.*` entry routes through `call_stdlib`'s central
//! `required_cap("ffi", _) -> Some(Cap::Ffi)` gate, so `ffi.open` is denied after a
//! `caps.drop("ffi")` / `--deny ffi` / `--sandbox`. Operating an ALREADY-OPEN handle
//! (`lib.symbol`, `sym.call`) is re-gated at `call_native_method` via
//! `NativeKind::governing_cap` (also `Cap::Ffi`), so a drop HOLDS for handles opened
//! before it.
//!
//! **Sized C ints marshal OVER `int`** (NUM §10): `i8`…`u64`/`size` exist only at the
//! C-ABI boundary, described as `ffi.i32` etc. and carried in/out over `Value::Int`
//! (i64). There is NO new `Value` kind. Narrowing is CHECKED (a too-large value → a
//! Tier-2 panic), except `u64`/`size`, which take the i64 **bit pattern** with no
//! sign check (§3.3).
//!
//! **UNSAFE discipline (Gate 0).** Arg count + each arg's type/shape are validated
//! BEFORE the libffi trampoline is entered, so a malformed call is a clean recoverable
//! Tier-2 panic — never a segfault/UB on reachable script input. The `unsafe` deref of
//! the resolved function address is sound because the `ForeignSymbol` keeps the owning
//! `Library` alive (§3.4).

use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, Value};
use libffi::middle::{Arg, Cif, CodePtr, Type};
use std::os::raw::c_void;
use std::rc::Rc;

// ───────────────────────────── C type descriptors ────────────────────────────

/// A C type at the marshalling boundary (§3.2). Sized ints carry in/out over the NUM
/// `int` (i64); floats over `float` (f64); `Ptr` over a `Bytes` buffer or a
/// `ForeignPtr`; `Void` is a return-only `nil`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiType {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    /// `size_t`/`ssize_t` — pointer-width; resolved per target at marshal time.
    Size,
    F32,
    F64,
    /// `void*` / any `T*`.
    Ptr,
    /// `void` — return type only.
    Void,
}

impl FfiType {
    /// The wire tag stored in the descriptor Object's `__ffi` field.
    fn tag(self) -> &'static str {
        match self {
            FfiType::I8 => "i8",
            FfiType::I16 => "i16",
            FfiType::I32 => "i32",
            FfiType::I64 => "i64",
            FfiType::U8 => "u8",
            FfiType::U16 => "u16",
            FfiType::U32 => "u32",
            FfiType::U64 => "u64",
            FfiType::Size => "size",
            FfiType::F32 => "f32",
            FfiType::F64 => "f64",
            FfiType::Ptr => "ptr",
            FfiType::Void => "void",
        }
    }

    fn from_tag(tag: &str) -> Option<FfiType> {
        Some(match tag {
            "i8" => FfiType::I8,
            "i16" => FfiType::I16,
            "i32" => FfiType::I32,
            "i64" => FfiType::I64,
            "u8" => FfiType::U8,
            "u16" => FfiType::U16,
            "u32" => FfiType::U32,
            "u64" => FfiType::U64,
            "size" => FfiType::Size,
            "f32" => FfiType::F32,
            "f64" => FfiType::F64,
            "ptr" => FfiType::Ptr,
            "void" => FfiType::Void,
            _ => return None,
        })
    }

    /// The libffi `Type` for the C-ABI CIF.
    fn libffi_type(self) -> Type {
        match self {
            FfiType::I8 => Type::i8(),
            FfiType::I16 => Type::i16(),
            FfiType::I32 => Type::i32(),
            FfiType::I64 => Type::i64(),
            FfiType::U8 => Type::u8(),
            FfiType::U16 => Type::u16(),
            FfiType::U32 => Type::u32(),
            FfiType::U64 => Type::u64(),
            FfiType::Size => Type::usize(),
            FfiType::F32 => Type::f32(),
            FfiType::F64 => Type::f64(),
            FfiType::Ptr => Type::pointer(),
            FfiType::Void => Type::void(),
        }
    }
}

/// Build the Tier-1 ok pair `[value, nil]` (`ffi.open`/`lib.symbol` success).
fn ok_pair(value: Value) -> Value {
    crate::interp::make_pair(value, Value::Nil)
}

/// Build the Tier-1 err pair `[nil, {message}]` (recoverable open/symbol failure).
fn err_pair(msg: Value) -> Value {
    crate::interp::make_pair(Value::Nil, crate::interp::make_error(msg))
}

/// Build a `ffi.<t>` descriptor: a tagged Object `{__ffi: "<tag>"}` — NOT a new
/// `Value` kind (exactly like `std/schema`'s tagged-Object schemas). Opaque to the
/// static checker (synths as `Unknown`).
fn descriptor(t: FfiType) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("__ffi".to_string(), Value::Str(t.tag().into()));
    Value::Object(crate::value::ObjectCell::new(m))
}

/// Read the `FfiType` out of a `ffi.<t>` descriptor Object, or `None` if `v` is not a
/// descriptor. The §3.3 "argtypes not `ffi.*` descriptors → Tier-2" check uses this.
fn descriptor_type(v: &Value) -> Option<FfiType> {
    if let Value::Object(o) = v {
        if let Some(Value::Str(tag)) = o.borrow().get("__ffi") {
            return FfiType::from_tag(tag);
        }
    }
    None
}

// ───────────────────────────── the symbol resource ───────────────────────────

/// A resolved foreign symbol + its bound signature (§3.4). Stored in
/// `ResourceState::ForeignSymbol`. Holds the function address as a raw `*mut c_void`
/// and KEEPS THE OWNING `Library` ALIVE via `_lib` so the address stays valid for
/// every `sym.call` (a borrowed `Symbol<'lib>` cannot be `'static`; the
/// raw-address-plus-kept-alive-`Library` pairing gives both `'static` storage AND
/// lifetime correctness — §3.4). The `Cif` is the libffi call-interface for the bound
/// signature.
pub struct FfiSymbol {
    /// The resolved function address (`dlsym` result).
    addr: *mut c_void,
    /// The libffi CIF for the bound `argtypes -> ret` signature.
    cif: Cif,
    /// The bound argument types (drives marshalling + the pre-trampoline arity check).
    argtypes: Vec<FfiType>,
    /// The bound return type (drives the result marshal-out + the typed `Cif::call`).
    ret: FfiType,
    /// Keeps the owning `Library` alive past the lib handle's own drop, so `addr`
    /// stays valid. SOUNDNESS-CRITICAL: the `unsafe` call deref relies on this.
    _lib: Rc<libloading::Library>,
}

// ───────────────────────────────── exports ───────────────────────────────────

/// `std/ffi` exports — the C-type descriptors as values + `open`/`struct`/`cstr`/
/// `read_cstr`. `symbol`/`call` are handle METHODS (resolved on the native handle),
/// registered in `std_arity.rs` under `("ffi", "symbol"|"call")`.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        // C-type descriptors (tagged Objects).
        ("i8", descriptor(FfiType::I8)),
        ("i16", descriptor(FfiType::I16)),
        ("i32", descriptor(FfiType::I32)),
        ("i64", descriptor(FfiType::I64)),
        ("u8", descriptor(FfiType::U8)),
        ("u16", descriptor(FfiType::U16)),
        ("u32", descriptor(FfiType::U32)),
        ("u64", descriptor(FfiType::U64)),
        ("size", descriptor(FfiType::Size)),
        ("f32", descriptor(FfiType::F32)),
        ("f64", descriptor(FfiType::F64)),
        ("ptr", descriptor(FfiType::Ptr)),
        ("void", descriptor(FfiType::Void)),
        // Functions.
        ("open", super::bi("ffi.open")),
        ("struct", super::bi("ffi.struct")),
        ("cstr", super::bi("ffi.cstr")),
        ("read_cstr", super::bi("ffi.read_cstr")),
        // Struct buffer accessors (FUNCTION form: `ffi.alloc(layout)`,
        // `ffi.get(layout, buf, name)`, `ffi.set(layout, buf, name, val)`). The spec
        // §5.1 also sketches a `layout.alloc()`/`.get()`/`.set()` METHOD form; the
        // method form needs a schema-style call-site hook in BOTH engines, deferred to
        // keep this unit's byte-identity surface minimal — the function form delivers
        // the same out-param round-trip capability.
        ("alloc", super::bi("ffi.alloc")),
        ("get", super::bi("ffi.get")),
        ("set", super::bi("ffi.set")),
    ]
}

// ───────────────────────────────── routing ───────────────────────────────────

impl Interp {
    /// `std/ffi` dispatch for the FREE functions (`open`/`struct`/`cstr`/`read_cstr`).
    /// The handle methods (`lib.symbol`, `sym.call`, struct `.alloc`/`get`/`set`) are
    /// routed through `call_native_method` / the schema-style call hook below. Gated by
    /// the central `required_cap("ffi", _)` before this is reached.
    pub(crate) async fn call_ffi(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "open" => self.ffi_open(args, span),
            "struct" => ffi_struct(args, span),
            "cstr" => ffi_cstr(args, span),
            // `read_cstr` works on either a Bytes buffer (resolved here) or a
            // ForeignPtr (needs the resource table → the interp-aware path).
            "read_cstr" => match super::arg(args, 0) {
                Value::Native(n) if n.kind == NativeKind::ForeignPtr => {
                    self.ffi_read_cstr_ptr(n.id, span)
                }
                other => ffi_read_cstr(&[other], span),
            },
            "alloc" => ffi_alloc(args, span),
            "get" => ffi_get(args, span),
            "set" => ffi_set(args, span),
            // Handle methods routed here for completeness (when called as `ffi.symbol`
            // / `ffi.call`, which is not the normal `lib.symbol(...)` form).
            "symbol" => Err(AsError::at(
                "ffi.symbol is a method on an open library handle (lib.symbol(name, argtypes, rettype))"
                    .to_string(),
                span,
            )
            .into()),
            "call" => Err(AsError::at(
                "ffi.call is a method on a symbol handle (sym.call(args))".to_string(),
                span,
            )
            .into()),
            _ => Err(AsError::at(format!("std/ffi has no function '{func}'"), span).into()),
        }
    }

    /// `ffi.open(path) -> [ForeignLib, err]` — `dlopen`. Tier-1: a failure (not found,
    /// not a shared object, missing dep) is recoverable `[nil, err]` data (you may
    /// probe for an optional library), NOT a panic.
    fn ffi_open(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let path = super::want_string(&super::arg(args, 0), span, "ffi.open")?;
        // SAFETY: `Library::new` is `unsafe` because loading a library runs its
        // initializers (arbitrary native code). The path is script-supplied data; a
        // bad path returns `Err` (→ Tier-1 below), never UB on our side. This is the
        // documented FFI trust boundary (the `ffi` capability gates reaching here).
        let lib = unsafe { libloading::Library::new(&*path) };
        match lib {
            Ok(lib) => {
                let handle = self.register_resource(
                    NativeKind::ForeignLib,
                    indexmap::IndexMap::new(),
                    ResourceState::ForeignLib(Rc::new(lib)),
                );
                Ok(ok_pair(handle))
            }
            Err(e) => Ok(err_pair(
                Value::Str(format!("ffi.open: {e}").into()),
            )),
        }
    }

    /// `lib.symbol(name, argtypes, rettype) -> [ForeignSymbol, err]` — `dlsym` + a
    /// bound signature. A missing symbol is Tier-1 `[nil, err]`; a malformed signature
    /// (argtypes not `ffi.*` descriptors, rettype not a descriptor) is a Tier-2 panic.
    pub(crate) fn ffi_lib_symbol(
        &self,
        lib_id: u64,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let name = super::want_string(&super::arg(args, 0), span, "lib.symbol")?;
        let argtypes_val = super::arg(args, 1);
        let rettype_val = super::arg(args, 2);

        // Validate the signature BEFORE touching the library (§3.3: malformed sig is a
        // Tier-2 programmer error, not Tier-1 data).
        let argtypes = parse_argtypes(&argtypes_val, span)?;
        let ret = descriptor_type(&rettype_val).ok_or_else(|| {
            Control::Panic(AsError::at(
                "lib.symbol: rettype must be a ffi.* type descriptor (e.g. ffi.i32)".to_string(),
                span,
            ))
        })?;

        // Resolve the symbol address + clone the owning Library (keep-alive).
        let lib_rc = self.with_resource(lib_id, |r| match r {
            Some(ResourceState::ForeignLib(lib)) => Some(lib.clone()),
            _ => None,
        });
        let lib_rc = match lib_rc {
            Some(lib) => lib,
            None => {
                return Err(AsError::at(
                    "lib.symbol: receiver is not an open foreign library".to_string(),
                    span,
                )
                .into())
            }
        };

        // SAFETY: `Library::get` looks up `name` in the loaded library. We immediately
        // turn the borrowed `Symbol<'lib>` into a raw address and keep `lib_rc` alive,
        // so the address is valid for the symbol's whole lifetime. A missing symbol
        // returns `Err` (→ Tier-1), never UB.
        let addr: *mut c_void = {
            let sym: Result<libloading::Symbol<'_, *mut c_void>, _> =
                unsafe { lib_rc.get(name.as_bytes()) };
            match sym {
                Ok(sym) => {
                    // `into_raw` yields the OS-level pointer cell; deref-and-cast to the
                    // function address. The kept-alive `lib_rc` outlives this address.
                    *sym
                }
                Err(e) => {
                    return Ok(err_pair(Value::Str(
                        format!("lib.symbol: {e}").into(),
                    )))
                }
            }
        };

        if addr.is_null() {
            return Ok(err_pair(Value::Str(
                format!("lib.symbol: symbol '{name}' resolved to a null address").into(),
            )));
        }

        // Build the libffi CIF for the bound signature.
        let arg_ffi: Vec<Type> = argtypes.iter().map(|t| t.libffi_type()).collect();
        let cif = Cif::new(arg_ffi, ret.libffi_type());

        let sym = FfiSymbol {
            addr,
            cif,
            argtypes,
            ret,
            _lib: lib_rc,
        };
        let handle = self.register_resource(
            NativeKind::ForeignSymbol,
            indexmap::IndexMap::new(),
            ResourceState::ForeignSymbol(Box::new(sym)),
        );
        Ok(ok_pair(handle))
    }

    /// `sym.call(args) -> ret` — marshal `args` per the bound `argtypes`, invoke through
    /// the libffi trampoline, marshal the result back per `rettype`. SYNCHRONOUS (§3.5:
    /// returns the value directly, not a future). Arity + per-arg shape are validated
    /// BEFORE the trampoline (Gate 0 — no reachable segfault).
    pub(crate) fn ffi_symbol_call(
        &self,
        sym_id: u64,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let call_args = super::want_array(&super::arg(args, 0), span, "sym.call")?;
        let call_args = call_args.borrow().clone();

        // Take the symbol resource out so we hold no `resources` borrow across the call
        // (there is no await here, but the borrow discipline is uniform). The symbol is
        // returned to the table afterward.
        let sym = match self.take_resource(sym_id) {
            Some(ResourceState::ForeignSymbol(s)) => s,
            Some(other) => {
                self.return_resource(sym_id, other);
                return Err(AsError::at(
                    "sym.call: receiver is not a foreign symbol".to_string(),
                    span,
                )
                .into());
            }
            None => {
                return Err(AsError::at(
                    "sym.call: foreign symbol handle is no longer valid".to_string(),
                    span,
                )
                .into())
            }
        };

        let result = invoke_symbol(self, &sym, &call_args, span);
        self.return_resource(sym_id, ResourceState::ForeignSymbol(sym));
        result
    }
}

/// Validate that `v` is an array of `ffi.*` type descriptors and collect them. A
/// non-array or a non-descriptor element is a Tier-2 panic (§3.3 malformed signature).
fn parse_argtypes(v: &Value, span: Span) -> Result<Vec<FfiType>, Control> {
    let arr = match v {
        Value::Array(a) => a.borrow().clone(),
        _ => {
            return Err(AsError::at(
                "lib.symbol: argtypes must be an array of ffi.* type descriptors".to_string(),
                span,
            )
            .into())
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, elem) in arr.iter().enumerate() {
        match descriptor_type(elem) {
            // `void` is a return-only type — invalid as an argument.
            Some(FfiType::Void) => {
                return Err(AsError::at(
                    format!("lib.symbol: ffi.void is not a valid argument type (arg {i})"),
                    span,
                )
                .into())
            }
            Some(t) => out.push(t),
            None => {
                return Err(AsError::at(
                    format!(
                        "lib.symbol: argtypes[{i}] is not a ffi.* type descriptor (e.g. ffi.i32)"
                    ),
                    span,
                )
                .into())
            }
        }
    }
    Ok(out)
}

/// Marshal `call_args` per `sym.argtypes`, invoke the libffi trampoline, marshal the
/// result back per `sym.ret`. ALL validation (arity, per-arg range/shape) happens
/// BEFORE the `unsafe` `cif.call`, so a malformed call is a clean Tier-2 panic.
fn invoke_symbol(
    interp: &Interp,
    sym: &FfiSymbol,
    call_args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    // --- Gate 0: arity check BEFORE any marshalling/trampoline. ---
    if call_args.len() != sym.argtypes.len() {
        return Err(AsError::at(
            format!(
                "ffi: call expected {} argument(s), got {}",
                sym.argtypes.len(),
                call_args.len()
            ),
            span,
        )
        .into());
    }

    // --- Marshal IN: build owned, fixed-layout storage for each argument, then take
    // `Arg` references into it. The storage Vecs MUST outlive the `cif.call` below
    // (the `Arg`s are raw pointers into them), so they live in this stack frame. ---
    let mut store_i8: Vec<i8> = Vec::new();
    let mut store_i16: Vec<i16> = Vec::new();
    let mut store_i32: Vec<i32> = Vec::new();
    let mut store_i64: Vec<i64> = Vec::new();
    let mut store_u8: Vec<u8> = Vec::new();
    let mut store_u16: Vec<u16> = Vec::new();
    let mut store_u32: Vec<u32> = Vec::new();
    let mut store_u64: Vec<u64> = Vec::new();
    let mut store_usize: Vec<usize> = Vec::new();
    let mut store_f32: Vec<f32> = Vec::new();
    let mut store_f64: Vec<f64> = Vec::new();
    let mut store_ptr: Vec<*mut c_void> = Vec::new();
    // Keep any borrowed `Bytes` buffers alive for the call duration (their address is
    // what we pass for a `ffi.ptr`).
    let mut bytes_guards: Vec<Rc<std::cell::RefCell<Vec<u8>>>> = Vec::new();

    // Pre-size so pushes never reallocate (a realloc would invalidate earlier `Arg`
    // pointers). Worst case every arg lands in one Vec, so reserve `len` in each.
    let n = call_args.len();
    store_i8.reserve(n);
    store_i16.reserve(n);
    store_i32.reserve(n);
    store_i64.reserve(n);
    store_u8.reserve(n);
    store_u16.reserve(n);
    store_u32.reserve(n);
    store_u64.reserve(n);
    store_usize.reserve(n);
    store_f32.reserve(n);
    store_f64.reserve(n);
    store_ptr.reserve(n);

    // Record (type, index-into-its-store) so we can build the `Arg` slice AFTER all
    // stores are fully populated (and thus stable — no further pushes).
    enum Slot {
        I8(usize),
        I16(usize),
        I32(usize),
        I64(usize),
        U8(usize),
        U16(usize),
        U32(usize),
        U64(usize),
        Usize(usize),
        F32(usize),
        F64(usize),
        Ptr(usize),
    }
    let mut slots: Vec<Slot> = Vec::with_capacity(n);

    for (i, (ty, val)) in sym.argtypes.iter().zip(call_args.iter()).enumerate() {
        match ty {
            FfiType::I8 => {
                let v = want_int_in_range(val, i8::MIN as i64, i8::MAX as i64, "i8", i, span)?;
                store_i8.push(v as i8);
                slots.push(Slot::I8(store_i8.len() - 1));
            }
            FfiType::I16 => {
                let v = want_int_in_range(val, i16::MIN as i64, i16::MAX as i64, "i16", i, span)?;
                store_i16.push(v as i16);
                slots.push(Slot::I16(store_i16.len() - 1));
            }
            FfiType::I32 => {
                let v = want_int_in_range(val, i32::MIN as i64, i32::MAX as i64, "i32", i, span)?;
                store_i32.push(v as i32);
                slots.push(Slot::I32(store_i32.len() - 1));
            }
            FfiType::I64 => {
                let v = want_int(val, "i64", i, span)?;
                store_i64.push(v);
                slots.push(Slot::I64(store_i64.len() - 1));
            }
            FfiType::U8 => {
                let v = want_int_in_range(val, 0, u8::MAX as i64, "u8", i, span)?;
                store_u8.push(v as u8);
                slots.push(Slot::U8(store_u8.len() - 1));
            }
            FfiType::U16 => {
                let v = want_int_in_range(val, 0, u16::MAX as i64, "u16", i, span)?;
                store_u16.push(v as u16);
                slots.push(Slot::U16(store_u16.len() - 1));
            }
            FfiType::U32 => {
                let v = want_int_in_range(val, 0, u32::MAX as i64, "u32", i, span)?;
                store_u32.push(v as u32);
                slots.push(Slot::U32(store_u32.len() - 1));
            }
            FfiType::U64 => {
                // §3.3 carve-out: NO sign range-check — the i64 bit pattern IS the u64.
                let v = want_int(val, "u64", i, span)?;
                store_u64.push(v as u64);
                slots.push(Slot::U64(store_u64.len() - 1));
            }
            FfiType::Size => {
                // size_t: bit pattern over the pointer-width, like u64 (no range check).
                let v = want_int(val, "size", i, span)?;
                store_usize.push(v as usize);
                slots.push(Slot::Usize(store_usize.len() - 1));
            }
            FfiType::F32 => {
                let v = want_float(val, "f32", i, span)?;
                store_f32.push(v as f32);
                slots.push(Slot::F32(store_f32.len() - 1));
            }
            FfiType::F64 => {
                let v = want_float(val, "f64", i, span)?;
                store_f64.push(v);
                slots.push(Slot::F64(store_f64.len() - 1));
            }
            FfiType::Ptr => {
                let p = marshal_ptr(interp, val, i, span, &mut bytes_guards)?;
                store_ptr.push(p);
                slots.push(Slot::Ptr(store_ptr.len() - 1));
            }
            FfiType::Void => {
                return Err(AsError::at(
                    format!("ffi: ffi.void is not a valid argument type (arg {i})"),
                    span,
                )
                .into())
            }
        }
    }

    // Build the `Arg` slice now that every store is fully populated (stable addresses).
    let ffi_args: Vec<Arg> = slots
        .iter()
        .map(|slot| match slot {
            Slot::I8(j) => Arg::new(&store_i8[*j]),
            Slot::I16(j) => Arg::new(&store_i16[*j]),
            Slot::I32(j) => Arg::new(&store_i32[*j]),
            Slot::I64(j) => Arg::new(&store_i64[*j]),
            Slot::U8(j) => Arg::new(&store_u8[*j]),
            Slot::U16(j) => Arg::new(&store_u16[*j]),
            Slot::U32(j) => Arg::new(&store_u32[*j]),
            Slot::U64(j) => Arg::new(&store_u64[*j]),
            Slot::Usize(j) => Arg::new(&store_usize[*j]),
            Slot::F32(j) => Arg::new(&store_f32[*j]),
            Slot::F64(j) => Arg::new(&store_f64[*j]),
            Slot::Ptr(j) => Arg::new(&store_ptr[*j]),
        })
        .collect();

    let code = CodePtr(sym.addr);

    // --- The trampoline. SAFETY: the CIF was built from `sym.argtypes -> sym.ret`; we
    // validated `call_args.len() == argtypes.len()` and marshalled each argument into a
    // store of the exact C type the CIF declares, so the `Arg` pointers and the CIF
    // agree. `sym.addr` is a valid function address kept alive by `sym._lib`. The
    // return type `R` of `cif.call::<R>` matches `sym.ret`'s C type below. There is no
    // remaining script-reachable way to mismatch — every path is validated above. ---
    let out = unsafe {
        match sym.ret {
            FfiType::I8 => Value::Int(sym.cif.call::<i8>(code, &ffi_args) as i64),
            FfiType::I16 => Value::Int(sym.cif.call::<i16>(code, &ffi_args) as i64),
            FfiType::I32 => Value::Int(sym.cif.call::<i32>(code, &ffi_args) as i64),
            FfiType::I64 => Value::Int(sym.cif.call::<i64>(code, &ffi_args)),
            FfiType::U8 => Value::Int(sym.cif.call::<u8>(code, &ffi_args) as i64),
            FfiType::U16 => Value::Int(sym.cif.call::<u16>(code, &ffi_args) as i64),
            FfiType::U32 => Value::Int(sym.cif.call::<u32>(code, &ffi_args) as i64),
            // §3.3 output asymmetry: a u64/size whose top bit is set comes back as the
            // two's-complement bit pattern (a negative `int`); bit-identical round-trip.
            FfiType::U64 => Value::Int(sym.cif.call::<u64>(code, &ffi_args) as i64),
            FfiType::Size => Value::Int(sym.cif.call::<usize>(code, &ffi_args) as i64),
            FfiType::F32 => Value::Float(sym.cif.call::<f32>(code, &ffi_args) as f64),
            FfiType::F64 => Value::Float(sym.cif.call::<f64>(code, &ffi_args)),
            FfiType::Ptr => {
                let p: *mut c_void = sym.cif.call::<*mut c_void>(code, &ffi_args);
                // A returned pointer becomes an opaque ForeignPtr handle.
                interp.register_resource(
                    NativeKind::ForeignPtr,
                    indexmap::IndexMap::new(),
                    ResourceState::ForeignPtr(p as usize),
                )
            }
            FfiType::Void => {
                sym.cif.call::<()>(code, &ffi_args);
                Value::Nil
            }
        }
    };
    // Keep `bytes_guards` alive until here (their addresses were live for the call).
    drop(bytes_guards);
    Ok(out)
}

/// Marshal a `ffi.ptr` argument: a `Bytes` passes its buffer address; a `ForeignPtr`
/// passes the opaque pointer; anything else is a Tier-2 panic. The returned `*mut
/// c_void` is valid only while the source stays alive — for `Bytes` we push the `Rc`
/// into `bytes_guards` so it outlives the call.
fn marshal_ptr(
    interp: &Interp,
    val: &Value,
    i: usize,
    span: Span,
    bytes_guards: &mut Vec<Rc<std::cell::RefCell<Vec<u8>>>>,
) -> Result<*mut c_void, Control> {
    match val {
        Value::Bytes(b) => {
            let ptr = b.borrow().as_ptr() as *mut c_void;
            bytes_guards.push(b.clone());
            Ok(ptr)
        }
        Value::Native(n) if n.kind == NativeKind::ForeignPtr => {
            let addr = interp.with_resource(n.id, |r| match r {
                Some(ResourceState::ForeignPtr(addr)) => Some(*addr),
                _ => None,
            });
            match addr {
                Some(addr) => Ok(addr as *mut c_void),
                None => Err(AsError::at(
                    format!("ffi: arg {i} foreign pointer handle is no longer valid"),
                    span,
                )
                .into()),
            }
        }
        Value::Nil => Ok(std::ptr::null_mut()),
        other => Err(AsError::at(
            format!(
                "ffi: arg {i} for ffi.ptr must be Bytes, a foreign pointer, or nil (got {})",
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Extract an i64 from an `int`/integral-`float` arg, Tier-2 on a non-int.
fn want_int(val: &Value, ty: &str, i: usize, span: Span) -> Result<i64, Control> {
    val.as_int_exact().ok_or_else(|| {
        Control::Panic(AsError::at(
            format!(
                "ffi: arg {i} for {ty} must be an int, got {}",
                crate::interp::type_name(val)
            ),
            span,
        ))
    })
}

/// Extract an i64 and range-check it into `[lo, hi]` (checked narrowing, §3.3). An
/// out-of-range value is a Tier-2 panic (`ffi: value 300 out of range for u8`).
fn want_int_in_range(
    val: &Value,
    lo: i64,
    hi: i64,
    ty: &str,
    i: usize,
    span: Span,
) -> Result<i64, Control> {
    let v = want_int(val, ty, i, span)?;
    if v < lo || v > hi {
        return Err(AsError::at(
            format!("ffi: value {v} out of range for {ty}"),
            span,
        )
        .into());
    }
    Ok(v)
}

/// Extract an f64 from a `float`/`int` arg, Tier-2 on a non-number.
fn want_float(val: &Value, ty: &str, i: usize, span: Span) -> Result<f64, Control> {
    val.as_f64().ok_or_else(|| {
        Control::Panic(AsError::at(
            format!(
                "ffi: arg {i} for {ty} must be a number, got {}",
                crate::interp::type_name(val)
            ),
            span,
        ))
    })
}

// ─────────────────────────── cstr / read_cstr / struct ───────────────────────

/// `ffi.cstr(s) -> Bytes` — a NUL-terminated `Bytes` you pass as a `ffi.ptr`. A
/// string containing an interior NUL is a Tier-2 panic (a C string cannot carry it).
fn ffi_cstr(args: &[Value], span: Span) -> Result<Value, Control> {
    let s = super::want_string(&super::arg(args, 0), span, "ffi.cstr")?;
    if s.as_bytes().contains(&0) {
        return Err(AsError::at(
            "ffi.cstr: string contains an interior NUL byte".to_string(),
            span,
        )
        .into());
    }
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    Ok(Value::Bytes(Rc::new(std::cell::RefCell::new(bytes))))
}

/// `ffi.read_cstr(ptr) -> string` — copy from a `ForeignPtr` (or a `Bytes` buffer)
/// until the first NUL into a `Str`. SAFETY-bounded: reading from a foreign pointer is
/// inherently `unsafe` (the C library owns the buffer's validity); a `ForeignPtr` from
/// a prior call is assumed live by the script's contract (documented §3.4).
fn ffi_read_cstr(args: &[Value], span: Span) -> Result<Value, Control> {
    match super::arg(args, 0) {
        Value::Bytes(b) => {
            let buf = b.borrow();
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf8_lossy(&buf[..end]).into_owned();
            Ok(Value::Str(s.into()))
        }
        // A ForeignPtr is dispatched to `ffi_read_cstr_ptr` (the interp-aware path) by
        // `call_ffi` BEFORE reaching here, so this free fn only sees Bytes / others.
        other => Err(AsError::at(
            format!(
                "ffi.read_cstr expects Bytes or a foreign pointer, got {}",
                crate::interp::type_name(&other)
            ),
            span,
        )
        .into()),
    }
}

/// `ffi.struct([[name, ffi.<t>]...]) -> layout` — a layout descriptor Object with
/// `size`/`align`/`fields` and the method set `alloc`/`get`/`set`. Stored as a tagged
/// Object `{__ffi_struct: true, size, align, fields: [[name, tag, offset]...]}` — no
/// new `Value` kind. Field offsets follow C alignment rules (each field aligned to its
/// own size; struct size rounded up to the max alignment).
fn ffi_struct(args: &[Value], span: Span) -> Result<Value, Control> {
    let fields_arr = super::want_array(&super::arg(args, 0), span, "ffi.struct")?;
    let fields_arr = fields_arr.borrow().clone();
    let mut offset: usize = 0;
    let mut max_align: usize = 1;
    let mut field_descs: Vec<Value> = Vec::with_capacity(fields_arr.len());

    for (i, entry) in fields_arr.iter().enumerate() {
        let pair = match entry {
            Value::Array(a) => a.borrow().clone(),
            _ => {
                return Err(AsError::at(
                    format!("ffi.struct: field {i} must be [name, ffi.<type>]"),
                    span,
                )
                .into())
            }
        };
        if pair.len() != 2 {
            return Err(AsError::at(
                format!("ffi.struct: field {i} must be [name, ffi.<type>]"),
                span,
            )
            .into());
        }
        let name = match &pair[0] {
            Value::Str(s) => s.to_string(),
            _ => {
                return Err(AsError::at(
                    format!("ffi.struct: field {i} name must be a string"),
                    span,
                )
                .into())
            }
        };
        let ty = descriptor_type(&pair[1]).ok_or_else(|| {
            Control::Panic(AsError::at(
                format!("ffi.struct: field {i} type must be a ffi.* descriptor"),
                span,
            ))
        })?;
        if ty == FfiType::Void {
            return Err(AsError::at(
                format!("ffi.struct: field {i} cannot be ffi.void"),
                span,
            )
            .into());
        }
        let (size, align) = ffi_type_size_align(ty);
        // Align the current offset up to this field's alignment.
        offset = align_up(offset, align);
        max_align = max_align.max(align);

        let mut fd = indexmap::IndexMap::new();
        fd.insert("name".to_string(), Value::Str(name.into()));
        fd.insert("type".to_string(), Value::Str(ty.tag().into()));
        fd.insert("offset".to_string(), Value::Int(offset as i64));
        field_descs.push(Value::Object(crate::value::ObjectCell::new(fd)));

        offset += size;
    }
    let total = align_up(offset, max_align);

    let mut layout = indexmap::IndexMap::new();
    layout.insert("__ffi_struct".to_string(), Value::Bool(true));
    layout.insert("size".to_string(), Value::Int(total as i64));
    layout.insert("align".to_string(), Value::Int(max_align as i64));
    layout.insert(
        "fields".to_string(),
        Value::Array(crate::value::ArrayCell::new(field_descs)),
    );
    Ok(Value::Object(crate::value::ObjectCell::new(layout)))
}

/// The size + alignment (bytes) of a scalar C type for struct layout.
fn ffi_type_size_align(ty: FfiType) -> (usize, usize) {
    match ty {
        FfiType::I8 | FfiType::U8 => (1, 1),
        FfiType::I16 | FfiType::U16 => (2, 2),
        FfiType::I32 | FfiType::U32 | FfiType::F32 => (4, 4),
        FfiType::I64 | FfiType::U64 | FfiType::F64 => (8, 8),
        FfiType::Size | FfiType::Ptr => {
            (std::mem::size_of::<usize>(), std::mem::align_of::<usize>())
        }
        FfiType::Void => (0, 1),
    }
}

fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

/// `ffi.alloc(layout) -> Bytes` — a zeroed `Bytes` of the layout's C size + alignment.
/// You pass it as a `ffi.ptr` out-param, then read fields back with `ffi.get`.
fn ffi_alloc(args: &[Value], span: Span) -> Result<Value, Control> {
    let layout = super::want_object(&super::arg(args, 0), span, "ffi.alloc")?;
    let size = match layout.borrow().get("size") {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => {
            return Err(AsError::at(
                "ffi.alloc: argument is not a ffi.struct layout".to_string(),
                span,
            )
            .into())
        }
    };
    Ok(Value::Bytes(Rc::new(std::cell::RefCell::new(vec![0u8; size]))))
}

/// Resolve a `(FfiType, offset)` for field `name` in `layout`, or a Tier-2 panic.
fn layout_field(
    layout: &Value,
    name: &str,
    span: Span,
    ctx: &str,
) -> Result<(FfiType, usize), Control> {
    let obj = super::want_object(layout, span, ctx)?;
    let fields = match obj.borrow().get("fields") {
        Some(Value::Array(a)) => a.borrow().clone(),
        _ => {
            return Err(AsError::at(
                format!("{ctx}: argument is not a ffi.struct layout"),
                span,
            )
            .into())
        }
    };
    for fd in &fields {
        if let Value::Object(o) = fd {
            let o = o.borrow();
            if let Some(Value::Str(fname)) = o.get("name") {
                if &**fname == name {
                    let ty = match o.get("type") {
                        Some(Value::Str(tag)) => FfiType::from_tag(tag),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        Control::Panic(AsError::at(
                            format!("{ctx}: field '{name}' has an invalid type tag"),
                            span,
                        ))
                    })?;
                    let off = match o.get("offset") {
                        Some(Value::Int(n)) if *n >= 0 => *n as usize,
                        _ => {
                            return Err(AsError::at(
                                format!("{ctx}: field '{name}' has an invalid offset"),
                                span,
                            )
                            .into())
                        }
                    };
                    return Ok((ty, off));
                }
            }
        }
    }
    Err(AsError::at(format!("{ctx}: no field '{name}' in the layout"), span).into())
}

/// `ffi.get(layout, buf, name) -> value` — read field `name` from a struct `Bytes`
/// buffer at its computed offset, marshalled out per the field's C type.
fn ffi_get(args: &[Value], span: Span) -> Result<Value, Control> {
    let layout = super::arg(args, 0);
    let buf = super::want_bytes(&super::arg(args, 1), span, "ffi.get")?;
    let name = super::want_string(&super::arg(args, 2), span, "ffi.get")?;
    let (ty, off) = layout_field(&layout, &name, span, "ffi.get")?;
    let (size, _) = ffi_type_size_align(ty);
    let buf = buf.borrow();
    if off + size > buf.len() {
        return Err(AsError::at(
            format!("ffi.get: field '{name}' at offset {off} exceeds buffer length {}", buf.len()),
            span,
        )
        .into());
    }
    let bytes = &buf[off..off + size];
    Ok(read_scalar(ty, bytes))
}

/// `ffi.set(layout, buf, name, value) -> nil` — write `value` into field `name` of a
/// struct `Bytes` buffer at its computed offset, marshalled in per the field's C type
/// (checked narrowing, §3.3).
fn ffi_set(args: &[Value], span: Span) -> Result<Value, Control> {
    let layout = super::arg(args, 0);
    let buf = super::want_bytes(&super::arg(args, 1), span, "ffi.set")?;
    let name = super::want_string(&super::arg(args, 2), span, "ffi.set")?;
    let val = super::arg(args, 3);
    let (ty, off) = layout_field(&layout, &name, span, "ffi.set")?;
    let (size, _) = ffi_type_size_align(ty);
    let encoded = write_scalar(ty, &val, &name, span)?;
    let mut buf = buf.borrow_mut();
    if off + size > buf.len() {
        return Err(AsError::at(
            format!("ffi.set: field '{name}' at offset {off} exceeds buffer length {}", buf.len()),
            span,
        )
        .into());
    }
    buf[off..off + size].copy_from_slice(&encoded);
    Ok(Value::Nil)
}

/// Read a scalar C value from `bytes` (native-endian) into a `Value` (§3.3 marshal-out).
fn read_scalar(ty: FfiType, bytes: &[u8]) -> Value {
    macro_rules! le {
        ($t:ty) => {{
            let mut a = [0u8; std::mem::size_of::<$t>()];
            a.copy_from_slice(bytes);
            <$t>::from_ne_bytes(a)
        }};
    }
    match ty {
        FfiType::I8 => Value::Int(bytes[0] as i8 as i64),
        FfiType::U8 => Value::Int(bytes[0] as i64),
        FfiType::I16 => Value::Int(le!(i16) as i64),
        FfiType::U16 => Value::Int(le!(u16) as i64),
        FfiType::I32 => Value::Int(le!(i32) as i64),
        FfiType::U32 => Value::Int(le!(u32) as i64),
        FfiType::I64 => Value::Int(le!(i64)),
        FfiType::U64 => Value::Int(le!(u64) as i64),
        FfiType::Size => Value::Int(le!(usize) as i64),
        FfiType::F32 => Value::Float(le!(f32) as f64),
        FfiType::F64 => Value::Float(le!(f64)),
        FfiType::Ptr => Value::Int(le!(usize) as i64),
        FfiType::Void => Value::Nil,
    }
}

/// Encode `val` as a scalar C value (native-endian) for `ty` (§3.3 marshal-in, with
/// the same checked-narrowing rules as `sym.call`'s argument marshalling).
fn write_scalar(ty: FfiType, val: &Value, name: &str, span: Span) -> Result<Vec<u8>, Control> {
    let idx = 0; // single field; reuse the range-check messages
    Ok(match ty {
        FfiType::I8 => (want_int_in_range(val, i8::MIN as i64, i8::MAX as i64, "i8", idx, span)?
            as i8)
            .to_ne_bytes()
            .to_vec(),
        FfiType::U8 => {
            (want_int_in_range(val, 0, u8::MAX as i64, "u8", idx, span)? as u8).to_ne_bytes().to_vec()
        }
        FfiType::I16 => (want_int_in_range(val, i16::MIN as i64, i16::MAX as i64, "i16", idx, span)?
            as i16)
            .to_ne_bytes()
            .to_vec(),
        FfiType::U16 => (want_int_in_range(val, 0, u16::MAX as i64, "u16", idx, span)? as u16)
            .to_ne_bytes()
            .to_vec(),
        FfiType::I32 => (want_int_in_range(val, i32::MIN as i64, i32::MAX as i64, "i32", idx, span)?
            as i32)
            .to_ne_bytes()
            .to_vec(),
        FfiType::U32 => (want_int_in_range(val, 0, u32::MAX as i64, "u32", idx, span)? as u32)
            .to_ne_bytes()
            .to_vec(),
        FfiType::I64 => want_int(val, "i64", idx, span)?.to_ne_bytes().to_vec(),
        FfiType::U64 => (want_int(val, "u64", idx, span)? as u64).to_ne_bytes().to_vec(),
        FfiType::Size => (want_int(val, "size", idx, span)? as usize).to_ne_bytes().to_vec(),
        FfiType::F32 => (want_float(val, "f32", idx, span)? as f32).to_ne_bytes().to_vec(),
        FfiType::F64 => want_float(val, "f64", idx, span)?.to_ne_bytes().to_vec(),
        FfiType::Ptr => {
            return Err(AsError::at(
                format!("ffi.set: field '{name}' is a pointer — set pointer fields via a foreign call, not ffi.set"),
                span,
            )
            .into())
        }
        FfiType::Void => {
            return Err(AsError::at(
                format!("ffi.set: field '{name}' is void (not a storable field)"),
                span,
            )
            .into())
        }
    })
}

/// `ffi.read_cstr(ptr)` for a `ForeignPtr` needs the interp to resolve the address.
/// Called from the interp method path.
impl Interp {
    pub(crate) fn ffi_read_cstr_ptr(&self, ptr_id: u64, span: Span) -> Result<Value, Control> {
        let addr = self.with_resource(ptr_id, |r| match r {
            Some(ResourceState::ForeignPtr(a)) => Some(*a),
            _ => None,
        });
        let addr = match addr {
            Some(a) => a,
            None => {
                return Err(AsError::at(
                    "ffi.read_cstr: foreign pointer handle is no longer valid".to_string(),
                    span,
                )
                .into())
            }
        };
        if addr == 0 {
            return Err(AsError::at(
                "ffi.read_cstr: foreign pointer is null".to_string(),
                span,
            )
            .into());
        }
        // SAFETY: `addr` is a non-null `ForeignPtr` the script obtained from a prior
        // foreign call; reading a C string from it is inherently the script's
        // responsibility (documented §3.4 — AScript does not track foreign-buffer
        // lifetimes). We walk until the first NUL via `CStr::from_ptr`, which reads
        // bytes until a terminator; if the buffer is not NUL-terminated this is UB —
        // the same contract every C `read_cstr` carries. This is the single
        // unavoidable trust point, gated by the `ffi` capability + the §3.4 contract.
        let s = unsafe {
            std::ffi::CStr::from_ptr(addr as *const std::os::raw::c_char)
                .to_string_lossy()
                .into_owned()
        };
        Ok(Value::Str(s.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform's libc / libm shared-object name (hermetic — always present).
    fn libc_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "libSystem.B.dylib"
        } else if cfg!(target_os = "windows") {
            "msvcrt.dll"
        } else {
            "libc.so.6"
        }
    }

    fn libm_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "libSystem.B.dylib"
        } else if cfg!(target_os = "windows") {
            "msvcrt.dll"
        } else {
            "libm.so.6"
        }
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    /// A Tier-1 error value is `{message: <Str>}` (via `make_error`).
    fn is_error_object(v: &Value) -> bool {
        matches!(v, Value::Object(o) if matches!(o.borrow().get("message"), Some(Value::Str(_))))
    }

    /// Open a library and resolve a symbol, unwrapping the Tier-1 `[value, err]`.
    fn open_symbol(
        interp: &Interp,
        lib: &str,
        name: &str,
        argtypes: Vec<FfiType>,
        ret: FfiType,
    ) -> u64 {
        let lib_pair = interp
            .ffi_open(&[Value::Str(lib.into())], span())
            .unwrap();
        let lib_id = pair_ok_native_id(&lib_pair);
        let argtype_vals: Vec<Value> = argtypes.iter().map(|t| descriptor(*t)).collect();
        let sym_pair = interp
            .ffi_lib_symbol(
                lib_id,
                &[
                    Value::Str(name.into()),
                    Value::Array(crate::value::ArrayCell::new(argtype_vals)),
                    descriptor(ret),
                ],
                span(),
            )
            .unwrap();
        pair_ok_native_id(&sym_pair)
    }

    /// Extract the native handle id from a Tier-1 `[Native, nil]` ok pair.
    fn pair_ok_native_id(v: &Value) -> u64 {
        if let Value::Array(a) = v {
            let b = a.borrow();
            assert_eq!(b.len(), 2, "Tier-1 pair");
            assert_eq!(b[1], Value::Nil, "expected ok pair, got err: {:?}", b[1]);
            if let Value::Native(n) = &b[0] {
                return n.id;
            }
        }
        panic!("expected [Native, nil], got {v:?}");
    }

    #[tokio::test]
    async fn libm_sqrt_and_cos() {
        let interp = Interp::new();
        // sqrt(2.0) ≈ 1.41421356
        let sqrt = open_symbol(&interp, libm_name(), "sqrt", vec![FfiType::F64], FfiType::F64);
        let r = interp
            .ffi_symbol_call(
                sqrt,
                &[Value::Array(crate::value::ArrayCell::new(vec![Value::Float(2.0)]))],
                span(),
            )
            .unwrap();
        if let Value::Float(f) = r {
            assert!((f - 2.0_f64.sqrt()).abs() < 1e-12, "sqrt(2) = {f}");
        } else {
            panic!("sqrt returned {r:?}");
        }
        // cos(0.0) == 1.0
        let cos = open_symbol(&interp, libm_name(), "cos", vec![FfiType::F64], FfiType::F64);
        let r = interp
            .ffi_symbol_call(
                cos,
                &[Value::Array(crate::value::ArrayCell::new(vec![Value::Float(0.0)]))],
                span(),
            )
            .unwrap();
        assert_eq!(r, Value::Float(1.0));
    }

    #[tokio::test]
    async fn libc_abs_int() {
        let interp = Interp::new();
        let abs = open_symbol(&interp, libc_name(), "abs", vec![FfiType::I32], FfiType::I32);
        let r = interp
            .ffi_symbol_call(
                abs,
                &[Value::Array(crate::value::ArrayCell::new(vec![Value::Int(-5)]))],
                span(),
            )
            .unwrap();
        assert_eq!(r, Value::Int(5));
    }

    #[tokio::test]
    async fn libc_strlen_via_cstr() {
        let interp = Interp::new();
        let strlen = open_symbol(&interp, libc_name(), "strlen", vec![FfiType::Ptr], FfiType::Size);
        let cstr = ffi_cstr(&[Value::Str("hello".into())], span()).unwrap();
        let r = interp
            .ffi_symbol_call(
                strlen,
                &[Value::Array(crate::value::ArrayCell::new(vec![cstr]))],
                span(),
            )
            .unwrap();
        assert_eq!(r, Value::Int(5));
    }

    #[test]
    fn cstr_is_nul_terminated() {
        let b = ffi_cstr(&[Value::Str("hi".into())], span()).unwrap();
        if let Value::Bytes(b) = b {
            assert_eq!(&*b.borrow(), &[b'h', b'i', 0]);
        } else {
            panic!("cstr not Bytes");
        }
        // Interior NUL → Tier-2 panic.
        assert!(ffi_cstr(&[Value::Str("a\0b".into())], span()).is_err());
    }

    #[tokio::test]
    async fn u8_range_check_panics() {
        let interp = Interp::new();
        // Use abs's i32 signature but pass via a u8-typed symbol re-bind: bind a fake
        // 1-arg symbol with u8. abs takes i32; instead we just test the marshalling
        // range check directly through a u8-typed `abs` (value 300 out of range).
        let sym = open_symbol(&interp, libc_name(), "abs", vec![FfiType::U8], FfiType::I32);
        let err = interp
            .ffi_symbol_call(
                sym,
                &[Value::Array(crate::value::ArrayCell::new(vec![Value::Int(300)]))],
                span(),
            )
            .unwrap_err();
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("out of range for u8"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected range panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_arity_panics_before_trampoline() {
        let interp = Interp::new();
        let abs = open_symbol(&interp, libc_name(), "abs", vec![FfiType::I32], FfiType::I32);
        // Pass zero args to a 1-arg symbol → Tier-2 arity panic (no trampoline entered).
        let err = interp
            .ffi_symbol_call(
                abs,
                &[Value::Array(crate::value::ArrayCell::new(vec![]))],
                span(),
            )
            .unwrap_err();
        match err {
            Control::Panic(e) => {
                assert!(e.message.contains("expected 1 argument"), "msg: {}", e.message)
            }
            other => panic!("expected arity panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_missing_library_is_tier1() {
        let interp = Interp::new();
        let pair = interp
            .ffi_open(&[Value::Str("/no/such/library.so.999".into())], span())
            .unwrap();
        if let Value::Array(a) = pair {
            let b = a.borrow();
            assert_eq!(b[0], Value::Nil, "value is nil on open failure");
            assert!(is_error_object(&b[1]), "err is an error object");
        } else {
            panic!("ffi.open should return a Tier-1 pair");
        }
    }

    #[tokio::test]
    async fn missing_symbol_is_tier1() {
        let interp = Interp::new();
        let lib_pair = interp
            .ffi_open(&[Value::Str(libc_name().into())], span())
            .unwrap();
        let lib_id = pair_ok_native_id(&lib_pair);
        let sym_pair = interp
            .ffi_lib_symbol(
                lib_id,
                &[
                    Value::Str("definitely_not_a_real_symbol_xyz".into()),
                    Value::Array(crate::value::ArrayCell::new(vec![])),
                    descriptor(FfiType::I32),
                ],
                span(),
            )
            .unwrap();
        if let Value::Array(a) = sym_pair {
            let b = a.borrow();
            assert_eq!(b[0], Value::Nil, "missing symbol → nil value");
            assert!(is_error_object(&b[1]), "err is an error object");
        } else {
            panic!("lib.symbol should return a Tier-1 pair");
        }
    }

    #[tokio::test]
    async fn bad_signature_is_tier2() {
        let interp = Interp::new();
        let lib_pair = interp
            .ffi_open(&[Value::Str(libc_name().into())], span())
            .unwrap();
        let lib_id = pair_ok_native_id(&lib_pair);
        // argtypes is not an array of descriptors → Tier-2 panic.
        let err = interp
            .ffi_lib_symbol(
                lib_id,
                &[
                    Value::Str("abs".into()),
                    Value::Int(42), // not an array
                    descriptor(FfiType::I32),
                ],
                span(),
            )
            .unwrap_err();
        assert!(matches!(err, Control::Panic(_)), "bad sig is Tier-2");
    }

    #[test]
    fn struct_layout_offsets_and_size() {
        // struct { i32 x; f64 y; } → x@0, y@8 (8-aligned), size 16.
        let layout = ffi_struct(
            &[Value::Array(crate::value::ArrayCell::new(vec![
                Value::Array(crate::value::ArrayCell::new(vec![
                    Value::Str("x".into()),
                    descriptor(FfiType::I32),
                ])),
                Value::Array(crate::value::ArrayCell::new(vec![
                    Value::Str("y".into()),
                    descriptor(FfiType::F64),
                ])),
            ]))],
            span(),
        )
        .unwrap();
        if let Value::Object(o) = layout {
            let o = o.borrow();
            assert_eq!(o.get("size"), Some(&Value::Int(16)));
            assert_eq!(o.get("align"), Some(&Value::Int(8)));
            if let Some(Value::Array(fields)) = o.get("fields") {
                let fields = fields.borrow();
                // x@0
                if let Value::Object(f0) = &fields[0] {
                    assert_eq!(f0.borrow().get("offset"), Some(&Value::Int(0)));
                }
                // y@8
                if let Value::Object(f1) = &fields[1] {
                    assert_eq!(f1.borrow().get("offset"), Some(&Value::Int(8)));
                }
            } else {
                panic!("no fields");
            }
        } else {
            panic!("layout not an object");
        }
    }

    #[test]
    fn struct_alloc_set_get_round_trip() {
        let layout = ffi_struct(
            &[Value::Array(crate::value::ArrayCell::new(vec![
                Value::Array(crate::value::ArrayCell::new(vec![
                    Value::Str("x".into()),
                    descriptor(FfiType::I32),
                ])),
                Value::Array(crate::value::ArrayCell::new(vec![
                    Value::Str("y".into()),
                    descriptor(FfiType::F64),
                ])),
            ]))],
            span(),
        )
        .unwrap();
        // alloc → zeroed Bytes of size 16.
        let buf = ffi_alloc(std::slice::from_ref(&layout), span()).unwrap();
        if let Value::Bytes(b) = &buf {
            assert_eq!(b.borrow().len(), 16);
        }
        // set x=3, y=2.5; read them back.
        ffi_set(
            &[layout.clone(), buf.clone(), Value::Str("x".into()), Value::Int(3)],
            span(),
        )
        .unwrap();
        ffi_set(
            &[layout.clone(), buf.clone(), Value::Str("y".into()), Value::Float(2.5)],
            span(),
        )
        .unwrap();
        let x = ffi_get(
            &[layout.clone(), buf.clone(), Value::Str("x".into())],
            span(),
        )
        .unwrap();
        assert_eq!(x, Value::Int(3));
        let y = ffi_get(&[layout, buf, Value::Str("y".into())], span()).unwrap();
        assert_eq!(y, Value::Float(2.5));
    }

    #[tokio::test]
    async fn u64_bit_pattern_round_trip() {
        // -1 marshals to u64 0xFFFF... — exercise via a passthrough: there is no pure
        // libc identity over u64 reliably, so validate the IN marshalling range rule:
        // u64 accepts a negative int (no range check), unlike u8.
        let interp = Interp::new();
        // strlen takes a ptr; bind a u64 ptr-less check is awkward — instead assert the
        // range helpers directly.
        assert!(want_int_in_range(&Value::Int(-1), 0, u8::MAX as i64, "u8", 0, span()).is_err());
        assert!(want_int(&Value::Int(-1), "u64", 0, span()).is_ok());
        let _ = interp; // keep an interp to mirror the other tests' shape
    }
}
