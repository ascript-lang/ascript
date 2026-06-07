// Service actor: a `worker class` that owns an in-isolate mock "database"
// (a plain object acting as a key-value store — no external dependencies).
//
// Demonstrates the "resource lives in the actor" pattern: the store is opened
// inside the isolate and never crosses the boundary — only data does.
// Also demonstrates full error-handling via [value, err] Result pairs and
// `recover` on a method that panics.
//
// Expected output (both VM and tree-walker, byte-identical):
//   10
//   32
//   42
//   not found: missing
//   caught panic: key must be non-empty
//   true

// A pure helper to simulate a DB lookup: returns [value, err].
// Shipped to the isolate transitively (it is a top-level fn).
fn dbGet(store, key: string): any {
  let v = store[key]
  if (v == nil) {
    return [nil, Err("not found: " + key)[1]]
  }
  return [v, nil]
}

worker class Service {
  store: any? = nil
  total_sum: number = 0
  fn init() {
    // "Open" the in-isolate mock store — pure state, no native resource.
    self.store = {}
  }
  fn put(k: string, v: number): number {
    assert(len(k) > 0, "key must be non-empty")
    self.store[k] = v
    self.total_sum = self.total_sum + v
    return v
  }
  fn get(k: string): any {
    return dbGet(self.store, k)
  }
  fn total(): number {
    return self.total_sum
  }
}

async fn main() {
  let s = await Service.spawn()

  // Normal put / get / total sequence.
  print(await s.put("a", 10)) // 10
  print(await s.put("b", 32)) // 32
  print(await s.total()) // 42

  // Typed get: returns a [value, err] pair that crosses the boundary as data.
  let [v, e] = await s.get("missing")
  if (e != nil) {
    print(e.message)
  }

  // Panic recovery: put with an empty key asserts inside the actor.
  let caught = recover(() => await s.put("", 99))
  if (caught[1] != nil) {
    print("caught panic: " + caught[1].message)
  }
  // Actor survives the panic — still responds correctly.
  print(await s.total() == 42) // true
  s.close()
}

await main()
