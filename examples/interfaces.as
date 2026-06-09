// Structural interfaces (IFACE) — a named method SET; a value CONFORMS if it
// structurally has those methods. No inheritance required, retroactive by
// construction, and a value may conform to arbitrarily many interfaces at once.
//
// This example shows the RUNTIME half that ships now: structural conformance via
// `instanceof`, optional `implements` (asserted intent, same predicate),
// composition via `extends`, and an interface-typed function parameter as a
// runtime contract. Static interface TYPE-checking is a later milestone (TYPE).

// A method requirement is a signature with NO body. `b` is the buffer; the return
// is the byte count.
interface Reader {
  fn read(b): int
}

interface Writer {
  fn write(b): int
}

// Composition: ReadWriter requires the UNION of Reader's and Writer's methods.
interface ReadWriter extends Reader, Writer {
}

// IMPLICIT (structural) conformance — File never names Reader, but it conforms
// because it has a matching `read`.
class File {
  fn read(b): int {
    return len(b)
  }
}

// EXPLICIT conformance — Socket ASSERTS it conforms to ReadWriter. At runtime the
// `implements` clause is documentation only; `instanceof` still runs the same
// structural check.
class Socket implements ReadWriter {
  fn read(b): int {
    return len(b)
  }
  fn write(b): int {
    return len(b)
  }
}

// A purely structural value: NullSink has a `write` but no `implements` clause —
// it still conforms to Writer.
class NullSink {
  fn write(b): int {
    return len(b)
  }
}

// An interface-typed parameter is a runtime CONTRACT: a non-conforming argument is
// rejected the same way a class annotation would reject it.
fn copy(src: Reader, dst: Writer): int {
  let n = src.read([1, 2, 3])
  return dst.write([1, 2, 3, n])
}

let f = File()
let s = Socket()
let sink = NullSink()

// Structural `instanceof` — true iff the value's class exposes every required
// method (by name + arity in v1).
print(f instanceof Reader) // true  (structural — File never said `implements`)
print(s instanceof Reader) // true  (Socket has read via ReadWriter)
print(s instanceof Writer) // true
print(s instanceof ReadWriter) // true  (composition: both read + write)
print(sink instanceof Writer) // true  (structural)
print(f instanceof Writer) // false (File has no write)

// A non-instance is always false, never an error.
print(42 instanceof Reader) // false
print(nil instanceof Reader) // false

// Interface-typed parameters accept any conforming value.
print(copy(f, s)) // 4
print(copy(s, sink)) // 4

// Dispatch on conformance at runtime.
fn kind(x): string {
  if (x instanceof ReadWriter) {
    return "read-writer"
  }
  if (x instanceof Reader) {
    return "reader"
  }
  if (x instanceof Writer) {
    return "writer"
  }
  return "neither"
}

print(kind(f)) // reader
print(kind(s)) // read-writer
print(kind(sink)) // writer
print(kind(42)) // neither
print("interfaces ok")
