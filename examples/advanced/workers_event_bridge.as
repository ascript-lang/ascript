// Event bridge: `task.pipe(gen, bus)` fans a worker generator stream onto a
// local events bus. Multiple listeners on the bus all receive each event in
// arrival order, with demand-driven backpressure threading back to the producer.
//
// The worker fn* yields objects whose `kind` field determines which listeners
// fire. Here two listener types ("item" and "end") each have two subscribers,
// demonstrating the fan-out property.
//
// Expected output (both VM and tree-walker, byte-identical):
//   listenerA saw: 1
//   listenerB saw: 1
//   listenerA saw: 2
//   listenerB saw: 2
//   listenerA saw: 3
//   listenerB saw: 3
//   done: received 3 items
import { pipe } from "std/task"
import * as events from "std/events"

// Produces a stream of item events followed by a single end event.
worker fn* source(n: number) {
  let i = 1
  while (i <= n) {
    yield {kind: "item", value: i}
    i = i + 1
  }
  yield {kind: "end", count: n}
}

async fn main() {
  let bus = events.new()

  // Two independent "item" listeners — both receive every item in order.
  bus.on("item", (e) => {
    print(`listenerA saw: ${e.value}`)
  })
  bus.on("item", (e) => {
    print(`listenerB saw: ${e.value}`)
  })

  // A single "end" listener that reports the final count.
  bus.on("end", (e) => {
    print(`done: received ${e.count} items`)
  })

  // Pipe the worker stream onto the bus; resolves after all events are delivered.
  await pipe(source(3), bus)
}

await main()
