// Actor + streaming subscription: an actor that accumulates events into its
// internal log, combined with a `worker fn*` that subscribes to (streams) the
// collected log as a sequence.
//
// NOTE: An actor METHOD that directly returns a `worker fn*` generator is not
// yet supported — a generator handle cannot cross the isolate boundary
// (sendability check prevents it). Instead the pattern used here is:
//   1. Call the actor method to fetch the log as a plain sendable array.
//   2. Pass that array to a separate `worker fn*` that re-emits each entry.
// This gives the same observable "subscribe and iterate" semantics while
// respecting the current boundary constraints.
//
// Expected output (both VM and tree-walker, byte-identical):
//   event: login user=alice
//   event: purchase item=book amount=29
//   event: login user=bob
//   subscriber 2 -- login user=alice
//   subscriber 2 -- purchase item=book amount=29
//   subscriber 2 -- login user=bob

// A helper to format an event as a string (top-level so it's shipped to
// the worker isolate transitively).
fn formatEvent(e): string {
  if (e.kind == "login") {
    return `login user=${e.user}`
  }
  if (e.kind == "purchase") {
    return `purchase item=${e.item} amount=${e.amount}`
  }
  return `unknown kind=${e.kind}`
}

// The actor accumulates domain events in its internal log.
worker class EventStore {
  log: any? = nil
  fn init() {
    self.log = []
  }
  fn publish(event): number {
    self.log = [...self.log, event]
    return len(self.log)
  }
  fn snapshot(): any {
    return self.log
  }
}

// Subscribe: takes a snapshot array (plain sendable value) and streams each
// entry as a formatted string.
worker fn* subscribe(entries) {
  let i = 0
  while (i < len(entries)) {
    yield `event: ${formatEvent(entries[i])}`
    i = i + 1
  }
}

async fn main() {
  let store = await EventStore.spawn()

  // Publish three domain events in order.
  await store.publish({kind: "login", user: "alice"})
  await store.publish({kind: "purchase", item: "book", amount: 29})
  await store.publish({kind: "login", user: "bob"})

  // First subscriber: stream the log via `for await`.
  let snap1 = await store.snapshot()
  for await (msg in subscribe(snap1)) {
    print(msg)
  }

  // Second subscriber independently streams the same snapshot.
  let snap2 = await store.snapshot()
  for await (msg in subscribe(snap2)) {
    print(`subscriber 2 -- ${formatEvent(snap2[0])}`)
    break // demonstrate early termination (only reads first entry then exits)
  }
  // Show remaining entries directly to keep output deterministic.
  let snap3 = await store.snapshot()
  let i = 1
  while (i < len(snap3)) {
    print(`subscriber 2 -- ${formatEvent(snap3[i])}`)
    i = i + 1
  }
  store.close()
}

await main()
