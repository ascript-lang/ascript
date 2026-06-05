// std/events — an event-emitter / pub-sub (core module).
import { new } from "std/events"

let bus = new()

// `on` registers a listener; `emit` (async) calls each in registration order.
bus.on("greet", (name) => print(`hello ${name}`))
bus.on("greet", (name) => print(`hi ${name}`))
let fired = await bus.emit("greet", "Ada")
assert(fired == 2, "two listeners fired")

// `once` fires exactly once, then removes itself.
bus.once("boot", () => print("booting..."))
await bus.emit("boot")
await bus.emit("boot")   // no-op — already removed
assert(bus.listenerCount("boot") == 0, "once listener gone")

// `off` removes a specific listener by identity.
fn onTick() { print("tick") }
bus.on("tick", onTick)
assert(bus.listenerCount("tick") == 1, "one tick listener")
bus.off("tick", onTick)
assert(bus.listenerCount("tick") == 0, "tick listener removed")

print("events_emitter: all assertions passed")
