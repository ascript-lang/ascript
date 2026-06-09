# `std/ffi` — foreign function interface (call C across the ABI)

`std/ffi` lets AScript call into an arbitrary C shared library at runtime — open a
`.so`/`.dylib`/`.dll`, look up a symbol, marshal arguments across the C ABI, and get
the result back as an ordinary AScript value. It is the system-access primitive that
lets the ecosystem wrap any C library without a Rust recompile, putting AScript next
to Java (JNI/Panama), C# (P/Invoke), Swift, and Go (cgo).

FFI is the most dangerous capability in the language — a wrong signature or a bad
pointer is memory-unsafe in a way nothing else in AScript is. Its safety comes not
from being compiled out, but from the **per-isolate capability gate** (see
[Capabilities](caps)): `ffi` is granted by default but subtractable, and the place you
most often want to deny it is an untrusted plugin worker. FFI and capabilities are
co-designed for exactly this reason.

> Feature: `std/ffi` is gated on the default-on `ffi` Cargo feature (it links
> `libloading` + `libffi`). Under `--no-default-features`, `import "std/ffi"` is an
> unknown-module error. The `std/caps` capability model is **core** and present in
> every build.

## A first call

```ascript
import * as ffi from "std/ffi"

let [libm, err] = ffi.open("libm.so.6")          // [ForeignLib, err] — Tier-1 Result
let [pow, e]    = libm.symbol("pow", [ffi.f64, ffi.f64], ffi.f64)
print(pow.call([2.0, 10.0]))                      // 1024.0
```

- **`ffi.open(path) -> [lib, err]`** — `dlopen`. A missing/invalid library is a
  **Tier-1 `[value, err]`** (recoverable data — you may probe for an optional
  library), not a panic.
- **`lib.symbol(name, argtypes, rettype) -> [symbol, err]`** — `dlsym` + a bound C
  signature. A missing symbol is **Tier-1**; a malformed signature (argtypes not
  `ffi.*` descriptors) is a **Tier-2 panic** (a programming bug).
- **`symbol.call(args) -> ret`** — marshal `args` per `argtypes`, invoke through the
  libffi trampoline, marshal the result back. A wrong arg count / out-of-range value
  is a **Tier-2 panic** with a clear message — validated **before** the trampoline, so
  a malformed call never segfaults.

The split — *which library exists on this machine* is Tier-1 data you handle; *calling
a symbol with the wrong shape* is a Tier-2 bug — is deliberate.

## C types, described in AScript

The marshalling vocabulary is a set of descriptor values (tagged objects, not new
value kinds). **Sized C ints marshal over AScript's `int`** — there is no `i32` value
kind, only an `int` and a boundary descriptor that says "this `int` is an `i32` here."

| Descriptor | C type | AScript value | Notes |
|---|---|---|---|
| `ffi.i8 i16 i32 i64` | `int8_t … int64_t` | `int` | narrower widths are range-checked |
| `ffi.u8 u16 u32` | `uint8_t … uint32_t` | `int` | range-checked into `0..=MAX` |
| `ffi.u64` | `uint64_t` | `int` | the i64 **bit pattern** is the value (no sign check) |
| `ffi.size` | `size_t`/`ssize_t` | `int` | pointer-width; bit-pattern like `u64` |
| `ffi.f32 f64` | `float`/`double` | `float` | `f32` narrows out / widens in (precision loss) |
| `ffi.ptr` | `void*`/`T*` | `Bytes` or `ForeignPtr` | a `Bytes` passes its buffer address |
| `ffi.cstr(s)` | `const char*` | from `string` | → a NUL-terminated `Bytes` you pass as `ffi.ptr` |
| `ffi.void` | `void` | `nil` | return type only |

**Narrowing is checked, never silent.** Passing `300` to a `ffi.u8` is a Tier-2 panic
(`ffi: value 300 out of range for u8`). The exception is `u64`/`size`: their width
equals `int`, so the i64 **bit pattern** is the value — passing `-1` marshals as
`0xFFFF_FFFF_FFFF_FFFF` (the standard signed-to-`u64::MAX` idiom), and a `u64`/`size`
return whose top bit is set comes back as a negative `int` (the two's-complement bit
pattern). Round-trips are bit-identical.

## Strings, structs & out-params

```ascript
let [libc, _] = ffi.open("libc.so.6")
let [strlen, _] = libc.symbol("strlen", [ffi.ptr], ffi.size)
print(strlen.call([ffi.cstr("hello")]))      // 5

// structs are a `Bytes` buffer + a layout descriptor
let Point = ffi.struct([["x", ffi.i32], ["y", ffi.f64]])   // x@0, y@8, size 16
let buf = ffi.alloc(Point)                                  // zeroed, C-aligned Bytes
ffi.set(Point, buf, "x", 3)
print(ffi.get(Point, buf, "x"))              // 3
// pass `buf` as a ffi.ptr out-param; a C call writes through it, then read it back
```

- **`ffi.cstr(s)`** → a NUL-terminated `Bytes`. **`ffi.read_cstr(ptr)`** copies a
  returned `const char*` (a `Bytes` or `ForeignPtr`) until the first NUL into a string.
- **`ffi.struct(fields)`** computes C offsets + alignment; **`ffi.alloc(layout)`**
  zeroes a correctly-aligned buffer; **`ffi.get`/`ffi.set`** read/write fields by name.

## The three handles

- **`ForeignLib`** — an open library; its `Drop` `dlclose`s deterministically.
- **`ForeignSymbol`** — a resolved symbol + bound signature; keeps its `Library` alive.
- **`ForeignPtr`** — an opaque C pointer returned by a call (e.g. `malloc`). AScript
  never auto-frees it — ownership is the C library's contract; free it by calling the
  library's own `free`.

All three are native handles: **GC-opaque** (the collector never traces into a foreign
pointer) and **non-sendable** (a pointer is only valid in the address space that
produced it — sending one to a worker is a structured-clone Tier-2 error).

## Threading: a C call stalls the isolate

A C call runs **synchronously** on the calling isolate's single thread (`sym.call`
returns the value directly, not a `future`). AScript is cooperatively scheduled and
`!Send`, so **a blocking C call stalls the whole isolate** — no other task makes
progress until it returns, and a `timeout`/cancel that fires *during* the call cannot
interrupt it (there is no `.await` point inside the trampoline).

> **Offload slow or blocking FFI to a `worker fn`.** Run the whole native call inside a
> worker isolate so the main isolate keeps serving; the result crosses back as ordinary
> structured-clone data (`Bytes`/`int`/`Object` — never a live `ForeignPtr`). This is
> exactly how FFI and [Workers](../language/workers) compose.

## FFI inside a workflow (determinism)

A foreign call is an opaque effect seam. Inside a [workflow](workflow) Record/Replay
context, a value-returning `sym.call` is recorded once (its marshalled return **and**
the post-call contents of any `Bytes` out-param) and replayed **without re-invoking C**
— so a resumed workflow is deterministic and out-params are faithful (not stale). A
call that **returns a pointer** or takes a **`ForeignPtr` out-param** cannot be
recorded across runs and is a loud Tier-2 refusal inside a determinism context — never
a silent wrong replay. The `ffi-nondeterminism` lint (Warning) flags direct `ffi.*`
calls inside a workflow body, steering native work into an `activity`.

## Errors at a glance

| Situation | Tier |
|---|---|
| `ffi.open` / `lib.symbol` failure (no such lib/symbol) | **Tier-1** `[nil, err]` |
| malformed signature, wrong arg count, out-of-range narrowing | **Tier-2** panic |
| `ffi` capability denied | **recoverable Tier-2** `capability 'ffi' denied` |
| pointer-return / `ForeignPtr` out-param inside a workflow | **Tier-2** refusal |

See also: [Capabilities & sandboxing](caps), [Workers & parallelism](../language/workers).
