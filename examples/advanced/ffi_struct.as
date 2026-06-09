// std/ffi (advanced) — C structs over `Bytes` + a real out-param round-trip.
//
// A C struct is modeled as a `Bytes` buffer plus a LAYOUT descriptor
// (`ffi.struct`) that computes each field's C offset + alignment. You `ffi.alloc`
// a zeroed, correctly-aligned buffer, `ffi.set`/`ffi.get` fields by name, and pass
// the buffer as a `ffi.ptr` out-param to a C function that writes through it.
//
// HERMETIC: we use libc `memset(void*, int, size_t)` to fill the buffer (a real C
// call writing through the passed pointer), then read the bytes back — bit-exact on
// every platform. Fully error-handled (Tier-1 `[value, err]` at every boundary).
import * as ffi from "std/ffi"
import * as os from "std/os"

fn libc_name(): string {
  let p = os.platform()
  if (p == "macos") {
    return "libSystem.B.dylib"
  }
  if (p == "windows") {
    return "msvcrt.dll"
  }
  return "libc.so.6"
}

fn main() {
  // ---- 1. Struct layout + field accessors (pure, no C call) ----
  // struct Point { i32 x; f64 y; }  → x@0, y@8 (8-aligned), size 16.
  let Point = ffi.struct([["x", ffi.i32], ["y", ffi.f64]])
  let buf = ffi.alloc(Point) // zeroed, C-aligned Bytes
  ffi.set(Point, buf, "x", 3)
  ffi.set(Point, buf, "y", 2.5)
  print(ffi.get(Point, buf, "x")) // 3
  print(ffi.get(Point, buf, "y")) // 2.5

  // ---- 2. A real C out-param: memset writes through the Bytes pointer ----
  let [libc, openErr] = ffi.open(libc_name())
  if (openErr != nil) {
    print("libc unavailable: " + openErr.message)
    return
  }
  // memset(void* s, int c, size_t n) — declare the return as ffi.void (we discard
  // the C `void*` return); it fills `n` bytes of `s` with the byte `c`.
  let [memset, symErr] = libc.symbol("memset", [ffi.ptr, ffi.i32, ffi.size], ffi.void)
  if (symErr != nil) {
    print("no memset: " + symErr.message)
    return
  }
  // A fresh 4-byte buffer (a 4×u8 struct); memset it to 0x41 ('A' = 65).
  let Quad = ffi.struct([["b0", ffi.u8], ["b1", ffi.u8], ["b2", ffi.u8], ["b3", ffi.u8]])
  let scratch = ffi.alloc(Quad)
  memset.call([scratch, 65, 4])
  // Read the bytes back via the struct accessors — the C call wrote through our
  // buffer (the out-param round-trip), so every byte is now 65.
  print(ffi.get(Quad, scratch, "b0")) // 65
  print(ffi.get(Quad, scratch, "b3")) // 65
}

main()
