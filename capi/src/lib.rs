//! `ascript-capi` — the C ABI for embedding the AScript engine (EMBED §8).
//!
//! A `cdylib`/`staticlib` over [`ascript::embed`] — the ONLY surface this crate wraps.
//! Every C-facing function is `unsafe extern "C"`, **panic-safe** (each body catches
//! unwind → status code; a caught panic never crosses the FFI boundary), **thread-
//! affinity checked** (a cheap `ThreadId` compare → `AS_ERR_WRONG_THREAD`, never UB on a
//! cross-thread `Rc` touch), and **length-explicit UTF-8** (no NUL-termination assumed on
//! input; outputs carry an explicit length).
//!
//! See `include/ascript.h` for the hand-written, checked-in header (the stable ABI).
//!
//! # The model (EMBED §1)
//!
//! An isolate is `!Send` per host thread. Each [`as_isolate`] records its creating
//! `ThreadId`; every entry compares it. Cross-thread use is a *checked error*, not UB.
//! The one unfixable case — [`as_value_free`] from the wrong thread — leaks the box and
//! returns (an off-thread `Rc` decrement is a data race; a documented leak beats UB).

// Every C-ABI fn shares one documented safety contract (the `ascript.h` ownership +
// THREADING blocks): handles came from this library, are freed once, on their creating
// thread; lengths match the buffers. Per-fn `# Safety` sections would just restate it, so
// the contract lives in the header + the module docs above.
#![allow(clippy::missing_safety_doc)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::panic::AssertUnwindSafe;
use std::thread::ThreadId;

use ascript::embed::{AsKind, AsValue, EmbedError, HostCtx, HostError, Isolate, OutputMode};

// ── status codes (mirrors `as_status` in ascript.h) ─────────────────────────

/// The ABI version this library implements. A host asserts `ascript_abi_version() ==
/// ASCRIPT_CAPI_ABI` from its header at load time. Bumps only on a breaking C-surface
/// change (the C ABI is the only stable ABI — EMBED §8.2/§10).
pub const ASCRIPT_CAPI_ABI: u32 = 1;

#[allow(non_camel_case_types)]
type as_status = c_int;

const AS_OK: as_status = 0;
const AS_ERR_COMPILE: as_status = 1;
const AS_ERR_PANIC: as_status = 2;
const AS_ERR_EXIT: as_status = 3;
const AS_ERR_UTF8: as_status = 4;
const AS_ERR_TYPE: as_status = 5;
const AS_ERR_UNDEFINED: as_status = 6;
const AS_ERR_CONFIG: as_status = 7;
const AS_ERR_WRONG_THREAD: as_status = 8;
const AS_ERR_NESTED_RUNTIME: as_status = 9;
const AS_ERR_POISONED: as_status = 10;
const AS_ERR_INTERNAL: as_status = 127;

// ── value-kind codes (mirrors `AS_KIND_*` in ascript.h) ─────────────────────

const AS_KIND_NIL: c_int = 0;
const AS_KIND_BOOL: c_int = 1;
const AS_KIND_INT: c_int = 2;
const AS_KIND_FLOAT: c_int = 3;
const AS_KIND_DECIMAL: c_int = 4;
const AS_KIND_STR: c_int = 5;
const AS_KIND_ARRAY: c_int = 6;
const AS_KIND_OBJECT: c_int = 7;
const AS_KIND_MAP: c_int = 8;
const AS_KIND_SET: c_int = 9;
const AS_KIND_BYTES: c_int = 10;
const AS_KIND_CALLABLE: c_int = 11;
const AS_KIND_FUTURE: c_int = 12;
const AS_KIND_OPAQUE: c_int = 13;

fn kind_code(k: AsKind) -> c_int {
    match k {
        AsKind::Nil => AS_KIND_NIL,
        AsKind::Bool => AS_KIND_BOOL,
        AsKind::Int => AS_KIND_INT,
        AsKind::Float => AS_KIND_FLOAT,
        AsKind::Decimal => AS_KIND_DECIMAL,
        AsKind::Str => AS_KIND_STR,
        AsKind::Array => AS_KIND_ARRAY,
        AsKind::Object => AS_KIND_OBJECT,
        AsKind::Map => AS_KIND_MAP,
        AsKind::Set => AS_KIND_SET,
        AsKind::Bytes => AS_KIND_BYTES,
        AsKind::Callable => AS_KIND_CALLABLE,
        AsKind::Future => AS_KIND_FUTURE,
        AsKind::Opaque => AS_KIND_OPAQUE,
        // `AsKind` is `#[non_exhaustive]`; a future kind maps to opaque (always
        // pass-back-able) rather than failing the read.
        _ => AS_KIND_OPAQUE,
    }
}

/// Map an [`EmbedError`] to its status code + a human message (for `last_error`).
fn embed_status(e: &EmbedError) -> (as_status, String) {
    match e {
        EmbedError::Compile(diags) => {
            let msg = diags
                .first()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "compile error".to_string());
            (AS_ERR_COMPILE, msg)
        }
        EmbedError::Panic(p) => (AS_ERR_PANIC, p.message.clone()),
        EmbedError::Exit(code) => (AS_ERR_EXIT, format!("script called exit({code})")),
        EmbedError::NestedRuntime => (AS_ERR_NESTED_RUNTIME, e.to_string()),
        EmbedError::Undefined(m) => (AS_ERR_UNDEFINED, m.clone()),
        EmbedError::Config(m) => (AS_ERR_CONFIG, m.clone()),
        EmbedError::Archive(m) => (AS_ERR_CONFIG, m.clone()),
        // `EmbedError` is `#[non_exhaustive]`; a future variant reports as a config error.
        _ => (AS_ERR_CONFIG, e.to_string()),
    }
}

// ── handles ─────────────────────────────────────────────────────────────────

/// A single registered C host function: the callback, its userdata, and its tier.
///
/// `userdata` is a raw pointer the HOST promises is thread-affine to this isolate (the
/// header documents it). `CHostFn` is `!Send` (it holds raw pointers), so it never
/// crosses a thread by construction — the promise is structural on the Rust side.
#[derive(Clone, Copy)]
struct CHostFn {
    func: as_host_fn,
    userdata: *mut c_void,
    /// `true` = a fallible (Tier-1 `[value, err]`) fn; `false` = plain (Tier-2 on error).
    fallible: bool,
}

/// The C `as_isolate` handle: the embed [`Isolate`], its owning `ThreadId`, a
/// per-isolate last-error buffer, a poison flag (EMBED §8.2), and the accumulator of
/// C host functions registered per `host:<module>` (so repeated `as_register_host_fn`
/// calls for one module ACCUMULATE — each call re-installs the whole module).
pub struct CIsolate {
    iso: Isolate,
    thread: ThreadId,
    /// The last error message for this isolate, as a NUL-terminated C string
    /// (`as_last_error` hands out a borrowed pointer valid until the next call).
    last_error: RefCell<CString>,
    /// Set when a caught Rust panic poisoned the isolate; every subsequent call (except
    /// `as_isolate_free`/`as_last_error`) returns `AS_ERR_POISONED`.
    poisoned: Cell<bool>,
    /// `host:<module>` → its accumulated `fname → CHostFn` map.
    host_fns: RefCell<HashMap<String, HashMap<String, CHostFn>>>,
}

impl CIsolate {
    fn set_error(&self, msg: &str) {
        // Replace interior NULs (a C string cannot carry them) so construction never fails.
        let cleaned = msg.replace('\0', " ");
        let c = CString::new(cleaned).unwrap_or_else(|_| CString::new("error").unwrap());
        *self.last_error.borrow_mut() = c;
    }
}

/// The C `as_value` handle: the embed [`AsValue`] + its owning `ThreadId` (so a
/// cross-thread `as_value_free` can leak-and-return instead of racing an `Rc`).
pub struct CValue {
    v: AsValue,
    thread: ThreadId,
}

/// Run `body` under the full per-extern-fn preamble for an isolate-bearing call:
/// thread check → poison check → `catch_unwind`. On a caught panic the isolate is
/// poisoned and `AS_ERR_INTERNAL` returned; the unwind never crosses the boundary.
///
/// SAFETY: `iso` must be a valid `*mut CIsolate` (or NULL, handled → `AS_ERR_CONFIG`).
unsafe fn with_isolate<F>(iso: *mut CIsolate, body: F) -> as_status
where
    F: FnOnce(&CIsolate) -> as_status,
{
    if iso.is_null() {
        return AS_ERR_CONFIG;
    }
    let c = &*iso;
    if std::thread::current().id() != c.thread {
        // Do NOT touch any `Rc` from the wrong thread — return before set_error (that
        // touches the RefCell, which is also thread-affine).
        return AS_ERR_WRONG_THREAD;
    }
    if c.poisoned.get() {
        return AS_ERR_POISONED;
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| body(c))) {
        Ok(status) => status,
        Err(_) => {
            c.poisoned.set(true);
            c.set_error("internal panic (isolate poisoned)");
            AS_ERR_INTERNAL
        }
    }
}

// ── version / ABI ───────────────────────────────────────────────────────────

/// The crate semver, packed `major<<16 | minor<<8 | patch`.
#[no_mangle]
pub extern "C" fn ascript_version() -> u32 {
    let major: u32 = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0);
    let minor: u32 = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0);
    let patch: u32 = env!("CARGO_PKG_VERSION_PATCH").parse().unwrap_or(0);
    (major << 16) | (minor << 8) | patch
}

/// The ABI version of this library (`ASCRIPT_CAPI_ABI`). The load-time guard.
#[no_mangle]
pub extern "C" fn ascript_abi_version() -> u32 {
    ASCRIPT_CAPI_ABI
}

// ── isolate lifecycle ───────────────────────────────────────────────────────

/// Construct a new isolate (deny-all caps, captured output). NULL on construction
/// failure. Free with [`as_isolate_free`].
#[no_mangle]
pub extern "C" fn as_isolate_new() -> *mut CIsolate {
    // catch_unwind so a build panic can't cross the boundary; NULL signals failure.
    let built = std::panic::catch_unwind(|| {
        Isolate::builder().output(OutputMode::Capture).build()
    });
    match built {
        Ok(Ok(iso)) => Box::into_raw(Box::new(CIsolate {
            iso,
            thread: std::thread::current().id(),
            last_error: RefCell::new(CString::new("").unwrap()),
            poisoned: Cell::new(false),
            host_fns: RefCell::new(HashMap::new()),
        })),
        _ => std::ptr::null_mut(),
    }
}

/// Free an isolate. NULL-safe. Works even from the wrong thread? No — freeing an
/// `Isolate` (which holds `Rc`s) off-thread is a data race, so this is documented as
/// creating-thread-only (the header THREADING block). Calling it on a POISONED isolate
/// is fine (free is always allowed).
#[no_mangle]
pub unsafe extern "C" fn as_isolate_free(iso: *mut CIsolate) {
    if iso.is_null() {
        return;
    }
    // SAFETY: the host promises `iso` came from `as_isolate_new` and is freed once, on
    // its creating thread (documented). A wrong-thread free is the host's UB (the same
    // class as the documented `as_value_free` leak) — we cannot check here without
    // touching the very `Rc`s a wrong-thread free would race, so we trust the contract.
    let boxed = unsafe { Box::from_raw(iso) };
    // Drop inside catch_unwind: `Isolate::drop` runs `gc::collect()` + shuts the runtime
    // down; a panic there must not cross the boundary.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || drop(boxed)));
}

/// The last error message for this isolate (borrowed; valid until the next call on this
/// isolate). Always succeeds for a live isolate, even when poisoned. `AS_ERR_CONFIG` on
/// NULL args; `AS_ERR_WRONG_THREAD` off-thread.
#[no_mangle]
pub unsafe extern "C" fn as_last_error(
    iso: *const CIsolate,
    msg: *mut *const c_char,
    msg_len: *mut usize,
) -> as_status {
    if iso.is_null() || msg.is_null() || msg_len.is_null() {
        return AS_ERR_CONFIG;
    }
    // SAFETY: NULL checked above; the host promises a valid `iso`.
    let c = unsafe { &*iso };
    if std::thread::current().id() != c.thread {
        return AS_ERR_WRONG_THREAD;
    }
    // No poison check: reading the last error after a poison is exactly the use case.
    let borrowed = c.last_error.borrow();
    unsafe {
        *msg = borrowed.as_ptr();
        *msg_len = borrowed.as_bytes().len();
    }
    AS_OK
}

// ── eval / call ─────────────────────────────────────────────────────────────

/// Read a length-explicit C string into a `&str` (NOT NUL-terminated — `src_len` bytes).
///
/// SAFETY: `ptr` must be valid for `len` bytes (or NULL → `Err`).
unsafe fn read_utf8<'a>(ptr: *const c_char, len: usize) -> Result<&'a str, as_status> {
    if ptr.is_null() {
        return Err(AS_ERR_CONFIG);
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len);
    std::str::from_utf8(bytes).map_err(|_| AS_ERR_UTF8)
}

/// Compile + run `src` on the isolate (blocking). On `AS_OK` `*out` receives a
/// caller-owned trailing-value handle (free with [`as_value_free`]). On error, consult
/// [`as_last_error`]; `*out` is left untouched.
#[no_mangle]
pub unsafe extern "C" fn as_eval(
    iso: *mut CIsolate,
    src: *const c_char,
    src_len: usize,
    out: *mut *mut CValue,
) -> as_status {
    unsafe {
        with_isolate(iso, |c| {
            let src = match read_utf8(src, src_len) {
                Ok(s) => s,
                Err(status) => return status,
            };
            match c.iso.eval(src) {
                Ok(v) => {
                    store_value(out, v, c.thread);
                    AS_OK
                }
                Err(e) => {
                    let (status, msg) = embed_status(&e);
                    c.set_error(&msg);
                    status
                }
            }
        })
    }
}

/// Call a module-scope global `name` with `nargs` argument handles. Auto-awaits an
/// `async fn` result. `*out` receives a caller-owned result handle on `AS_OK`.
#[no_mangle]
pub unsafe extern "C" fn as_call(
    iso: *mut CIsolate,
    name: *const c_char,
    name_len: usize,
    args: *const *const CValue,
    nargs: usize,
    out: *mut *mut CValue,
) -> as_status {
    unsafe {
        with_isolate(iso, |c| {
            let name = match read_utf8(name, name_len) {
                Ok(s) => s,
                Err(status) => return status,
            };
            let argv = match collect_args(args, nargs, c.thread) {
                Ok(v) => v,
                Err(status) => return status,
            };
            match c.iso.call(name, &argv) {
                Ok(v) => {
                    store_value(out, v, c.thread);
                    AS_OK
                }
                Err(e) => {
                    let (status, msg) = embed_status(&e);
                    c.set_error(&msg);
                    status
                }
            }
        })
    }
}

/// Gather `nargs` argument handles into an owned `Vec<AsValue>`, validating each pointer
/// is non-NULL and thread-matched.
///
/// SAFETY: `args` must point to `nargs` valid `*const CValue` (or be NULL when `nargs==0`).
unsafe fn collect_args(
    args: *const *const CValue,
    nargs: usize,
    thread: ThreadId,
) -> Result<Vec<AsValue>, as_status> {
    if nargs == 0 {
        return Ok(Vec::new());
    }
    if args.is_null() {
        return Err(AS_ERR_CONFIG);
    }
    let slice = std::slice::from_raw_parts(args, nargs);
    let mut out = Vec::with_capacity(nargs);
    for &p in slice {
        if p.is_null() {
            return Err(AS_ERR_CONFIG);
        }
        let cv = &*p;
        if cv.thread != thread {
            return Err(AS_ERR_WRONG_THREAD);
        }
        out.push(cv.v.clone());
    }
    Ok(out)
}

/// Box an `AsValue` into a caller-owned `CValue` and store it at `*out` (NULL-safe: a
/// NULL `out` drops the value, matching "value produced but discarded").
///
/// SAFETY: `out` must be a valid `*mut *mut CValue` or NULL.
unsafe fn store_value(out: *mut *mut CValue, v: AsValue, thread: ThreadId) {
    if out.is_null() {
        return;
    }
    *out = Box::into_raw(Box::new(CValue { v, thread }));
}

// ── value constructors (caller-owned until passed/freed) ────────────────────

/// Box a fresh value handle on the CURRENT thread.
fn new_value(v: AsValue) -> *mut CValue {
    Box::into_raw(Box::new(CValue {
        v,
        thread: std::thread::current().id(),
    }))
}

/// A `nil` value handle.
#[no_mangle]
pub extern "C" fn as_nil() -> *mut CValue {
    new_value(AsValue::nil())
}

/// A `bool` value handle.
#[no_mangle]
pub extern "C" fn as_bool(b: bool) -> *mut CValue {
    new_value(AsValue::from(b))
}

/// An `int` value handle.
#[no_mangle]
pub extern "C" fn as_int(n: i64) -> *mut CValue {
    new_value(AsValue::from(n))
}

/// A `float` value handle.
#[no_mangle]
pub extern "C" fn as_float(x: f64) -> *mut CValue {
    new_value(AsValue::from(x))
}

/// A `string` value handle from `len` UTF-8 bytes. NULL on invalid UTF-8 or NULL `utf8`.
#[no_mangle]
pub unsafe extern "C" fn as_string(utf8: *const c_char, len: usize) -> *mut CValue {
    if utf8.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host promises `utf8` is valid for `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(utf8 as *const u8, len) };
    match std::str::from_utf8(bytes) {
        Ok(s) => new_value(AsValue::from(s)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a value handle. NULL-safe. **Creating-thread only**: a wrong-thread free LEAKS
/// the box and returns (an off-thread `Rc` decrement is a data race; the leak is the
/// documented contract — EMBED §8.2).
#[no_mangle]
pub unsafe extern "C" fn as_value_free(v: *mut CValue) {
    if v.is_null() {
        return;
    }
    // SAFETY: the host promises `v` came from a value constructor / out-param and is freed
    // once. Read the owning thread id WITHOUT taking ownership first.
    let thread = unsafe { (*v).thread };
    if std::thread::current().id() != thread {
        // Leak the box (documented) rather than race the `Rc` refcount off-thread.
        return;
    }
    // SAFETY: on-thread; reconstruct + drop the box. A drop panic must not cross.
    let boxed = unsafe { Box::from_raw(v) };
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || drop(boxed)));
}

// ── value readers ───────────────────────────────────────────────────────────

/// SAFETY helper: borrow a `*const CValue`, NULL-checked.
unsafe fn value_ref<'a>(v: *const CValue) -> Result<&'a CValue, as_status> {
    if v.is_null() {
        return Err(AS_ERR_CONFIG);
    }
    Ok(&*v)
}

/// The value's [`AsKind`] code (`AS_KIND_*`). A value read does NOT need an isolate
/// (values are self-contained), but it IS thread-affine: a value carries its owning
/// thread id and a wrong-thread read returns `AS_ERR_WRONG_THREAD`.
#[no_mangle]
pub unsafe extern "C" fn as_value_kind(v: *const CValue, out: *mut c_int) -> as_status {
    if out.is_null() {
        return AS_ERR_CONFIG;
    }
    unsafe {
        let cv = match value_ref(v) {
            Ok(c) => c,
            Err(s) => return s,
        };
        if cv.thread != std::thread::current().id() {
            return AS_ERR_WRONG_THREAD;
        }
        match std::panic::catch_unwind(AssertUnwindSafe(|| kind_code(cv.v.kind()))) {
            Ok(code) => {
                *out = code;
                AS_OK
            }
            Err(_) => AS_ERR_INTERNAL,
        }
    }
}

/// Read an `int`. `AS_ERR_TYPE` if the value is not an `int`.
#[no_mangle]
pub unsafe extern "C" fn as_value_int(v: *const CValue, out: *mut i64) -> as_status {
    if out.is_null() {
        return AS_ERR_CONFIG;
    }
    unsafe {
        let cv = match value_ref(v) {
            Ok(c) => c,
            Err(s) => return s,
        };
        if cv.thread != std::thread::current().id() {
            return AS_ERR_WRONG_THREAD;
        }
        match cv.v.as_int() {
            Some(n) => {
                *out = n;
                AS_OK
            }
            None => AS_ERR_TYPE,
        }
    }
}

/// Read a `float`. `AS_ERR_TYPE` if not a `float`.
#[no_mangle]
pub unsafe extern "C" fn as_value_float(v: *const CValue, out: *mut f64) -> as_status {
    if out.is_null() {
        return AS_ERR_CONFIG;
    }
    unsafe {
        let cv = match value_ref(v) {
            Ok(c) => c,
            Err(s) => return s,
        };
        if cv.thread != std::thread::current().id() {
            return AS_ERR_WRONG_THREAD;
        }
        match cv.v.as_float() {
            Some(x) => {
                *out = x;
                AS_OK
            }
            None => AS_ERR_TYPE,
        }
    }
}

/// Read a `bool`. `AS_ERR_TYPE` if not a `bool`.
#[no_mangle]
pub unsafe extern "C" fn as_value_bool(v: *const CValue, out: *mut bool) -> as_status {
    if out.is_null() {
        return AS_ERR_CONFIG;
    }
    unsafe {
        let cv = match value_ref(v) {
            Ok(c) => c,
            Err(s) => return s,
        };
        if cv.thread != std::thread::current().id() {
            return AS_ERR_WRONG_THREAD;
        }
        match cv.v.as_bool() {
            Some(b) => {
                *out = b;
                AS_OK
            }
            None => AS_ERR_TYPE,
        }
    }
}

/// Borrow a `string` value's UTF-8 bytes (`*ptr`/`*len`; valid until the value is freed).
/// `AS_ERR_TYPE` if not a `string`.
#[no_mangle]
pub unsafe extern "C" fn as_value_string(
    v: *const CValue,
    ptr: *mut *const c_char,
    len: *mut usize,
) -> as_status {
    if ptr.is_null() || len.is_null() {
        return AS_ERR_CONFIG;
    }
    unsafe {
        let cv = match value_ref(v) {
            Ok(c) => c,
            Err(s) => return s,
        };
        if cv.thread != std::thread::current().id() {
            return AS_ERR_WRONG_THREAD;
        }
        match cv.v.as_str() {
            Some(s) => {
                *ptr = s.as_ptr() as *const c_char;
                *len = s.len();
                AS_OK
            }
            None => AS_ERR_TYPE,
        }
    }
}

// ── JSON bridge + output (Task 4.2) ─────────────────────────────────────────

/// Deep-serialize a value to a JSON string (`*out`/`*len`; free with [`as_string_free`]).
/// Routes through the isolate's `std/json` serializer. `AS_ERR_CONFIG` on a
/// non-serializable kind or a reference cycle (message in [`as_last_error`]).
#[no_mangle]
pub unsafe extern "C" fn as_value_to_json(
    iso: *const CIsolate,
    v: *const CValue,
    out: *mut *mut c_char,
    len: *mut usize,
) -> as_status {
    if iso.is_null() || out.is_null() || len.is_null() {
        return AS_ERR_CONFIG;
    }
    // SAFETY: NULL checked; host promises a valid `iso`.
    let c = &*iso;
    if std::thread::current().id() != c.thread {
        return AS_ERR_WRONG_THREAD;
    }
    if c.poisoned.get() {
        return AS_ERR_POISONED;
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        let cv = value_ref(v)?;
        if cv.thread != c.thread {
            return Err(AS_ERR_WRONG_THREAD);
        }
        cv.v.to_json().map_err(|e| {
            let (_s, msg) = embed_status(&e);
            c.set_error(&msg);
            AS_ERR_CONFIG
        })
    })) {
        Ok(Ok(json)) => {
            store_cstring(out, len, json);
            AS_OK
        }
        Ok(Err(status)) => status,
        Err(_) => {
            c.poisoned.set(true);
            c.set_error("internal panic (isolate poisoned)");
            AS_ERR_INTERNAL
        }
    }
}

/// Parse `len` UTF-8 JSON bytes into a fresh value handle (a DEEP COPY). `*out` receives
/// a caller-owned handle on `AS_OK`. `AS_ERR_CONFIG` on invalid JSON.
#[no_mangle]
pub unsafe extern "C" fn as_json_parse(
    iso: *mut CIsolate,
    json: *const c_char,
    len: usize,
    out: *mut *mut CValue,
) -> as_status {
    with_isolate(iso, |c| {
        let text = match read_utf8(json, len) {
            Ok(s) => s,
            Err(status) => return status,
        };
        match c.iso.json_parse(text) {
            Ok(v) => {
                store_value(out, v, c.thread);
                AS_OK
            }
            Err(e) => {
                let (_s, msg) = embed_status(&e);
                c.set_error(&msg);
                AS_ERR_CONFIG
            }
        }
    })
}

/// Drain the isolate's captured output (`*out`/`*len`; free with [`as_string_free`]).
/// Empty under inherit/no-output. The buffer is cleared (drained), so repeated calls
/// return only NEW output.
#[no_mangle]
pub unsafe extern "C" fn as_take_output(
    iso: *mut CIsolate,
    out: *mut *mut c_char,
    len: *mut usize,
) -> as_status {
    if out.is_null() || len.is_null() {
        return AS_ERR_CONFIG;
    }
    with_isolate(iso, |c| {
        let s = c.iso.take_output();
        store_cstring(out, len, s);
        AS_OK
    })
}

/// Box a Rust `String` into a heap C string (interior NULs replaced) + its byte length,
/// transfer ownership to the caller (`as_string_free`).
///
/// SAFETY: `out`/`len` must be valid out-pointers (checked by callers).
unsafe fn store_cstring(out: *mut *mut c_char, len: *mut usize, s: String) {
    let cleaned = s.replace('\0', " ");
    let byte_len = cleaned.len();
    let c = CString::new(cleaned).unwrap_or_else(|_| CString::new("").unwrap());
    *len = byte_len;
    *out = c.into_raw();
}

/// Free a string returned by `as_value_to_json`/`as_take_output`. NULL-safe.
#[no_mangle]
pub unsafe extern "C" fn as_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: `s` came from `CString::into_raw` (the only producer).
    let _ = CString::from_raw(s);
}

// ── host functions (Task 4.2) ───────────────────────────────────────────────

/// A C host-function callback (EMBED §8.2). On success the callback writes a result
/// handle to `*out` (ownership transfers to the engine — the engine frees it). On error
/// it returns a non-`AS_OK` status and MAY write an `as_string_free`-able message to
/// `*err_utf8`.
///
/// `tier` semantics are set at registration: a fallible (tier 1) fn's error becomes the
/// script-visible `[nil, err]` pair; a plain (tier 0) fn's error becomes a Tier-2 panic.
///
/// `userdata` is the raw pointer passed at registration — the host promises it is
/// thread-affine to the isolate.
#[allow(non_camel_case_types)]
pub type as_host_fn = unsafe extern "C" fn(
    userdata: *mut c_void,
    iso: *mut CIsolate,
    args: *const *const CValue,
    nargs: usize,
    out: *mut *mut CValue,
    err_utf8: *mut *mut c_char,
) -> as_status;

/// Register a C-callback host function under `host:<module>` on the isolate (late
/// registration — must precede the FIRST `import` of that module, EMBED §8.2). Repeated
/// calls for the same module ACCUMULATE; each call re-installs the whole module.
///
/// `tier`: 0 = plain (an error → Tier-2 panic); 1 = fallible (an error → the
/// script-visible `[nil, err]` pair). The callback + `userdata` are wrapped in a Rust
/// closure (the closure is `!Send`, so the host's thread-affinity promise on `userdata`
/// is structural).
#[no_mangle]
pub unsafe extern "C" fn as_register_host_fn(
    iso: *mut CIsolate,
    module: *const c_char,
    module_len: usize,
    name: *const c_char,
    name_len: usize,
    func: Option<as_host_fn>,
    userdata: *mut c_void,
    tier: c_int,
) -> as_status {
    with_isolate(iso, |c| {
        let module = match read_utf8(module, module_len) {
            Ok(s) => s.to_string(),
            Err(status) => return status,
        };
        let fname = match read_utf8(name, name_len) {
            Ok(s) => s.to_string(),
            Err(status) => return status,
        };
        let Some(func) = func else {
            return AS_ERR_CONFIG;
        };
        let entry = CHostFn {
            func,
            userdata,
            fallible: tier == 1,
        };
        // Accumulate this fn into the module's map.
        {
            let mut map = c.host_fns.borrow_mut();
            map.entry(module.clone()).or_default().insert(fname, entry);
        }
        // Re-install the WHOLE module (late registration replaces the registry entry; the
        // engine errors if the module was already imported by a script). Clone the fn map
        // out of the borrow before building (the builder closure runs synchronously, but
        // keep the borrow scope tight — the standing discipline).
        let fns: HashMap<String, CHostFn> = c
            .host_fns
            .borrow()
            .get(&module)
            .cloned()
            .unwrap_or_default();
        // The isolate's own ThreadId — captured so the result/arg CValue boxes the
        // callback bridge creates carry the right owning thread.
        let thread = c.thread;
        let result = c.iso.register_host_module_late(&module, move |b| {
            for (fname, entry) in &fns {
                let entry = *entry;
                let bridge = make_c_bridge(entry, thread);
                if entry.fallible {
                    b.fallible_func(fname, bridge);
                } else {
                    b.func(fname, bridge);
                }
            }
        });
        match result {
            Ok(()) => AS_OK,
            Err(e) => {
                let (status, msg) = embed_status(&e);
                c.set_error(&msg);
                status
            }
        }
    })
}

/// Build the Rust closure that bridges a registered C host fn into the engine: marshal
/// the `&[AsValue]` args into caller-owned `CValue` boxes, invoke the C callback, and
/// adapt its result/`out`/`err` back to `Result<AsValue, HostError>`.
fn make_c_bridge(
    entry: CHostFn,
    thread: ThreadId,
) -> impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static {
    move |_ctx: &mut HostCtx, args: &[AsValue]| {
        // Box each arg as a CValue the callback can read; collect the raw pointers.
        let boxes: Vec<*mut CValue> = args
            .iter()
            .map(|a| {
                Box::into_raw(Box::new(CValue {
                    v: a.clone(),
                    thread,
                }))
            })
            .collect();
        let arg_ptrs: Vec<*const CValue> = boxes.iter().map(|&p| p as *const CValue).collect();
        let mut out: *mut CValue = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        // SAFETY: the callback came from `as_register_host_fn`; we pass it valid arg
        // pointers + out/err slots. `iso` is NULL here (re-entrant eval is rejected v1 —
        // the callback must NOT call back into the isolate; a host that does gets a
        // NULL-isolate AS_ERR_CONFIG, never UB).
        let status = unsafe {
            (entry.func)(
                entry.userdata,
                std::ptr::null_mut(),
                arg_ptrs.as_ptr(),
                arg_ptrs.len(),
                &mut out,
                &mut err,
            )
        };
        // Reclaim the arg boxes (we own them).
        for p in boxes {
            // SAFETY: each `p` came from `Box::into_raw` just above and was not freed.
            drop(unsafe { Box::from_raw(p) });
        }
        if status == AS_OK {
            // Take ownership of the out handle (the callback transferred it).
            let value = if out.is_null() {
                AsValue::nil()
            } else {
                // SAFETY: `out` came from a CValue box the callback created via a value
                // constructor (or our `store_value`), ownership transferred to us.
                let boxed = unsafe { Box::from_raw(out) };
                boxed.v
            };
            Ok(value)
        } else {
            // Read + free the error message if the callback supplied one.
            let msg = if err.is_null() {
                format!("host function failed (status {status})")
            } else {
                // SAFETY: `err` came from `as_string_free`-able `CString::into_raw`.
                let cstr = unsafe { std::ffi::CStr::from_ptr(err) };
                let s = cstr.to_string_lossy().into_owned();
                unsafe {
                    let _ = CString::from_raw(err);
                }
                s
            };
            // A fallible fn's recoverable error → the Tier-1 pair (via Recoverable); a
            // plain fn upgrades Recoverable to Tier-2 (the embed builder's rule). Either
            // way `Recoverable` is the right carrier; the builder form decides the tier.
            Err(HostError::Recoverable(msg))
        }
    }
}

// ── test-only panic injection (Task 4.1) ─────────────────────────────────────

/// `#[cfg(test)]`-only: inject an internal panic to exercise the poison path. NOT in the
/// header, NOT exported in release builds.
#[cfg(test)]
#[no_mangle]
pub unsafe extern "C" fn as__test_panic(iso: *mut CIsolate) -> as_status {
    unsafe {
        with_isolate(iso, |_c| {
            panic!("injected test panic");
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(s: &str) -> (Vec<u8>, usize) {
        (s.as_bytes().to_vec(), s.len())
    }

    #[test]
    fn eval_int_roundtrip() {
        unsafe {
            let iso = as_isolate_new();
            assert!(!iso.is_null());
            let (src, len) = cstr("1 + 2");
            let mut out: *mut CValue = std::ptr::null_mut();
            let st = as_eval(iso, src.as_ptr() as *const c_char, len, &mut out);
            assert_eq!(st, AS_OK, "eval status");
            assert!(!out.is_null());
            let mut n: i64 = 0;
            assert_eq!(as_value_int(out, &mut n), AS_OK);
            assert_eq!(n, 3);
            // kind is Int
            let mut k: c_int = -1;
            assert_eq!(as_value_kind(out, &mut k), AS_OK);
            assert_eq!(k, AS_KIND_INT);
            as_value_free(out);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn eval_error_sets_last_error() {
        unsafe {
            let iso = as_isolate_new();
            // A runtime panic: call a missing method on an int.
            let (src, len) = cstr("let x = 1\nx.nope()");
            let mut out: *mut CValue = std::ptr::null_mut();
            let st = as_eval(iso, src.as_ptr() as *const c_char, len, &mut out);
            assert!(
                st == AS_ERR_PANIC || st == AS_ERR_COMPILE,
                "expected panic/compile, got {st}"
            );
            // last_error non-empty
            let mut msg: *const c_char = std::ptr::null();
            let mut mlen: usize = 0;
            assert_eq!(as_last_error(iso, &mut msg, &mut mlen), AS_OK);
            assert!(mlen > 0, "last_error must be non-empty after an error");
            as_isolate_free(iso);
        }
    }

    #[test]
    fn wrong_thread_eval_is_checked() {
        unsafe {
            let iso = as_isolate_new();
            let addr = iso as usize;
            let st = std::thread::spawn(move || {
                let iso = addr as *mut CIsolate;
                let (src, len) = cstr("1");
                let mut out: *mut CValue = std::ptr::null_mut();
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out)
            })
            .join()
            .unwrap();
            assert_eq!(st, AS_ERR_WRONG_THREAD);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn wrong_thread_value_free_leaks_no_crash() {
        unsafe {
            let iso = as_isolate_new();
            let (src, len) = cstr("42");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            let addr = out as usize;
            // Free from another thread: must NOT crash; the box leaks (documented contract).
            std::thread::spawn(move || {
                as_value_free(addr as *mut CValue);
            })
            .join()
            .unwrap();
            // The handle is still alive on this thread (leaked, not freed) — read it.
            let mut n: i64 = 0;
            assert_eq!(as_value_int(out, &mut n), AS_OK);
            assert_eq!(n, 42);
            // Free correctly on the owning thread now.
            as_value_free(out);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn poisoning_then_poisoned_then_free_ok() {
        unsafe {
            let iso = as_isolate_new();
            // Inject an internal panic → AS_ERR_INTERNAL.
            assert_eq!(as__test_panic(iso), AS_ERR_INTERNAL);
            // The NEXT call → AS_ERR_POISONED.
            let (src, len) = cstr("1");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_ERR_POISONED
            );
            // last_error still readable.
            let mut msg: *const c_char = std::ptr::null();
            let mut mlen: usize = 0;
            assert_eq!(as_last_error(iso, &mut msg, &mut mlen), AS_OK);
            // free still works.
            as_isolate_free(iso);
        }
    }

    #[test]
    fn null_args_are_config_errors() {
        unsafe {
            // NULL isolate → AS_ERR_CONFIG (never deref'd).
            let mut out: *mut CValue = std::ptr::null_mut();
            let (src, len) = cstr("1");
            assert_eq!(
                as_eval(std::ptr::null_mut(), src.as_ptr() as *const c_char, len, &mut out),
                AS_ERR_CONFIG
            );
            // NULL src on a real isolate → AS_ERR_CONFIG.
            let iso = as_isolate_new();
            assert_eq!(as_eval(iso, std::ptr::null(), 0, &mut out), AS_ERR_CONFIG);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn invalid_utf8_is_utf8_error() {
        unsafe {
            let iso = as_isolate_new();
            let bad = [0xff_u8, 0xfe, 0xfd];
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, bad.as_ptr() as *const c_char, bad.len(), &mut out),
                AS_ERR_UTF8
            );
            as_isolate_free(iso);
        }
    }

    #[test]
    fn type_mismatch_read() {
        unsafe {
            let iso = as_isolate_new();
            let (src, len) = cstr("\"hello\"");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            let mut n: i64 = 0;
            assert_eq!(as_value_int(out, &mut n), AS_ERR_TYPE);
            // but as_value_string works.
            let mut ptr: *const c_char = std::ptr::null();
            let mut slen: usize = 0;
            assert_eq!(as_value_string(out, &mut ptr, &mut slen), AS_OK);
            assert_eq!(slen, 5);
            as_value_free(out);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn version_and_abi() {
        assert_eq!(ascript_abi_version(), ASCRIPT_CAPI_ABI);
        assert!(ascript_version() >= (6u32 << 8));
    }

    // ── Task 4.2 ─────────────────────────────────────────────────────────────

    /// A test C callback playing the host side: reads userdata as an `i64` bias, adds it
    /// to arg0 (an int), returns the sum. Demonstrates userdata round-trip.
    unsafe extern "C" fn add_bias(
        userdata: *mut c_void,
        _iso: *mut CIsolate,
        args: *const *const CValue,
        nargs: usize,
        out: *mut *mut CValue,
        _err: *mut *mut c_char,
    ) -> as_status {
        let bias = *(userdata as *const i64);
        if nargs < 1 || args.is_null() {
            return AS_ERR_CONFIG;
        }
        let a0 = *args;
        let mut n: i64 = 0;
        if as_value_int(a0, &mut n) != AS_OK {
            return AS_ERR_TYPE;
        }
        *out = as_int(n + bias);
        AS_OK
    }

    /// A fallible C callback: returns an error status + message when arg0 == 0.
    unsafe extern "C" fn fail_on_zero(
        _userdata: *mut c_void,
        _iso: *mut CIsolate,
        args: *const *const CValue,
        nargs: usize,
        out: *mut *mut CValue,
        err: *mut *mut c_char,
    ) -> as_status {
        if nargs < 1 {
            return AS_ERR_CONFIG;
        }
        let mut n: i64 = 0;
        let _ = as_value_int(*args, &mut n);
        if n == 0 {
            let m = CString::new("zero not allowed").unwrap();
            *err = m.into_raw();
            return AS_ERR_PANIC; // any non-OK status
        }
        *out = as_int(n * 10);
        AS_OK
    }

    #[test]
    fn host_fn_userdata_roundtrip() {
        unsafe {
            let iso = as_isolate_new();
            // userdata: a heap i64 bias of 100 (kept alive for the isolate's life).
            let bias: Box<i64> = Box::new(100);
            let ud = Box::into_raw(bias) as *mut c_void;
            let (m, ml) = cstr("host:app");
            let (f, fl) = cstr("addBias");
            assert_eq!(
                as_register_host_fn(
                    iso,
                    m.as_ptr() as *const c_char,
                    ml,
                    f.as_ptr() as *const c_char,
                    fl,
                    Some(add_bias),
                    ud,
                    0,
                ),
                AS_OK
            );
            let (src, len) = cstr("import * as app from \"host:app\"\napp.addBias(7)");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            let mut n: i64 = 0;
            assert_eq!(as_value_int(out, &mut n), AS_OK);
            assert_eq!(n, 107, "100 bias + 7");
            as_value_free(out);
            as_isolate_free(iso);
            // Reclaim the userdata box.
            drop(Box::from_raw(ud as *mut i64));
        }
    }

    #[test]
    fn host_fn_tier_mapping() {
        unsafe {
            let iso = as_isolate_new();
            let (m, ml) = cstr("host:app");
            // tier 1 (fallible) → script sees [nil, err].
            let (f, fl) = cstr("checked");
            assert_eq!(
                as_register_host_fn(
                    iso,
                    m.as_ptr() as *const c_char,
                    ml,
                    f.as_ptr() as *const c_char,
                    fl,
                    Some(fail_on_zero),
                    std::ptr::null_mut(),
                    1,
                ),
                AS_OK
            );
            let prog = "import * as app from \"host:app\"\n\
                        let [v, e] = app.checked(0)\n\
                        print(v == nil, e.message)";
            let (src, len) = cstr(prog);
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            let mut o: *mut c_char = std::ptr::null_mut();
            let mut ol: usize = 0;
            assert_eq!(as_take_output(iso, &mut o, &mut ol), AS_OK);
            let got = std::ffi::CStr::from_ptr(o).to_string_lossy().into_owned();
            as_string_free(o);
            assert_eq!(got.trim(), "true zero not allowed");
            as_value_free(out);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn host_fn_plain_tier_panics() {
        unsafe {
            let iso = as_isolate_new();
            let (m, ml) = cstr("host:app");
            let (f, fl) = cstr("strict");
            // tier 0 (plain) → an error upgrades to a Tier-2 panic.
            assert_eq!(
                as_register_host_fn(
                    iso,
                    m.as_ptr() as *const c_char,
                    ml,
                    f.as_ptr() as *const c_char,
                    fl,
                    Some(fail_on_zero),
                    std::ptr::null_mut(),
                    0,
                ),
                AS_OK
            );
            let (src, len) = cstr("import * as app from \"host:app\"\napp.strict(0)");
            let mut out: *mut CValue = std::ptr::null_mut();
            let st = as_eval(iso, src.as_ptr() as *const c_char, len, &mut out);
            assert_eq!(st, AS_ERR_PANIC, "plain-fn error is a Tier-2 panic");
            as_isolate_free(iso);
        }
    }

    #[test]
    fn late_registration_after_import_errors() {
        unsafe {
            let iso = as_isolate_new();
            let (m, ml) = cstr("host:app");
            let (f, fl) = cstr("a");
            // Register one fn, import the module.
            assert_eq!(
                as_register_host_fn(
                    iso,
                    m.as_ptr() as *const c_char,
                    ml,
                    f.as_ptr() as *const c_char,
                    fl,
                    Some(add_bias),
                    Box::into_raw(Box::new(0i64)) as *mut c_void,
                    0,
                ),
                AS_OK
            );
            let (src, len) = cstr("import * as app from \"host:app\"\n1");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            as_value_free(out);
            // Now a SECOND registration on the already-imported module → AS_ERR_CONFIG.
            let (g, gl) = cstr("b");
            let st = as_register_host_fn(
                iso,
                m.as_ptr() as *const c_char,
                ml,
                g.as_ptr() as *const c_char,
                gl,
                Some(add_bias),
                Box::into_raw(Box::new(0i64)) as *mut c_void,
                0,
            );
            assert_eq!(st, AS_ERR_CONFIG, "late reg after import is rejected");
            as_isolate_free(iso);
        }
    }

    #[test]
    fn json_bridge_roundtrip() {
        unsafe {
            let iso = as_isolate_new();
            // Parse → a value → serialize back.
            let (j, jl) = cstr(r#"{"a":1,"b":[2,3]}"#);
            let mut v: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_json_parse(iso, j.as_ptr() as *const c_char, jl, &mut v),
                AS_OK
            );
            let mut k: c_int = -1;
            assert_eq!(as_value_kind(v, &mut k), AS_OK);
            assert_eq!(k, AS_KIND_OBJECT);
            let mut out: *mut c_char = std::ptr::null_mut();
            let mut ol: usize = 0;
            assert_eq!(as_value_to_json(iso, v, &mut out, &mut ol), AS_OK);
            let got = std::ffi::CStr::from_ptr(out).to_string_lossy().into_owned();
            as_string_free(out);
            assert_eq!(got, r#"{"a":1,"b":[2,3]}"#);
            as_value_free(v);
            as_isolate_free(iso);
        }
    }

    #[test]
    fn take_output_drains() {
        unsafe {
            let iso = as_isolate_new();
            let (src, len) = cstr("print(\"hi\")");
            let mut out: *mut CValue = std::ptr::null_mut();
            assert_eq!(
                as_eval(iso, src.as_ptr() as *const c_char, len, &mut out),
                AS_OK
            );
            as_value_free(out);
            let mut o: *mut c_char = std::ptr::null_mut();
            let mut ol: usize = 0;
            assert_eq!(as_take_output(iso, &mut o, &mut ol), AS_OK);
            let got = std::ffi::CStr::from_ptr(o).to_string_lossy().into_owned();
            as_string_free(o);
            assert_eq!(got, "hi\n");
            // A second drain is empty (drained).
            let mut o2: *mut c_char = std::ptr::null_mut();
            let mut ol2: usize = 0;
            assert_eq!(as_take_output(iso, &mut o2, &mut ol2), AS_OK);
            assert_eq!(ol2, 0);
            as_string_free(o2);
            as_isolate_free(iso);
        }
    }
}
