// Bidirectional worker generator: `gen.next(v)` injects a value back into
// the producer across the isolate boundary. The value becomes the result of
// the suspended `yield` expression inside the producer body.
//
// This enables request/response patterns: the consumer steers the producer
// by sending data back with each demand credit.
//
// Expected output (both VM and tree-walker, byte-identical):
//   start
//   got: 5
//   got: 12
//   total: 17

// The producer yields a prompt, receives a value back via the yield expression,
// and accumulates a running total. The final yield returns the total.
worker fn* accumulate() {
  let total = 0
  let a = yield "start"
  total = total + a
  let b = yield `got: ${a}`
  total = total + b
  yield `got: ${b}`
  yield `total: ${total}`
}

async fn main() {
  let g = accumulate()
  print(await g.next()) // "start"  (first next() has no input)
  print(await g.next(5)) // "got: 5"  (a = 5)
  print(await g.next(12)) // "got: 12" (b = 12)
  print(await g.next()) // "total: 17"
  g.close()
}

await main()
