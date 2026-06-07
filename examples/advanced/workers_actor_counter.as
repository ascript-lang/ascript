// Stateful counter actor: spawn a `worker class` into its own isolate,
// call methods sequentially via the async proxy handle, and observe that state
// persists across calls (the isolate keeps running between messages).
//
// The actor also maintains a small key→value cache to show that arbitrary
// mutable state survives across method invocations — the classic actor invariant.
//
// Expected output (both VM and tree-walker, byte-identical):
//   1
//   2
//   42
//   42
//   3
worker class Counter {
  n: number = 0
  cache: any? = nil
  fn init() {
    self.cache = {}
  }
  fn inc(): number {
    self.n = self.n + 1
    return self.n
  }
  fn remember(k: string, v: number): number {
    self.cache[k] = v
    return self.cache[k]
  }
  fn lookup(k: string): any {
    return self.cache[k]
  }
}

async fn main() {
  let c = await Counter.spawn()
  print(await c.inc()) // 1
  print(await c.inc()) // 2
  print(await c.remember("x", 42)) // 42
  print(await c.lookup("x")) // 42
  print(await c.inc()) // 3 — state persisted across all calls
  c.close()
}

await main()
