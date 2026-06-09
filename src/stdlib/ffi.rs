//! `std/ffi` — the Foreign Function Interface (FFI campaign §3/§5).
//!
//! Task 6 lands the HANDLE LAYER: the three `NativeKind` handles
//! (`ForeignLib`/`ForeignSymbol`/`ForeignPtr`) backed by `ResourceState`, the
//! C-type descriptor vocabulary (`ffi.i32` …), the `FfiSymbol` resource (a resolved
//! function address + bound signature that KEEPS THE OWNING `Library` ALIVE, §3.4),
//! and `ffi.open` (`dlopen`, Tier-1). Task 7 adds `lib.symbol`/`sym.call` marshalling
//! + struct/cstr/read_cstr.
//!
//! All three handles are native resources with deterministic `Drop` (the `Library`
//! `dlclose`s on drop) and are **GC-untraced** (a raw foreign pointer / OS handle is
//! opaque memory the collector cannot reason about — `gc.rs`'s `Native` catch-all
//! `_ => {}` keeps them leaf-opaque; proven by a unit test in `gc.rs`).
//!
//! **Capability gate.** Every `ffi.*` entry routes through `call_stdlib`'s central
//! `required_cap("ffi", _) -> Some(Cap::Ffi)` gate, so `ffi.open` is denied after a
//! `caps.drop("ffi")` / `--deny ffi` / `--sandbox`. Operating an ALREADY-OPEN handle
//! is re-gated at `call_native_method` via `NativeKind::governing_cap` (also
//! `Cap::Ffi`), so a drop HOLDS for handles opened before it.

use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, Value};
use libffi::middle::{Cif, Type};
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

    #[allow(dead_code)] // Used by Task 7's signature parsing.
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
    #[allow(dead_code)] // Used by Task 7's CIF build.
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

/// Build the Tier-1 ok pair `[value, nil]` (`ffi.open` success).
fn ok_pair(value: Value) -> Value {
    crate::interp::make_pair(value, Value::Nil)
}

/// Build the Tier-1 err pair `[nil, {message}]` (recoverable open failure).
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
    #[allow(dead_code)] // Read by Task 7's `sym.call`.
    pub(crate) addr: *mut c_void,
    /// The libffi CIF for the bound `argtypes -> ret` signature.
    #[allow(dead_code)] // Read by Task 7's `sym.call`.
    pub(crate) cif: Cif,
    /// The bound argument types (drives marshalling + the pre-trampoline arity check).
    #[allow(dead_code)] // Read by Task 7's `sym.call`.
    pub(crate) argtypes: Vec<FfiType>,
    /// The bound return type (drives the result marshal-out + the typed `Cif::call`).
    #[allow(dead_code)] // Read by Task 7's `sym.call`.
    pub(crate) ret: FfiType,
    /// Keeps the owning `Library` alive past the lib handle's own drop, so `addr`
    /// stays valid. SOUNDNESS-CRITICAL: the `unsafe` call deref relies on this.
    pub(crate) _lib: Rc<libloading::Library>,
}

// ───────────────────────────────── exports ───────────────────────────────────

/// `std/ffi` exports — the C-type descriptors as values + `open` (Task 6).
/// `struct`/`cstr`/`read_cstr`/`alloc`/`get`/`set` + the `symbol`/`call` handle
/// methods land in Task 7.
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
    ]
}

// ───────────────────────────────── routing ───────────────────────────────────

impl Interp {
    /// `std/ffi` dispatch. Gated by the central `required_cap("ffi", _)` before this is
    /// reached. Task 6 wires `open`; Task 7 adds the rest.
    pub(crate) async fn call_ffi(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "open" => self.ffi_open(args, span),
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
            Err(e) => Ok(err_pair(Value::Str(format!("ffi.open: {e}").into()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn libc_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "libSystem.B.dylib"
        } else if cfg!(target_os = "windows") {
            "msvcrt.dll"
        } else {
            "libc.so.6"
        }
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    /// A Tier-1 error value is `{message: <Str>}` (via `make_error`).
    fn is_error_object(v: &Value) -> bool {
        matches!(v, Value::Object(o) if matches!(o.borrow().get("message"), Some(Value::Str(_))))
    }

    #[tokio::test]
    async fn open_libc_succeeds_tier1() {
        let interp = Interp::new();
        let pair = interp
            .ffi_open(&[Value::Str(libc_name().into())], span())
            .unwrap();
        if let Value::Array(a) = pair {
            let b = a.borrow();
            assert!(matches!(&b[0], Value::Native(_)), "value is a ForeignLib handle");
            assert_eq!(b[1], Value::Nil, "no error on success");
        } else {
            panic!("ffi.open should return a Tier-1 pair");
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
}
