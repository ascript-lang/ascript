// std/ffi — call C math functions from the platform's libm across the C ABI.
//
// FFI opens a shared library (`ffi.open` → dlopen), binds a symbol with a C
// signature (`lib.symbol(name, argtypes, rettype)`), and invokes it
// (`sym.call(args)`). Sized C types are described as `ffi.f64`/`ffi.i32`/… and
// marshalled over AScript's `int`/`float` — there is no separate sized-int kind.
//
// HERMETIC + CROSS-PLATFORM: the library NAME differs per OS, so we resolve it from
// `os.platform()` and probe with `ffi.open` (a Tier-1 `[lib, err]` — a missing
// library is recoverable DATA, not a crash). The example degrades gracefully if no
// libm is present, and the math results we pick (`pow(2,10)=1024`, `sqrt(144)=12`,
// `cos(0)=1`) are BIT-EXACT on every platform, so the output is deterministic.
import * as ffi from "std/ffi"
import * as os from "std/os"

// The platform's libm shared-object name.
fn libm_name(): string {
  let p = os.platform()
  if (p == "macos") {
    return "libSystem.B.dylib"
  }
  if (p == "windows") {
    return "msvcrt.dll"
  }
  return "libm.so.6"
}

fn main() {
  let [libm, openErr] = ffi.open(libm_name())
  if (openErr != nil) {
    // No libm on this host — report and stop (recoverable, not a panic).
    print("libm unavailable: " + openErr.message)
    return
  }

  // pow(2.0, 10.0) -> 1024.0  (two f64 in, f64 out)
  let [pow, e1] = libm.symbol("pow", [ffi.f64, ffi.f64], ffi.f64)
  if (e1 != nil) {
    print("no pow: " + e1.message)
    return
  }
  print(pow.call([2.0, 10.0])) // 1024.0

  // sqrt(144.0) -> 12.0
  let [sqrt, e2] = libm.symbol("sqrt", [ffi.f64], ffi.f64)
  if (e2 != nil) {
    print("no sqrt: " + e2.message)
    return
  }
  print(sqrt.call([144.0])) // 12.0

  // cos(0.0) -> 1.0
  let [cos, e3] = libm.symbol("cos", [ffi.f64], ffi.f64)
  if (e3 != nil) {
    print("no cos: " + e3.message)
    return
  }
  print(cos.call([0.0])) // 1.0
}

main()
