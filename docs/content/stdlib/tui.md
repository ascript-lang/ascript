:::eyebrow Standard library

# Terminal UI

`std/tui` requires the `tui` Cargo feature (default on). It's a hand-rolled double-buffered terminal renderer over crossterm. Coordinates are 0-based; draws clip silently out of bounds. Fallible terminal I/O is Tier-1; argument misuse / use-after-close is Tier-2.

The module exports two entry points — `init()` and `buffer(width, height)` — each of which hands back a **terminal handle**. Every other operation is a method on that handle: lifecycle/mode switches, drawing primitives, rendering, and event reading. Drawing always targets an in-memory **back buffer**; `flush()` is what actually paints the screen, repainting only the cells that changed since the previous flush.

> [!NOTE] `init()` returns a `[term, err]` pair (destructure it), while `buffer(...)` returns the handle **directly**. The asymmetry is deliberate: `buffer` can only fail on argument misuse, which is a Tier-2 panic, so a pair would be noise.

## std/tui {#stdtui}

### Creating a handle

#### `init()` → `[term, err]`

Queries the real terminal size and builds a handle sized to it. On a non-tty (for example, in CI) the size query falls back to **80×24**. The returned `err` is currently always `nil`; destructure the pair anyway for forward compatibility:

```ascript
import { init } from "std/tui"

let [term, err] = init()
let s = term.size()
print(s.width)   // real terminal width, or 80 on a non-tty
```

#### `buffer(width, height)` → `term`

Builds an **off-screen** handle of exactly `width × height` cells without touching the real terminal. It returns the handle **directly** (not a pair). This is the variant to reach for in tests and anywhere you want a deterministic `dump()`: it supports every drawing method, and `flush()` still runs but writes to stdout (harmless but pointless off-screen — just don't call it).

```ascript
import { buffer } from "std/tui"

let term = buffer(14, 4)
```

> [!TIER2] `width` and `height` must each be an integer in `1..=65535`. A non-number, non-integer, negative, or zero dimension panics. (A zero-sized buffer is degenerate — nothing to draw into — so it's rejected up front rather than handed back unusable.)

### Lifecycle & terminal state

These methods query or mutate the handle. The mode switches (`enterRaw`, `enterAltScreen`, etc.) perform real terminal I/O and are therefore `await`-ed and Tier-1.

- `size()` → `{width, height}` — the back buffer's dimensions as an object.
- `clear()` — reset every cell of the back buffer to a blank space. (Affects the back buffer only; call `flush()` to push the cleared screen.)
- `moveCursor(x, y)` — set the **logical** cursor position. `flush()` parks the real cursor there after painting.
- `await enterRaw()` → `[nil, err]` — enable raw mode.
- `await leaveRaw()` → `[nil, err]` — disable raw mode.
- `await enterAltScreen()` → `[nil, err]` — switch to the alternate screen.
- `await leaveAltScreen()` → `[nil, err]` — switch back to the main screen.
- `await showCursor(show)` → `[nil, err]` — `show` is a boolean: `true` shows the cursor, `false` hides it.
- `close()` / `restore()` — **aliases.** Undo any active raw mode and alternate screen, show the cursor, and **consume the handle**.

> [!TIER1] The mode switches and `close`/`restore` perform fallible terminal I/O and return `[nil, err]` — `err` is `nil` on success, an error value on failure. The handle tracks which modes are active, so `close`/`restore` only undo what was actually enabled.

> [!TIER2] `close()`/`restore()` consume the handle. Any subsequent use — including a second `close()` — is a "use after close" panic. `moveCursor` coordinates must be integers in `0..=65535`; `showCursor` requires a boolean.

### Drawing primitives

All drawing targets the back buffer and is **bounds-clipped**: coordinates or lengths that fall outside the buffer are silently dropped rather than erroring. Each primitive takes an optional trailing `style?` object (see [The style object](#the-style-object)); omit it (or pass `nil`) for defaults.

- `setCell(x, y, char, style?)` — set a single cell. `char` is a string; only its first character is used, and an empty string is a no-op.
- `text(x, y, str, style?)` — write `str` left-to-right from `(x, y)`, one cell per character. It clips at the row's right edge and does **not** wrap.
- `hline(x, y, len, char?, style?)` — a horizontal run `len` cells wide. `char` defaults to `─`.
- `vline(x, y, len, char?, style?)` — a vertical run `len` cells tall. `char` defaults to `│`.
- `box(x, y, w, h, style?)` — a border rectangle (corners `┌┐└┘`, edges `─│`); the interior is left untouched. A `w` or `h` of 1 draws the corresponding single line; 0 draws nothing.
- `fill(x, y, w, h, char, style?)` — fill the `w × h` rectangle with `char`.

> [!NOTE] Drawing methods are synchronous and never error on geometry — out-of-bounds coordinates and over-long lengths simply clip. Nothing reaches the screen until you call `flush()`.

> [!TIER2] Coordinates, lengths, and box/fill dimensions must be integers in `0..=65535`. A bad `style` field (unknown color name, out-of-range index, wrong-length rgb array, non-boolean flag) panics — see below.

### Rendering

#### `await flush()` → `[nil, err]`

Diffs the back buffer against the last-flushed buffer and writes **only the changed cells** to stdout, then parks the real cursor at the logical position. After a write the last-flushed snapshot is synced to the back buffer regardless of the write outcome.

> [!TIER1] `flush()` performs a real stdout write and returns `[nil, err]`. A failed write leaves the back buffer authoritative (the screen may simply lag).

### Events

Both readers `await`, return `[event, err]`, and skip key **Release** events so that a single keypress yields exactly one key object across platforms (Windows and kitty-protocol terminals otherwise emit a Release too). Key **Repeat** events *are* surfaced, so holding a key auto-repeats.

- `await pollEvent(timeoutMs?)` → `[event, err]` — wait up to `timeoutMs` (default `0`) for an event. On timeout it returns `[nil, nil]` (no event, no error).
- `await readEvent()` → `[event, err]` — block until an event arrives.

A `resize` event also resizes the handle's buffers (clearing them); redraw and flush after one.

The returned `event` is an object whose `type` field discriminates the shape:

| `type`     | Fields                                            | Notes |
|------------|---------------------------------------------------|-------|
| `"key"`    | `key`, `ctrl`, `alt`, `shift`                     | `key` is a string, e.g. `"a"`, `"Enter"`, `"Up"`, `"F5"`, `"Esc"`, `"Tab"`, `"Backspace"`. The modifier fields are booleans. |
| `"mouse"`  | `x`, `y`, `kind`, `button`                        | `kind` is `"down"`, `"up"`, `"drag"`, `"moved"`, `"scrollUp"`, `"scrollDown"`, `"scrollLeft"`, or `"scrollRight"`. `button` is `"left"`/`"right"`/`"middle"`, or `nil` when not applicable. |
| `"resize"` | `width`, `height`                                 | The new terminal dimensions. |
| `"focus"`  | `focused`                                         | Boolean: `true` on focus gained, `false` on lost. |
| `"paste"`  | `text`                                            | The pasted string (bracketed paste). |

> [!TIER1] Both readers return `[event, err]`; an underlying read failure surfaces as a non-`nil` `err`. Remember that `pollEvent` returns `[nil, nil]` on timeout — that is *not* an error.

> [!TIER2] A `pollEvent` `timeoutMs` must be an integer in `0..=65535`.

### The style object

Every drawing primitive accepts an optional `style` object with these fields, all optional:

`{fg?, bg?, bold?, underline?, italic?, reverse?}`

- `fg` / `bg` — a color, given as one of:
  - a **name** string: `"black"`, `"red"`, `"green"`, `"yellow"`, `"blue"`, `"magenta"`, `"cyan"`, `"white"`, their `"bright*"` variants (e.g. `"brightred"`), or `"default"` / `"reset"`.
  - an **index** number in `0..=255` (256-color palette).
  - an `[r, g, b]` **array**, each component an integer `0..=255` (24-bit truecolor).
- `bold`, `underline`, `italic`, `reverse` — booleans (default `false`).

A `nil` or missing style means all defaults (reset colors, no attributes). Missing individual fields default the same way.

> [!TIER2] A malformed style field panics: an unknown color name, a color index outside `0..=255`, an rgb array that isn't exactly three integers in range, or a non-boolean attribute flag.

### Debugging

These read the back buffer's characters as text (styling is ignored) — handy for snapshot tests against an off-screen `buffer(...)`.

- `dump()` → `string` — the whole back buffer, rows joined by `\n`, each row's trailing spaces trimmed.
- `dumpRow(y)` → `string` — a single row's characters, trailing spaces trimmed. An out-of-range `y` yields an empty string.

## Examples

### Deterministic off-screen drawing

`buffer(...)` plus `dump()` gives output you can assert against without a real terminal — no raw mode, no alt screen, no `flush()`:

```ascript
import * as tui from "std/tui"

let term = tui.buffer(14, 4)
term.box(0, 0, 14, 4, { fg: "cyan" })
term.text(2, 1, "AScript", { bold: true })
term.text(2, 2, "TUI demo")
print(term.dump())
```

This prints a 14×4 box with the two text lines inside it, each row trimmed of trailing spaces.

### A real-terminal event loop

On a real tty: initialize, enter raw mode and the alternate screen, then loop — redraw the back buffer, flush, and block on an event until the user presses `q`. `restore()` undoes the modes and shows the cursor on the way out.

```ascript
import { init } from "std/tui"

let [term, _] = init()
await term.enterRaw()
await term.enterAltScreen()

let running = true
while running {
  term.clear()
  let s = term.size()
  term.box(0, 0, s.width, s.height, { fg: "cyan" })
  term.text(2, 1, "Press q to quit", { bold: true })
  await term.flush()

  let [ev, err] = await term.readEvent()
  if err != nil {
    running = false
  } else if ev.type == "key" and ev.key == "q" {
    running = false
  }
}

await term.restore()
```
