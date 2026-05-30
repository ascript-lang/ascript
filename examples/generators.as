//! Generators & coroutines (M17 Phase 4): fn*, yield, gen.next, for await.
//!
//! Run me: `cargo run -- run examples/generators.as`

import * as time from "std/time"

// A generator (`fn*`) produces a sequence lazily: nothing runs until it is
// driven. `for await` consumes it value by value.
fn* count(n) {
  let i = 1
  while (i <= n) {
    yield i
    i = i + 1
  }
}

for await (x in count(3)) {
  print(x)
}

// Bidirectional coroutine: `yield` evaluates to the value the consumer passes
// to `gen.next(v)`, so a generator can both produce and receive.
fn* echo() {
  let a = yield "ready"
  print(a)
  let b = yield "more"
  print(b)
}

let g = echo()
print(g.next())      // "ready" (first next starts the body)
print(g.next("one")) // prints "one", yields "more"
g.next("two")        // prints "two", then the generator ends

// Async generators may await between yields, and they compose: `doubled`
// consumes another async generator and re-yields transformed values.
async fn* ticks() {
  yield 1
  await time.sleep(1)
  yield 2
  yield 3
}

async fn* doubled(src) {
  for await (n in src) {
    yield n * 2
  }
}

async fn main() {
  for await (v in doubled(ticks())) {
    print(v)
  }
}

await main()
