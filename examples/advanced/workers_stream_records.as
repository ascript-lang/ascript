// Worker streaming generator: a `worker fn*` running its producer body in a
// dedicated isolate and streaming structured records back, consumed transparently
// via `for await (x in gen)`.
//
// Each yielded object crosses the isolate boundary via structured-clone
// encode/decode, arriving at the consumer with all fields intact.
// Demand-driven backpressure: the producer advances only when the consumer
// asks for the next value.
//
// Expected output (both VM and tree-walker, byte-identical):
//   1:rec-1
//   2:rec-2
//   3:rec-3
//   4:rec-4
worker fn* records(n: number) {
  let i = 1
  while (i <= n) {
    yield {id: i, label: `rec-${i}`}
    i = i + 1
  }
}

async fn main() {
  for await (r in records(4)) {
    print(`${r.id}:${r.label}`)
  }
}

await main()
