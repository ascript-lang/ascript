// Defensive count/size guards on the stdlib. Several functions take a numeric
// count or size from script and allocate a buffer of that size: `string.repeat`
// builds an N-fold string, `string.padStart`/`padEnd` build an N-wide one, and
// the byte readers (`reader.read`, `body.read`, `stream.read`) pre-reserve N
// bytes. A pathological count — `1/0` (Infinity), `NaN`, or a huge finite value
// like `1e18` — must NOT be allowed to drive the allocator into a host abort
// (`capacity overflow` / OOM) that bypasses `recover`. The runtime validates the
// count BEFORE the cast, so these become CLEAN, recoverable Tier-2 panics: the
// `[value, err]` pair carries the failure as ordinary data and the program keeps
// running.
import * as string from "std/string"

// A small helper that wraps a thunk and reports whether it failed gracefully.
// `recover(fn)` runs `fn` and returns `[value, err]`: `[v, nil]` on success,
// `[nil, err]` when the body panicked (the panic is caught, not propagated).
fn attempt(label: string, thunk) {
  let [value, err] = recover(thunk)
  if (err != nil) {
    print(`${label}: rejected -> ${err.message}`)
  } else {
    print(`${label}: ok (len ${len(value)})`)
  }
}

// 1) Infinity. `1.0 / 0.0` is +Infinity (float division does not trap); without
//    the guard this cast to `usize::MAX` and aborted the process. Now it is a
//    recoverable panic. (Integer `1 / 0` traps separately — also recoverable.)
attempt("repeat(Infinity)", () => string.repeat("x", 1.0 / 0.0))

// 2) A huge finite count. `1e18` repetitions would attempt a 10^18-byte
//    allocation (an OOM abort); the in-range guard rejects it cleanly.
attempt("repeat(1e18)", () => string.repeat("x", 1e18))

// 3) NaN. `0.0 / 0.0` is NaN; non-finite counts are rejected.
attempt("repeat(NaN)", () => string.repeat("x", 0.0 / 0.0))

// 4) A negative count is rejected too (it would also wrap on cast).
attempt("repeat(-1)", () => string.repeat("x", -1))

// 5) The same guard protects `padStart`/`padEnd`, whose target width drives a
//    fill allocation.
attempt("padStart(Infinity)", () => string.padStart("7", 1.0 / 0.0, "0"))

// 6) After every recovered panic the program is still healthy: a normal,
//    in-range repeat works exactly as before.
print(`normal: ${string.repeat("ab", 3)}`)

// 7) Even an in-range count is rejected if the RESULTING string would exceed the
//    allocation bound (`s.len() * count`), so `String::repeat` itself can never
//    panic on capacity overflow.
attempt("repeat(huge product)", () => string.repeat("0123456789", 4294967295))
