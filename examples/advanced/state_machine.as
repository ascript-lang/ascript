// A state machine driven by a payload-carrying event enum — the canonical ADT
// example. `Event` is a sum of typed events; `State` is a sum of typed states; the
// transition function is one exhaustive `match` over both, so a forgotten event or
// state is a COMPILE error (`non-exhaustive-match`), not a runtime surprise.
import * as array from "std/array"

// The inputs: each event carries the data it needs. `KeyPress` is a single-field
// positional payload; `Resize` is a multi-field named payload; `Quit` is a unit
// variant (no payload).
enum Event {
  KeyPress(int),
  Resize(w: int, h: int),
  Quit,
}

// The machine's states. `Running` carries the current viewport; `Stopped` is unit.
enum State {
  Running(width: int, height: int),
  Stopped,
}

// The transition function: a total function over (state, event). Both `match`es are
// exhaustive over their enum — note the unit variants are written QUALIFIED
// (`State.Stopped`, `Event.Quit`) so they MATCH the variant rather than binding the
// subject (the documented ADT rule; a bare `Stopped =>` would shadow-bind).
fn step(s: State, e: Event): State {
  return match s {
    State.Stopped => State.Stopped,
    Running(width: w, height: h) => match e {
      KeyPress(code) if code == 27 => State.Stopped,
      KeyPress(_) => State.Running(width: w, height: h),
      Resize(nw, nh) => State.Running(width: nw, height: nh),
      Event.Quit => State.Stopped,
    },
  }
}

// Render a state for display — another exhaustive `match`, this time producing a
// string. `State.Stopped` is qualified; `Running` destructures its fields.
fn describe(s: State): string {
  return match s {
    Running(width: w, height: h) => `running ${w}x${h}`,
    State.Stopped => "stopped",
  }
}

// Drive the machine through a sequence of events, folding the state and recording
// each step's description.
fn run(initial: State, events: array<Event>): array<string> {
  let trace = [describe(initial)]
  let s = initial
  for (e of events) {
    s = step(s, e)
    array.push(trace, describe(s))
  }
  return trace
}

// A scripted run: a no-op keypress, a resize, then Esc (which stops the machine),
// then a resize that is ignored because the machine is already stopped.
let events = [Event.KeyPress(65), Event.Resize(w: 100, h: 40), Event.KeyPress(27), Event.Resize(w: 10, h: 10)]

let trace = run(State.Running(width: 80, height: 24), events)
for (line of trace) {
  print(line) // running 80x24 / running 80x24 / running 100x40 / stopped / stopped
}

// Constructing a variant with a wrong payload type is a recoverable Tier-2 error,
// surfaced as a `[value, err]` pair by `recover` — no unwinding, the program
// continues. The error message names the variant whose field failed. (The bad
// value flows through an untyped helper so the example stays a clean, zero-
// diagnostic program: the static checker only flags a PROVABLY-wrong arg.)
fn untyped(x) {
  return x
}
let bad = recover(() => Event.KeyPress(untyped("not an int")))
print(bad[0]) // nil
print(bad[1].message) // Event.KeyPress: expected int, got string

// A multi-field named variant constructed positionally is likewise a recoverable
// error (named variants require named args to avoid positional ambiguity).
let bad2 = recover(() => Event.Resize(untyped(100), untyped(40)))
print(bad2[1].message) // Event.Resize requires named fields (w:, h:)
