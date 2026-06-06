# AScript Milestone 15 — Terminal UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §11.2 `std/tui`: raw mode, alt screen, screen buffer, key/mouse events, basic widgets & drawing. Backed by `crossterm` (terminal control + events) + a hand-rolled double-buffered screen (cells), under a default-on `tui` Cargo feature.

**Architecture:** A `Terminal` `Value::Native` handle (M13/M14 mechanism) owns a **screen Buffer** (a `width × height` grid of `Cell { ch, fg, bg, attrs }`) plus a "last flushed" buffer. Drawing functions (`text`/`setCell`/`box`/`hline`/`vline`/`fill`) mutate the back buffer; `flush()` computes the per-cell diff and writes ONLY changed cells to the terminal via crossterm (`MoveTo`+`SetColors`+`Print`). Raw mode / alt screen / cursor / events go through crossterm. **The buffer + diff logic + event→object conversion are pure and unit-testable WITHOUT a real terminal**; only the actual stdout writes (raw mode, alt screen, flush output, blocking event read) require a tty and are exercised minimally/gated. Per the spec's "crossterm/ratatui" license, I use crossterm + a hand-rolled cell buffer rather than ratatui's immediate-mode widget API (which doesn't bridge cleanly to a scripting value model) — same pragmatic-subset approach M12 took with icu; documented.

**Tech Stack:** Rust 2021. New crate (feature `tui`, default-on): `crossterm` (terminal control, styling, events). No ratatui (hand-rolled buffer). All synchronous (terminal I/O); `pollEvent` uses crossterm's `event::poll(timeout)` on the current thread.

**Starting state (end of M14, on `main`):** 455 tests default (245 `--no-default`), clippy clean. Resource-handle mechanism (`Value::Native`/`NativeMethod` + `resources` table + `call_native_method` cfg-dispatch) mature (sqlite/process/net all use it). Features: data/datetime/intl/sys/crypto/compress/sql/net (+http3 off).

**Conventions:** single-threaded `Rc`/`RefCell`; `Control` Panic/Propagate; Tier-1 `[value,err]` for fallible terminal I/O (enabling raw mode, flush, event read can fail → err); Tier-2 panic for arg-type misuse + use-after-close; cfg-gated registration; dual-config builds; `run`/`run_err` helpers.

## Semantics decided

- **`Terminal` handle** (`NativeKind::Terminal`): owns the back buffer + the last-flushed buffer + a cursor position + flags (raw/alt active). `type`="terminal", Display `<native terminal #id>`.
- **Coordinates** are 0-based `(x=col, y=row)`, top-left origin. Out-of-bounds draws are clipped (ignored), not errors.
- **Style** object: `{fg, bg, bold, underline, italic, reverse}` (all optional). Colors: a name string ("red","brightblue","black",… the 16 ANSI names) OR `[r,g,b]` (truecolor) OR a number 0-255 (256-color). Missing → default/reset.
- **Events** (`pollEvent`/`readEvent`) → an object: key `{type:"key", key (a readable name like "a"/"Enter"/"Up"/"F1"), ctrl, alt, shift}`; mouse `{type:"mouse", x, y, kind ("down"/"up"/"drag"/"moved"/"scrollUp"/"scrollDown"), button ("left"/"right"/"middle"/nil)}`; resize `{type:"resize", width, height}`. `pollEvent(timeoutMs?)` → `[event|nil, err]` (nil = no event within timeout); `readEvent()` → `[event, err]` (blocks).
- **Fallible-I/O → Tier-1** (raw/alt/flush/events can fail on a non-tty or I/O error); **arg misuse → Tier-2**.

## File structure

| File | Responsibility | Change |
|---|---|---|
| `Cargo.toml` | `tui` feature + `crossterm` | modify |
| `src/value.rs` | `NativeKind::Terminal` | modify |
| `src/interp.rs` | `ResourceState::Terminal(...)` + `call_terminal_method` dispatch arm | modify |
| `src/stdlib/tui.rs` | `std/tui` — Cell/Buffer/Style/event-conv + the module + methods | create |
| `examples/tui.as` | end-to-end example (buffer draw → assert via a buffer-dump, no real tty) | create |
| `tests/cli.rs` | example integration test | modify |

## Scope & Justified Deferrals

| Deferred | Why | Owner |
|---|---|---|
| ratatui's widget framework | Immediate-mode widget API doesn't bridge to a scripting value model; hand-rolled buffer + drawing primitives cover "basic widgets & drawing" per the spec's "crossterm/ratatui" license | n/a (pragmatic subset, documented) |
| LSP | Tooling | **M16** |

Nothing in M15's own scope is deferred. (If a real-tty operation can't be unit-tested in CI, its LOGIC — buffer state it would flush, the event it would parse — is tested via a pure path; the raw stdout write is the only untested sliver, documented.)

---

## Task 1: `tui` feature + `Terminal` handle + screen `Buffer` (crossterm)

**Files:** `Cargo.toml`, `src/value.rs`, `src/interp.rs`, create `src/stdlib/tui.rs`.

- [ ] **Cargo.toml:** `tui = ["dep:crossterm"]` added to `default`; `crossterm = { version = "0.28", optional = true }` (verify resolved version + API; adapt + report). Run `cargo build`.
- [ ] **`src/value.rs`:** `NativeKind::Terminal` (+ type_name "terminal").
- [ ] **`src/interp.rs`:** `ResourceState::Terminal(Box<tui::TerminalState>)` (cfg `tui`; box to keep the enum compact). `call_native_method` arm `#[cfg(feature="tui")] Terminal => self.call_terminal_method(...)`. Add a `terminal_mut(id)` accessor.
- [ ] **`src/stdlib/tui.rs`:** define `Cell { ch: char, fg: Color, bg: Color, attrs: Attrs }` (Color = an enum Reset/Named/Indexed(u8)/Rgb(u8,u8,u8); Attrs = bold/underline/italic/reverse flags), `Buffer { width, height, cells: Vec<Cell> }` (row-major; `cell_mut(x,y)`/`get(x,y)` with bounds-clip), `TerminalState { back: Buffer, flushed: Buffer, cursor: (u16,u16), raw: bool, alt: bool }`.
  - Module fns: `init()→[term, err]` — query terminal size via `crossterm::terminal::size()` (fallback 80×24 if not a tty), build a Terminal handle with buffers sized to it. `size(term)` can be a method.
  - Methods (via `call_terminal_method`): `size()→{width,height}`; `enterRaw()/leaveRaw()→[nil,err]` (`crossterm::terminal::enable/disable_raw_mode`); `enterAltScreen()/leaveAltScreen()→[nil,err]` (`EnterAlternateScreen`/`LeaveAlternateScreen` via `execute!`); `showCursor(bool)→[nil,err]`; `moveCursor(x,y)`; `clear()` (reset the back buffer to blank cells); `restore()/close()` (leave raw+alt if active, show cursor, finalize the resource). Fallible terminal ops → Tier-1.
  - Register cfg-gated `tui` in mod.rs (std path `std/tui`; module fn `init`).
- [ ] **Tests** (no real tty needed for the buffer): unit-test `Buffer::new(w,h)` dims + blank cells; `cell_mut`/`get` bounds-clip (out-of-range get → None / a default, set → no-op); `clear()` resets. A Rust unit test constructing a `TerminalState` directly and checking the buffer. interp e2e: `init()` returns a terminal handle, `type(term)=="terminal"`, `term.size()` returns an object with width/height numbers (size works without a tty via the 80×24 fallback). `cargo test` (both configs — tui cfg's out under no-default) + clippy + `build --no-default-features`. Commit `feat: std/tui terminal handle + screen buffer (crossterm, tui feature)`.

---

## Task 2: Drawing primitives + styling (buffer-mutating, fully testable)

**Files:** `src/stdlib/tui.rs`.

- [ ] Implement drawing methods that mutate the back buffer (all bounds-clipped, no errors on OOB):
  - `setCell(x, y, char, style?)` — set one cell (char = a 1-char string; style optional).
  - `text(x, y, str, style?)` — write a string left-to-right from (x,y), one cell per char (wide chars count as 1 cell for v1 — document; clip at row end).
  - `hline(x, y, len, char?, style?)` / `vline(x, y, len, char?, style?)` — default char a box-drawing line (`─`/`│`) or a given char.
  - `box(x, y, w, h, style?)` — draw a border rectangle (corners `┌┐└┘`, edges `─│`); interior untouched.
  - `fill(x, y, w, h, char, style?)` — fill a rectangle with a char.
  - **Style parsing:** a shared `parse_style(value) -> Style` (Tier-2 panic on a malformed style — e.g. an unknown color name, a non-[r,g,b] array, wrong types). Color name table (16 ANSI names + "default"); `[r,g,b]` → Rgb; number 0-255 → Indexed.
- [ ] **A test-only buffer-dump** to make drawing testable: a method `dump()→string` (or `dumpRow(y)→string`) that returns the buffer's characters as text (rows joined by `\n`), ignoring styling — so tests can assert what was drawn WITHOUT a real terminal. (This is genuinely useful for users too — keep it as a real method, documented as a debug/snapshot aid.) Plus a Rust-level way to inspect a cell's style for the styling tests.
- [ ] Tests (on the buffer, no tty): `text(2,1,"Hi")` then `dump()` shows "Hi" at row 1 col 2; clipping (text past the right edge truncates; negative/huge coords are no-ops); `box(0,0,4,3)` draws the right border characters (assert via dump); `fill(1,1,2,2,"#")` fills the rect; `hline`/`vline`; style parsing — a valid `{fg:"red", bold:true}` applies (inspect the cell's Style in a Rust unit test), an invalid color `{fg:"banana"}` → Tier-2 panic, `[300,0,0]` (out-of-range rgb) → panic. interp e2e: draw a small UI into the buffer and assert via `dump()`. `cargo test` + clippy (both configs) + commit `feat: std/tui drawing primitives + styling`.

---

## Task 3: flush (diff render) + events

**Files:** `src/stdlib/tui.rs`.

- [ ] **`flush()→[nil, err]`:** compute the per-cell diff between `back` and `flushed`; for each changed cell, write to the terminal via crossterm (`queue!`/`execute!` with `MoveTo(x,y)`, `SetForegroundColor`/`SetBackgroundColor`/`SetAttributes`, `Print(ch)`), then copy `back`→`flushed`. Position the cursor afterward. This writes to stdout — fallible → Tier-1. **The DIFF computation is unit-testable** (separate the "compute the list of (x,y,cell) changes" pure fn from the crossterm write); test that after drawing + a notional flush, `flushed` matches `back` and the diff list is correct; a second flush with no changes → empty diff. (The crossterm stdout write itself isn't asserted in CI — it's exercised but its output not captured; document.)
- [ ] **Events:** `pollEvent(timeoutMs?)→[event|nil, err]` (`crossterm::event::poll(Duration)` → if true `event::read()` else nil; default timeout 0 = non-blocking poll, or a given ms); `readEvent()→[event, err]` (`event::read()`, blocks). Convert `crossterm::event::Event` → an AScript object via a **pure `event_to_value(Event) -> Value`** fn (UNIT-TESTABLE without a tty): KeyEvent → `{type:"key", key:<name>, ctrl, alt, shift}` (map crossterm KeyCode → a readable name: chars as themselves, Enter/Esc/Tab/Backspace/Delete/Up/Down/Left/Right/Home/End/PageUp/PageDown/F1..F12/Insert); MouseEvent → `{type:"mouse", x, y, kind, button}`; Resize(w,h) → `{type:"resize", width, height}` (and resize the buffers on a resize event — document whether resize auto-resizes the buffer or just reports). 
- [ ] Tests: the **diff computation** (draw → diff list correct → flushed syncs → no-op second flush); the **`event_to_value` conversion** (construct `crossterm::event::{KeyEvent, MouseEvent, ...}` values directly and assert the resulting AScript object for: a char key with ctrl, Enter, an arrow, F5, a mouse left-down at (3,4), a scroll, a resize) — all WITHOUT a real terminal. `pollEvent` with a 0ms timeout on a non-tty returns nil-or-err gracefully (no panic/hang) — test it returns without hanging. `cargo test` + clippy (both configs) + commit `feat: std/tui flush (diff render) + key/mouse/resize events`.

---

## Task 4: Example + holistic

**Files:** create `examples/tui.as`; modify `tests/cli.rs`.

- [ ] **`examples/tui.as`** — a self-contained, NON-interactive demo that draws into the buffer and prints the `dump()` (so it produces deterministic output WITHOUT needing a real terminal / without entering raw mode). E.g.:
```
import * as tui from "std/tui"
let [term, err] = tui.init()
term.clear()
term.box(0, 0, 12, 4, { fg: "cyan" })
term.text(2, 1, "AScript", { bold: true })
term.text(2, 2, "TUI demo")
print(term.dump())
```
(Do NOT enter raw mode / alt screen / flush in the example — those need a real tty and would garble CI output. The `dump()` of the drawn buffer is deterministic.) RUN it (`cargo run --quiet -- run examples/tui.as`); capture the exact dump output (a 80-wide-or-sized grid showing the box + text — or size the buffer small; consider `init` defaulting to 80×24 → the dump is large; OPTION: add a `tui.buffer(width, height)` constructor that makes a fixed-size off-screen buffer handle WITHOUT querying the terminal, so the example/test output is small + deterministic. RECOMMENDED: add `tui.buffer(w,h)→term-like handle` for off-screen/testable drawing; `init()` is the real-terminal one. Then the example uses `tui.buffer(14,4)` for a tiny deterministic dump.) Decide + implement the off-screen buffer constructor if it makes the example clean; report.
- [ ] Integration test `runs_tui_example` in `tests/cli.rs`, gated `#[cfg(feature="tui")]`, asserting the dump contains "AScript"/"TUI demo" + the box border chars.
- [ ] Conformance: example parses under both parsers. FINAL: `cargo test` (default) + `cargo test --no-default-features` (tui cfg's out) + `cargo clippy --all-targets` (both configs) + `cargo build --no-default-features`. All green/clean/compile. Commit `test: std/tui end-to-end example + integration test`.

---

## Definition of Done

- `cargo test` (default) passes; `cargo clippy --all-targets` clean; `cargo test --no-default-features` passes + `cargo build --no-default-features` compiles (tui cfg out).
- Implemented per spec §11.2 `std/tui`: raw mode, alt screen, screen buffer, key/mouse/resize events, basic widgets & drawing (box/text/line/fill + styling), via crossterm + a hand-rolled double buffer.
- The testable core (buffer ops, diff computation, event→object conversion, style parsing) is unit-tested without a real tty; the raw stdout writes are exercised but not asserted in CI (documented). The pragmatic crossterm-over-ratatui choice is documented.
- Tier-1 for fallible terminal I/O; Tier-2 for arg/style misuse + use-after-close.
- Nothing in M15 scope deferred (ratatui-widgets explicitly out per the spec's pragmatic license).

## Hand-off to Milestone 16 ("Language Server") — the FINAL milestone

M16: `ascript lsp` (`tower-lsp`), an `ascript lsp` CLI subcommand running a language server over stdio, over the shared front-end (lexer/parser → the conformance-tested grammar) + the M9 `SourceInfo`/ariadne diagnostics. Capabilities: diagnostics (lex/parse/contract errors via `AsError`+`SourceInfo`), hover, completion (keywords + the stdlib registry in `std_module_exports`), goto-definition, document symbols. Address the M9 cross-module-diagnostics span-provenance limitation here (thread a module id into spans/`AsError`). Feature `lsp` (or always-on in the CLI). After M16, EVERYTHING in the spec (§§2–16) is implemented — Phase 2+ complete.
