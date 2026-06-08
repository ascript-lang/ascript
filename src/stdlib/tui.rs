//! `std/tui` — terminal control + a hand-rolled double-buffered screen (feature
//! `tui`), spec §11.2. Backed by `crossterm` 0.28 for raw mode / alt screen /
//! cursor / styling; the screen `Buffer` (a `width × height` grid of `Cell`s) is
//! hand-rolled so it bridges cleanly to AScript's value model.
//!
//! Task 1 (this file's first slice) lands the `Terminal` `Value::Native` handle
//! plus the screen `Buffer`/`Cell` types and the terminal lifecycle methods
//! (size / clear / raw / alt screen / cursor / restore). The buffer, the size
//! query, and the close lifecycle are unit-testable WITHOUT a real tty (`size()`
//! falls back to an 80x24 default on a non-tty); drawing + styling arrive in Task 2;
//! Task 3 adds the diff-based `flush` plus key/mouse/resize events.
//!
//! The testable core stays pure + tty-free: [`Buffer::diff`] computes the minimal
//! set of changed cells and [`event_to_value`] converts a crossterm `Event` to an
//! AScript object — both unit-tested directly. The actual stdout write
//! ([`flush_changes`]) is exercised but its escape output isn't asserted in CI.
//! `pollEvent`/`readEvent` surface only key Press/Repeat events (never Release) via
//! the pure [`surfaces`] predicate, so a single keypress yields one key object on
//! every platform (Windows / kitty-protocol terminals emit a Release otherwise).
//!
//! Tier policy (spec §11.3): fallible terminal I/O (enable/disable raw mode, alt
//! screen, cursor show/hide) → Tier-1 `[nil, err]`; argument-type misuse and
//! use-after-close → Tier-2 panic.

use super::bi;
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use std::io::Write as _;
use std::rc::Rc;

// ---- Cell / Color / Attrs ----

/// A cell's color: a reset/default, a 256-color index, or 24-bit truecolor.
/// `Named` carries the resolved crossterm color for an ANSI color name (the name
/// table itself lands with `parse_style` in Task 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    #[default]
    Reset,
    Named(crossterm::style::Color),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    /// The crossterm color this maps to (for flushing).
    pub fn to_crossterm(self) -> crossterm::style::Color {
        match self {
            Color::Reset => crossterm::style::Color::Reset,
            Color::Named(c) => c,
            Color::Indexed(n) => crossterm::style::Color::AnsiValue(n),
            Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
        }
    }
}

/// Text attribute flags for a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attrs {
    pub bold: bool,
    pub underline: bool,
    pub italic: bool,
    pub reverse: bool,
}

/// A resolved drawing style: foreground/background colors plus attribute flags.
/// Produced by [`parse_style`] from an AScript `{fg?, bg?, bold?, ...}` object and
/// applied to each cell a draw touches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

/// One screen cell: a character plus its foreground/background colors and attrs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Default for Cell {
    /// A blank cell: a space with reset colors and no attributes.
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Reset,
            bg: Color::Reset,
            attrs: Attrs::default(),
        }
    }
}

// ---- Buffer ----

/// A row-major `width × height` grid of `Cell`s. Out-of-bounds accessors return
/// `None` so draws clip silently rather than erroring (spec §11.2 coordinates).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Buffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

impl Buffer {
    /// A blank buffer of `width × height` cells.
    pub fn new(width: u16, height: u16) -> Self {
        let count = width as usize * height as usize;
        Buffer {
            width,
            height,
            cells: vec![Cell::default(); count],
        }
    }

    /// Row-major flat index for `(x, y)`, or `None` if out of bounds.
    pub fn idx(&self, x: u16, y: u16) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// Borrow the cell at `(x, y)`, or `None` if out of bounds.
    #[allow(dead_code)] // used by Task 2's flush diff + tests.
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        self.idx(x, y).map(|i| &self.cells[i])
    }

    /// Mutably borrow the cell at `(x, y)`, or `None` if out of bounds (for
    /// bounds-clipped draws).
    #[allow(dead_code)] // used by Task 2's drawing primitives.
    pub fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.idx(x, y).map(move |i| &mut self.cells[i])
    }

    /// Reset every cell to blank.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
    }

    // ---- drawing primitives (all bounds-clipped; OOB coords/lengths are no-ops) ----

    /// Set the cell at `(x, y)` to `ch` with `style`. Out-of-bounds is a no-op.
    pub fn set_cell(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if let Some(c) = self.cell_mut(x, y) {
            c.ch = ch;
            c.fg = style.fg;
            c.bg = style.bg;
            c.attrs = style.attrs;
        }
    }

    /// Write `s` left-to-right from `(x, y)`, one cell per `char` (wide/combining
    /// chars count as one cell in v1), clipping at the row's right edge — it does
    /// not wrap. A `y` past the bottom (or an `x` past the right) is a no-op.
    pub fn text(&mut self, x: u16, y: u16, s: &str, style: Style) {
        for (i, ch) in s.chars().enumerate() {
            let Some(cx) = x.checked_add(i as u16) else {
                break;
            };
            if cx >= self.width {
                break;
            }
            self.set_cell(cx, y, ch, style);
        }
    }

    /// Draw a horizontal run of `ch`, `len` cells wide, from `(x, y)`. Clipped.
    pub fn hline(&mut self, x: u16, y: u16, len: u16, ch: char, style: Style) {
        for i in 0..len {
            let Some(cx) = x.checked_add(i) else { break };
            if cx >= self.width {
                break;
            }
            self.set_cell(cx, y, ch, style);
        }
    }

    /// Draw a vertical run of `ch`, `len` cells tall, from `(x, y)`. Clipped.
    pub fn vline(&mut self, x: u16, y: u16, len: u16, ch: char, style: Style) {
        for i in 0..len {
            let Some(cy) = y.checked_add(i) else { break };
            if cy >= self.height {
                break;
            }
            self.set_cell(x, cy, ch, style);
        }
    }

    /// Draw a border rectangle (corners `┌┐└┘`, edges `─│`); the interior is left
    /// untouched. Degenerate sizes (`w` or `h` < 2) are clipped sensibly: a width or
    /// height of 1 draws the corresponding single line, 0 draws nothing.
    pub fn draw_box(&mut self, x: u16, y: u16, w: u16, h: u16, style: Style) {
        if w == 0 || h == 0 {
            return;
        }
        let (Some(right), Some(bottom)) = (x.checked_add(w - 1), y.checked_add(h - 1)) else {
            return;
        };
        // Edges.
        self.hline(x, y, w, '─', style);
        self.hline(x, bottom, w, '─', style);
        self.vline(x, y, h, '│', style);
        self.vline(right, y, h, '│', style);
        // Corners (drawn last so they win at intersections).
        self.set_cell(x, y, '┌', style);
        self.set_cell(right, y, '┐', style);
        self.set_cell(x, bottom, '└', style);
        self.set_cell(right, bottom, '┘', style);
    }

    /// Fill the `w × h` rectangle anchored at `(x, y)` with `ch` + `style`. Clipped.
    pub fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, ch: char, style: Style) {
        for dy in 0..h {
            let Some(cy) = y.checked_add(dy) else { break };
            if cy >= self.height {
                break;
            }
            self.hline(x, cy, w, ch, style);
        }
    }

    /// The list of `(x, y, cell)` triples where `self` (the back buffer) differs
    /// from `prev` (the last-flushed buffer) — the minimal set of cells `flush`
    /// must repaint. A PURE function (no I/O) so it's unit-testable without a tty.
    ///
    /// If the two buffers' dimensions differ (e.g. just after a resize), every cell
    /// of `self` is reported changed (a full repaint).
    pub fn diff(&self, prev: &Buffer) -> Vec<(u16, u16, Cell)> {
        let mut out = Vec::new();
        let same_dims = self.width == prev.width && self.height == prev.height;
        for y in 0..self.height {
            for x in 0..self.width {
                let cur = self.cells[y as usize * self.width as usize + x as usize];
                let changed = if same_dims {
                    prev.cells[y as usize * prev.width as usize + x as usize] != cur
                } else {
                    true
                };
                if changed {
                    out.push((x, y, cur));
                }
            }
        }
        out
    }

    /// One row's characters as a string, with trailing spaces trimmed (for readable
    /// debug/snapshot output). An out-of-range `y` yields an empty string.
    pub fn dump_row(&self, y: u16) -> String {
        if y >= self.height {
            return String::new();
        }
        let start = y as usize * self.width as usize;
        let row = &self.cells[start..start + self.width as usize];
        let s: String = row.iter().map(|c| c.ch).collect();
        s.trim_end().to_string()
    }

    /// The whole buffer's characters as text, rows joined by `\n`, each row's
    /// trailing spaces trimmed. A real debug/snapshot aid (styling is ignored).
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for y in 0..self.height {
            out.push_str(&self.dump_row(y));
            out.push('\n');
        }
        out
    }
}

// ---- TerminalState ----

/// The live state behind a `Terminal` handle: the back buffer being drawn into,
/// the last-flushed buffer (for diffing in Task 2), the logical cursor position,
/// and whether raw mode / the alternate screen are currently active (so
/// `restore`/`close` can undo them).
pub struct TerminalState {
    pub back: Buffer,
    pub flushed: Buffer,
    pub cursor: (u16, u16),
    pub raw: bool,
    pub alt: bool,
}

impl TerminalState {
    /// Build state with both buffers sized to `width × height`.
    pub fn new(width: u16, height: u16) -> Self {
        TerminalState {
            back: Buffer::new(width, height),
            flushed: Buffer::new(width, height),
            cursor: (0, 0),
            raw: false,
            alt: false,
        }
    }
}

// ---- module exports + dispatch ----

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("init", bi("tui.init")), ("buffer", bi("tui.buffer"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn use_after_close(span: Span) -> Control {
    AsError::at("use after close: this terminal handle is closed", span).into()
}

impl Interp {
    /// Module-level dispatch for `std/tui` (`init`).
    pub(crate) fn call_tui(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "init" => self.tui_init(),
            "buffer" => self.tui_buffer(args, span),
            _ => Err(AsError::at(format!("std/tui has no function '{}'", func), span).into()),
        }
    }

    /// `init() -> [term, err]`. Query the terminal size (80×24 fallback on a
    /// non-tty), build a `TerminalState` sized to it, register the handle.
    fn tui_init(&self) -> Result<Value, Control> {
        let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
        let state = TerminalState::new(w, h);
        let handle = self.register_resource(
            NativeKind::Terminal,
            indexmap::IndexMap::new(),
            ResourceState::Terminal(Box::new(state)),
        );
        Ok(make_pair(handle, Value::Nil))
    }

    /// `buffer(width, height) -> term`. An OFF-SCREEN, explicit-size variant of
    /// `init()`: it builds the SAME kind of `Terminal` handle (the `NativeKind::Terminal`
    /// resource with a `ResourceState::Terminal`) sized to `width × height` WITHOUT
    /// querying the real terminal. Use it for off-screen drawing, testing, and deterministic
    /// `dump()`s. It supports every drawing method + `dump()`. `flush()` on such a
    /// handle still runs the diff and writes to stdout like any terminal (harmless on
    /// a non-tty, but pointless off-screen) — the example/tests just don't call it.
    ///
    /// Unlike `init()` (which can't fail in a way we surface, hence its `[term,err]`),
    /// `buffer` returns the handle DIRECTLY: the only failure is arg/size misuse,
    /// which is a Tier-2 panic (per `want_dim`: non-number / non-integer / negative /
    /// zero / > u16::MAX). A `[term, err]` pair here would be pure noise.
    fn tui_buffer(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let w = want_dim(&super::arg(args, 0), span, "tui.buffer width")?;
        let h = want_dim(&super::arg(args, 1), span, "tui.buffer height")?;
        let state = TerminalState::new(w, h);
        let handle = self.register_resource(
            NativeKind::Terminal,
            indexmap::IndexMap::new(),
            ResourceState::Terminal(Box::new(state)),
        );
        Ok(handle)
    }

    /// Dispatch a method on a `Terminal` handle. Async-signature for dispatch
    /// uniformity though every Task-1 op is synchronous.
    pub(crate) async fn call_terminal_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;

        // `close`/`restore` consume the handle; run them before the presence
        // check so a double close is itself a "use after close".
        if m.method == "close" || m.method == "restore" {
            return match self.take_resource(id) {
                Some(ResourceState::Terminal(state)) => {
                    // Undo any active terminal modes, then show the cursor.
                    let mut last_err: Option<String> = None;
                    if state.raw {
                        if let Err(e) = crossterm::terminal::disable_raw_mode() {
                            last_err =
                                Some(format!("terminal.{}: disable raw mode: {}", m.method, e));
                        }
                    }
                    if state.alt {
                        if let Err(e) = crossterm::execute!(
                            std::io::stdout(),
                            crossterm::terminal::LeaveAlternateScreen
                        ) {
                            last_err =
                                Some(format!("terminal.{}: leave alt screen: {}", m.method, e));
                        }
                    }
                    if let Err(e) = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)
                    {
                        last_err = Some(format!("terminal.{}: show cursor: {}", m.method, e));
                    }
                    match last_err {
                        Some(msg) => Ok(err_pair(msg)),
                        None => Ok(make_pair(Value::Nil, Value::Nil)),
                    }
                }
                _ => Err(use_after_close(span)),
            };
        }

        // Every other method needs a live handle.
        if self.terminal_mut(id).is_none() {
            return Err(use_after_close(span));
        }

        match m.method.as_str() {
            "size" => {
                let state = self.terminal_mut(id).expect("checked present");
                let mut map = indexmap::IndexMap::new();
                map.insert("width".to_string(), Value::Float(state.back.width as f64));
                map.insert(
                    "height".to_string(),
                    Value::Float(state.back.height as f64),
                );
                Ok(Value::Object(crate::value::ObjectCell::new(map)))
            }
            "clear" => {
                let mut state = self.terminal_mut(id).expect("checked present");
                state.back.clear();
                Ok(Value::Nil)
            }
            "moveCursor" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.moveCursor x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.moveCursor y")?;
                let mut state = self.terminal_mut(id).expect("checked present");
                state.cursor = (x, y);
                Ok(Value::Nil)
            }
            "enterRaw" => match crossterm::terminal::enable_raw_mode() {
                Ok(()) => {
                    self.terminal_mut(id).expect("checked present").raw = true;
                    Ok(make_pair(Value::Nil, Value::Nil))
                }
                Err(e) => Ok(err_pair(format!("terminal.enterRaw failed: {}", e))),
            },
            "leaveRaw" => match crossterm::terminal::disable_raw_mode() {
                Ok(()) => {
                    self.terminal_mut(id).expect("checked present").raw = false;
                    Ok(make_pair(Value::Nil, Value::Nil))
                }
                Err(e) => Ok(err_pair(format!("terminal.leaveRaw failed: {}", e))),
            },
            "enterAltScreen" => {
                match crossterm::execute!(
                    std::io::stdout(),
                    crossterm::terminal::EnterAlternateScreen
                ) {
                    Ok(()) => {
                        self.terminal_mut(id).expect("checked present").alt = true;
                        Ok(make_pair(Value::Nil, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("terminal.enterAltScreen failed: {}", e))),
                }
            }
            "leaveAltScreen" => {
                match crossterm::execute!(
                    std::io::stdout(),
                    crossterm::terminal::LeaveAlternateScreen
                ) {
                    Ok(()) => {
                        self.terminal_mut(id).expect("checked present").alt = false;
                        Ok(make_pair(Value::Nil, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("terminal.leaveAltScreen failed: {}", e))),
                }
            }
            "showCursor" => {
                let show = want_bool(&super::arg(&args, 0), span, "terminal.showCursor")?;
                let mut out = std::io::stdout();
                let res = if show {
                    crossterm::execute!(out, crossterm::cursor::Show)
                } else {
                    crossterm::execute!(out, crossterm::cursor::Hide)
                };
                let _ = out.flush();
                match res {
                    Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                    Err(e) => Ok(err_pair(format!("terminal.showCursor failed: {}", e))),
                }
            }
            "setCell" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.setCell x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.setCell y")?;
                let ch = want_char(&super::arg(&args, 2), span, "terminal.setCell char")?;
                let style = parse_style(&super::arg(&args, 3), span)?;
                if let Some(ch) = ch {
                    self.terminal_mut(id)
                        .expect("checked present")
                        .back
                        .set_cell(x, y, ch, style);
                }
                Ok(Value::Nil)
            }
            "text" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.text x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.text y")?;
                let s = super::want_string(&super::arg(&args, 2), span, "terminal.text str")?;
                let style = parse_style(&super::arg(&args, 3), span)?;
                self.terminal_mut(id)
                    .expect("checked present")
                    .back
                    .text(x, y, &s, style);
                Ok(Value::Nil)
            }
            "hline" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.hline x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.hline y")?;
                let len = want_u16(&super::arg(&args, 2), span, "terminal.hline len")?;
                let ch = match &super::arg(&args, 3) {
                    Value::Nil => Some('─'),
                    other => want_char(other, span, "terminal.hline char")?,
                };
                let style = parse_style(&super::arg(&args, 4), span)?;
                if let Some(ch) = ch {
                    self.terminal_mut(id)
                        .expect("checked present")
                        .back
                        .hline(x, y, len, ch, style);
                }
                Ok(Value::Nil)
            }
            "vline" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.vline x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.vline y")?;
                let len = want_u16(&super::arg(&args, 2), span, "terminal.vline len")?;
                let ch = match &super::arg(&args, 3) {
                    Value::Nil => Some('│'),
                    other => want_char(other, span, "terminal.vline char")?,
                };
                let style = parse_style(&super::arg(&args, 4), span)?;
                if let Some(ch) = ch {
                    self.terminal_mut(id)
                        .expect("checked present")
                        .back
                        .vline(x, y, len, ch, style);
                }
                Ok(Value::Nil)
            }
            "box" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.box x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.box y")?;
                let w = want_u16(&super::arg(&args, 2), span, "terminal.box w")?;
                let h = want_u16(&super::arg(&args, 3), span, "terminal.box h")?;
                let style = parse_style(&super::arg(&args, 4), span)?;
                self.terminal_mut(id)
                    .expect("checked present")
                    .back
                    .draw_box(x, y, w, h, style);
                Ok(Value::Nil)
            }
            "fill" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.fill x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.fill y")?;
                let w = want_u16(&super::arg(&args, 2), span, "terminal.fill w")?;
                let h = want_u16(&super::arg(&args, 3), span, "terminal.fill h")?;
                let ch = want_char(&super::arg(&args, 4), span, "terminal.fill char")?;
                let style = parse_style(&super::arg(&args, 5), span)?;
                if let Some(ch) = ch {
                    self.terminal_mut(id)
                        .expect("checked present")
                        .back
                        .fill(x, y, w, h, ch, style);
                }
                Ok(Value::Nil)
            }
            "flush" => {
                // Scope each `terminal_mut` (a RefMut guard) so it drops before the
                // next borrow — a RefMut can't be live twice on the same cell.
                let (changes, cursor) = {
                    let state = self.terminal_mut(id).expect("checked present");
                    // Pure diff first (unit-tested separately), then the crossterm write.
                    (state.back.diff(&state.flushed), state.cursor)
                };
                let res = flush_changes(&changes, cursor);
                // Sync flushed←back regardless of write outcome (the back buffer is
                // the source of truth; a failed write just means the screen may lag).
                {
                    let mut state = self.terminal_mut(id).expect("checked present");
                    state.flushed = state.back.clone();
                }
                match res {
                    Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                    Err(e) => Ok(err_pair(format!("terminal.flush failed: {}", e))),
                }
            }
            "pollEvent" => {
                let ms = match &super::arg(&args, 0) {
                    Value::Nil => 0,
                    other => want_u16(other, span, "terminal.pollEvent timeoutMs")? as u64,
                };
                // Poll → read → skip non-surfacing events (key Release) → re-poll for
                // the next one with a small budget. We bound the loop so a flood of
                // Release events can't make `pollEvent` block past its timeout: each
                // skipped event consumes one more poll(remaining-or-0ms) cycle, and a
                // poll(false) (nothing ready) bails to nil.
                let mut remaining = ms;
                loop {
                    match crossterm::event::poll(std::time::Duration::from_millis(remaining)) {
                        Ok(true) => match crossterm::event::read() {
                            Ok(ev) => {
                                if surfaces(&ev) {
                                    self.apply_event_resize(id, &ev);
                                    break Ok(make_pair(event_to_value(ev), Value::Nil));
                                }
                                // Skipped (e.g. a key Release). Look for the next event
                                // within the leftover budget; on a timed poll we drop to
                                // 0ms so we never block waiting after a skip.
                                remaining = 0;
                            }
                            Err(e) => {
                                break Ok(err_pair(format!(
                                    "terminal.pollEvent read failed: {}",
                                    e
                                )))
                            }
                        },
                        Ok(false) => break Ok(make_pair(Value::Nil, Value::Nil)),
                        Err(e) => break Ok(err_pair(format!("terminal.pollEvent failed: {}", e))),
                    }
                }
            }
            "readEvent" => {
                // Block until a *surfacing* event arrives, skipping key Release events
                // (so a single keypress yields one key object, not Press+Release).
                loop {
                    match crossterm::event::read() {
                        Ok(ev) => {
                            if surfaces(&ev) {
                                self.apply_event_resize(id, &ev);
                                break Ok(make_pair(event_to_value(ev), Value::Nil));
                            }
                        }
                        Err(e) => break Ok(err_pair(format!("terminal.readEvent failed: {}", e))),
                    }
                }
            }
            "dump" => {
                let state = self.terminal_mut(id).expect("checked present");
                Ok(Value::Str(state.back.dump().into()))
            }
            "dumpRow" => {
                let y = want_u16(&super::arg(&args, 0), span, "terminal.dumpRow y")?;
                let state = self.terminal_mut(id).expect("checked present");
                Ok(Value::Str(state.back.dump_row(y).into()))
            }
            other => Err(AsError::at(format!("terminal has no method '{}'", other), span).into()),
        }
    }
}

impl Interp {
    /// On a `Resize(w, h)` event, resize the terminal's `back` and `flushed`
    /// buffers to the new dimensions. We CLEAR rather than preserve content (the
    /// simplest, race-free choice — the caller is expected to redraw + flush after
    /// a resize anyway); `flushed` is also cleared so the next flush fully repaints
    /// the resized screen. Non-resize events are ignored. The handle is assumed
    /// live (every event caller has already passed the presence check).
    fn apply_event_resize(&self, id: u64, ev: &crossterm::event::Event) {
        if let crossterm::event::Event::Resize(w, h) = ev {
            if let Some(mut state) = self.terminal_mut(id) {
                state.back = Buffer::new(*w, *h);
                state.flushed = Buffer::new(*w, *h);
                // Keep the logical cursor inside the new bounds (tidiness).
                state.cursor.0 = state.cursor.0.min(w.saturating_sub(1));
                state.cursor.1 = state.cursor.1.min(h.saturating_sub(1));
            }
        }
    }
}

/// Write the `(x, y, cell)` diff list to stdout via crossterm, then position the
/// cursor and flush. Each cell gets a `MoveTo`, its fg/bg colors, its attributes
/// (reset-then-apply so a cell never inherits a previous cell's bold/underline),
/// and its char. Fallible (stdout write) → surfaced as Tier-1 by the caller.
///
/// NOTE: this performs real stdout writes. On a non-tty (CI) the ANSI escapes go
/// to the captured stream but are not asserted — the diff computation (pure) and
/// the `flushed`←`back` sync are what the tests cover.
fn flush_changes(changes: &[(u16, u16, Cell)], cursor: (u16, u16)) -> std::io::Result<()> {
    use crossterm::style::{Attribute, SetAttribute, SetBackgroundColor, SetForegroundColor};
    use crossterm::{cursor::MoveTo, queue, style::Print};
    let mut out = std::io::stdout();
    for &(x, y, cell) in changes {
        queue!(out, MoveTo(x, y))?;
        // Reset attributes first so each cell starts from a clean slate, then apply.
        queue!(out, SetAttribute(Attribute::Reset))?;
        queue!(out, SetForegroundColor(cell.fg.to_crossterm()))?;
        queue!(out, SetBackgroundColor(cell.bg.to_crossterm()))?;
        if cell.attrs.bold {
            queue!(out, SetAttribute(Attribute::Bold))?;
        }
        if cell.attrs.underline {
            queue!(out, SetAttribute(Attribute::Underlined))?;
        }
        if cell.attrs.italic {
            queue!(out, SetAttribute(Attribute::Italic))?;
        }
        if cell.attrs.reverse {
            queue!(out, SetAttribute(Attribute::Reverse))?;
        }
        queue!(out, Print(cell.ch))?;
    }
    // Clear residual styling, then park the cursor at the logical position.
    queue!(out, SetAttribute(Attribute::Reset))?;
    queue!(out, MoveTo(cursor.0, cursor.1))?;
    out.flush()
}

/// A non-negative integer coordinate in `0..=u16::MAX` (Tier-2 on misuse).
fn want_u16(v: &Value, span: Span, ctx: &str) -> Result<u16, Control> {
    let n = super::want_number(v, span, ctx)?;
    if n < 0.0 || n.fract() != 0.0 || n > u16::MAX as f64 {
        return Err(
            AsError::at(format!("{} must be an integer 0..={}", ctx, u16::MAX), span).into(),
        );
    }
    Ok(n as u16)
}

/// Like `want_u16` but for a buffer DIMENSION: requires a positive integer in
/// `1..=u16::MAX`. A zero-sized off-screen buffer is degenerate (nothing to draw
/// into), so we reject it up front rather than hand back an unusable handle.
fn want_dim(v: &Value, span: Span, ctx: &str) -> Result<u16, Control> {
    let n = super::want_number(v, span, ctx)?;
    if n < 1.0 || n.fract() != 0.0 || n > u16::MAX as f64 {
        return Err(
            AsError::at(format!("{} must be an integer 1..={}", ctx, u16::MAX), span).into(),
        );
    }
    Ok(n as u16)
}

/// Resolve an ANSI color name to its crossterm color. The 16 standard names map to
/// crossterm's dim variants (`red` → `DarkRed`), `bright*` to the vivid variants
/// (`brightred` → `Red`); `default`/`reset` → `Reset`. Unknown → `None`.
fn named_color(name: &str) -> Option<Color> {
    use crossterm::style::Color as C;
    let c = match name {
        "default" | "reset" => return Some(Color::Reset),
        "black" => C::Black,
        "red" => C::DarkRed,
        "green" => C::DarkGreen,
        "yellow" => C::DarkYellow,
        "blue" => C::DarkBlue,
        "magenta" => C::DarkMagenta,
        "cyan" => C::DarkCyan,
        "white" => C::Grey,
        "brightblack" => C::DarkGrey,
        "brightred" => C::Red,
        "brightgreen" => C::Green,
        "brightyellow" => C::Yellow,
        "brightblue" => C::Blue,
        "brightmagenta" => C::Magenta,
        "brightcyan" => C::Cyan,
        "brightwhite" => C::White,
        _ => return None,
    };
    Some(Color::Named(c))
}

/// Parse one color field (`fg`/`bg`): a name string → `Named`, a `[r,g,b]` array
/// (each 0-255) → `Rgb`, a number 0-255 → `Indexed`. Malformed → Tier-2 panic.
fn parse_color(v: &Value, span: Span, field: &str) -> Result<Color, Control> {
    match v {
        Value::Str(s) => named_color(s).ok_or_else(|| {
            AsError::at(
                format!("terminal style: unknown color name '{}' for '{}'", s, field),
                span,
            )
            .into()
        }),
        Value::Float(n) => {
            if *n < 0.0 || n.fract() != 0.0 || *n > 255.0 {
                return Err(AsError::at(
                    format!(
                        "terminal style: '{}' color index must be an integer 0..=255, got {}",
                        field, n
                    ),
                    span,
                )
                .into());
            }
            Ok(Color::Indexed(*n as u8))
        }
        Value::Array(a) => {
            let a = a.borrow();
            if a.len() != 3 {
                return Err(AsError::at(
                    format!(
                        "terminal style: '{}' rgb array must have 3 elements, got {}",
                        field,
                        a.len()
                    ),
                    span,
                )
                .into());
            }
            let mut parts = [0u8; 3];
            for (i, item) in a.iter().enumerate() {
                let Value::Float(n) = item else {
                    return Err(AsError::at(
                        format!("terminal style: '{}' rgb component must be a number", field),
                        span,
                    )
                    .into());
                };
                if *n < 0.0 || n.fract() != 0.0 || *n > 255.0 {
                    return Err(AsError::at(
                        format!(
                            "terminal style: '{}' rgb component must be an integer 0..=255, got {}",
                            field, n
                        ),
                        span,
                    )
                    .into());
                }
                parts[i] = *n as u8;
            }
            Ok(Color::Rgb(parts[0], parts[1], parts[2]))
        }
        other => Err(AsError::at(
            format!(
                "terminal style: '{}' must be a color name, index 0..=255, or [r,g,b], got {}",
                field,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// One boolean style flag from a style object: a `bool`, or absent (→ `false`).
/// A present-but-non-bool value is a Tier-2 panic.
fn parse_flag(
    map: &indexmap::IndexMap<String, Value>,
    key: &str,
    span: Span,
) -> Result<bool, Control> {
    match map.get(key) {
        None | Some(Value::Nil) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(AsError::at(
            format!(
                "terminal style: '{}' must be a boolean, got {}",
                key,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Parse a `{fg?, bg?, bold?, underline?, italic?, reverse?}` style object into a
/// [`Style`]. A `nil`/missing style arg → all defaults. Missing fields default
/// (Reset color / no attribute). Any malformed field → Tier-2 panic (spec §11.3).
pub fn parse_style(v: &Value, span: Span) -> Result<Style, Control> {
    let mut style = Style::default();
    let map = match v {
        Value::Nil => return Ok(style),
        Value::Object(o) => o.borrow(),
        other => {
            return Err(AsError::at(
                format!(
                    "terminal style must be an object, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
    };
    if let Some(fg) = map.get("fg").filter(|v| !matches!(v, Value::Nil)) {
        style.fg = parse_color(fg, span, "fg")?;
    }
    if let Some(bg) = map.get("bg").filter(|v| !matches!(v, Value::Nil)) {
        style.bg = parse_color(bg, span, "bg")?;
    }
    style.attrs.bold = parse_flag(&map, "bold", span)?;
    style.attrs.underline = parse_flag(&map, "underline", span)?;
    style.attrs.italic = parse_flag(&map, "italic", span)?;
    style.attrs.reverse = parse_flag(&map, "reverse", span)?;
    Ok(style)
}

/// Extract a single drawing `char` from a 1-char-string argument. A multi-char
/// string takes its first char (documented); an empty string → `None` (the caller
/// treats that as a no-op). A non-string is a Tier-2 panic.
fn want_char(v: &Value, span: Span, ctx: &str) -> Result<Option<char>, Control> {
    let s = super::want_string(v, span, ctx)?;
    Ok(s.chars().next())
}

/// A boolean argument (Tier-2 on misuse).
fn want_bool(v: &Value, span: Span, ctx: &str) -> Result<bool, Control> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(AsError::at(
            format!(
                "{} expects a boolean, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// Build an AScript object `Value` from `(key, value)` pairs (insertion-ordered).
fn make_object(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = indexmap::IndexMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(crate::value::ObjectCell::new(m))
}

/// A readable name for a crossterm `KeyCode`. `Char(c)` becomes the single-char
/// string; named keys map to their CamelCase name; `F(n)` → `"F<n>"`. Less-common
/// keys (media, modifier-only, caps/scroll/num lock, null) fall back to a lowercase
/// debug-ish name so they round-trip to *something* rather than being dropped.
fn key_code_name(code: crossterm::event::KeyCode) -> String {
    use crossterm::event::KeyCode as K;
    match code {
        K::Char(c) => c.to_string(),
        K::Enter => "Enter".into(),
        K::Esc => "Esc".into(),
        K::Tab => "Tab".into(),
        K::BackTab => "BackTab".into(),
        K::Backspace => "Backspace".into(),
        K::Delete => "Delete".into(),
        K::Insert => "Insert".into(),
        K::Up => "Up".into(),
        K::Down => "Down".into(),
        K::Left => "Left".into(),
        K::Right => "Right".into(),
        K::Home => "Home".into(),
        K::End => "End".into(),
        K::PageUp => "PageUp".into(),
        K::PageDown => "PageDown".into(),
        K::F(n) => format!("F{}", n),
        K::Null => "Null".into(),
        K::CapsLock => "CapsLock".into(),
        K::ScrollLock => "ScrollLock".into(),
        K::NumLock => "NumLock".into(),
        K::PrintScreen => "PrintScreen".into(),
        K::Pause => "Pause".into(),
        K::Menu => "Menu".into(),
        K::KeypadBegin => "KeypadBegin".into(),
        K::Media(_) | K::Modifier(_) => format!("{:?}", code),
    }
}

/// Whether an event should be surfaced to the AScript program (vs. silently
/// dropped by `pollEvent`/`readEvent`). PURE + unit-testable.
///
/// Key events surface ONLY for `Press` and `Repeat` kinds — a `Release` is dropped.
/// On Windows and kitty-protocol terminals a single keypress emits both a Press and
/// a Release; without this filter every keypress would yield two identical key
/// objects. `Repeat` IS surfaced so that holding a key auto-repeats (the common,
/// desirable behaviour for e.g. arrow-key navigation). All non-key events (mouse,
/// resize, focus, paste) always surface.
pub fn surfaces(ev: &crossterm::event::Event) -> bool {
    use crossterm::event::{Event, KeyEventKind};
    match ev {
        Event::Key(k) => matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat),
        _ => true,
    }
}

/// Convert a `crossterm` `Event` into an AScript object. PURE + unit-testable: it
/// touches no terminal state. Shapes:
/// - `Key`   → `{type:"key", key, ctrl, alt, shift}`
/// - `Mouse` → `{type:"mouse", x, y, kind, button}` (`button` is `nil` when N/A)
/// - `Resize`→ `{type:"resize", width, height}`
/// - `FocusGained`/`FocusLost` → `{type:"focus", focused}`
/// - `Paste` → `{type:"paste", text}`
pub fn event_to_value(ev: crossterm::event::Event) -> Value {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEventKind};
    match ev {
        Event::Key(k) => {
            let m = k.modifiers;
            make_object(vec![
                ("type", Value::Str("key".into())),
                ("key", Value::Str(key_code_name(k.code).into())),
                ("ctrl", Value::Bool(m.contains(KeyModifiers::CONTROL))),
                ("alt", Value::Bool(m.contains(KeyModifiers::ALT))),
                ("shift", Value::Bool(m.contains(KeyModifiers::SHIFT))),
            ])
        }
        Event::Mouse(me) => {
            let (kind, button) = match me.kind {
                MouseEventKind::Down(b) => ("down", Some(b)),
                MouseEventKind::Up(b) => ("up", Some(b)),
                MouseEventKind::Drag(b) => ("drag", Some(b)),
                MouseEventKind::Moved => ("moved", None),
                MouseEventKind::ScrollUp => ("scrollUp", None),
                MouseEventKind::ScrollDown => ("scrollDown", None),
                MouseEventKind::ScrollLeft => ("scrollLeft", None),
                MouseEventKind::ScrollRight => ("scrollRight", None),
            };
            let button = match button {
                Some(MouseButton::Left) => Value::Str("left".into()),
                Some(MouseButton::Right) => Value::Str("right".into()),
                Some(MouseButton::Middle) => Value::Str("middle".into()),
                None => Value::Nil,
            };
            make_object(vec![
                ("type", Value::Str("mouse".into())),
                ("x", Value::Float(me.column as f64)),
                ("y", Value::Float(me.row as f64)),
                ("kind", Value::Str(kind.into())),
                ("button", button),
            ])
        }
        Event::Resize(w, h) => make_object(vec![
            ("type", Value::Str("resize".into())),
            ("width", Value::Float(w as f64)),
            ("height", Value::Float(h as f64)),
        ]),
        Event::FocusGained => make_object(vec![
            ("type", Value::Str("focus".into())),
            ("focused", Value::Bool(true)),
        ]),
        Event::FocusLost => make_object(vec![
            ("type", Value::Str("focus".into())),
            ("focused", Value::Bool(false)),
        ]),
        Event::Paste(text) => make_object(vec![
            ("type", Value::Str("paste".into())),
            ("text", Value::Str(text.into())),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::style::Color as Ct;

    /// Build an object Value from `(key, value)` pairs for style-parse tests.
    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = indexmap::IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }

    fn arr(items: Vec<f64>) -> Value {
        Value::Array(crate::value::ArrayCell::new(
            items.into_iter().map(Value::Float).collect(),
        ))
    }

    #[test]
    fn new_buffer_has_dims_and_blank_cells() {
        let b = Buffer::new(10, 3);
        assert_eq!(b.width, 10);
        assert_eq!(b.height, 3);
        assert_eq!(b.cells.len(), 30);
        assert!(b.cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(Cell::default().ch, ' ');
    }

    // ---- parse_style ----

    #[test]
    fn parse_style_nil_is_default() {
        let s = parse_style(&Value::Nil, Span::new(0, 0)).unwrap();
        assert_eq!(s, Style::default());
    }

    #[test]
    fn parse_style_named_color_and_flags() {
        let v = obj(vec![
            ("fg", Value::Str("red".into())),
            ("bold", Value::Bool(true)),
        ]);
        let s = parse_style(&v, Span::new(0, 0)).unwrap();
        assert_eq!(s.fg, Color::Named(Ct::DarkRed));
        assert!(s.attrs.bold);
        assert!(!s.attrs.underline);
        assert_eq!(s.bg, Color::Reset);
    }

    #[test]
    fn parse_style_bright_color() {
        let v = obj(vec![("fg", Value::Str("brightred".into()))]);
        let s = parse_style(&v, Span::new(0, 0)).unwrap();
        assert_eq!(s.fg, Color::Named(Ct::Red));
    }

    #[test]
    fn parse_style_rgb_array() {
        let v = obj(vec![("fg", arr(vec![10.0, 20.0, 30.0]))]);
        let s = parse_style(&v, Span::new(0, 0)).unwrap();
        assert_eq!(s.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn parse_style_indexed_number() {
        let v = obj(vec![("bg", Value::Float(200.0))]);
        let s = parse_style(&v, Span::new(0, 0)).unwrap();
        assert_eq!(s.bg, Color::Indexed(200));
    }

    #[test]
    fn parse_style_unknown_color_panics() {
        let v = obj(vec![("fg", Value::Str("banana".into()))]);
        assert!(parse_style(&v, Span::new(0, 0)).is_err());
    }

    #[test]
    fn parse_style_rgb_out_of_range_panics() {
        let v = obj(vec![("fg", arr(vec![300.0, 0.0, 0.0]))]);
        assert!(parse_style(&v, Span::new(0, 0)).is_err());
    }

    #[test]
    fn parse_style_rgb_wrong_len_panics() {
        let v = obj(vec![("fg", arr(vec![1.0, 2.0]))]);
        assert!(parse_style(&v, Span::new(0, 0)).is_err());
    }

    #[test]
    fn parse_style_non_bool_flag_panics() {
        let v = obj(vec![("bold", Value::Str("yes".into()))]);
        assert!(parse_style(&v, Span::new(0, 0)).is_err());
    }

    #[test]
    fn get_and_cell_mut_bounds_clip() {
        let mut b = Buffer::new(4, 2);
        // In-bounds.
        assert!(b.get(0, 0).is_some());
        assert!(b.get(3, 1).is_some());
        // Out-of-bounds → None (clip, not error).
        assert!(b.get(4, 0).is_none());
        assert!(b.get(0, 2).is_none());
        assert!(b.cell_mut(4, 0).is_none());
        assert!(b.cell_mut(0, 2).is_none());
        // cell_mut on an in-bounds cell yields a writable reference.
        b.cell_mut(2, 1).unwrap().ch = 'X';
        assert_eq!(b.get(2, 1).unwrap().ch, 'X');
        // Row-major layout: (2,1) is index 1*4 + 2 = 6.
        assert_eq!(b.idx(2, 1), Some(6));
    }

    #[test]
    fn clear_resets_a_mutated_buffer() {
        let mut b = Buffer::new(3, 2);
        b.cell_mut(0, 0).unwrap().ch = 'A';
        b.cell_mut(2, 1).unwrap().fg = Color::Rgb(1, 2, 3);
        assert_ne!(b.get(0, 0).unwrap().ch, ' ');
        b.clear();
        assert!(b.cells.iter().all(|c| *c == Cell::default()));
    }

    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    /// Run an AScript program expecting a Tier-2 panic; return the panic message.
    async fn run_err(src: &str) -> String {
        match crate::run_source(src).await {
            Ok(out) => panic!("expected a panic, but program produced output:\n{}", out),
            Err(e) => e.message,
        }
    }

    #[tokio::test]
    async fn init_returns_a_terminal_handle() {
        let out = run(r#"
import { init } from "std/tui"
let [term, err] = init()
print(err)
print(type(term))
"#)
        .await;
        assert_eq!(out, "nil\nterminal\n");
    }

    #[tokio::test]
    async fn size_returns_positive_dims_on_non_tty() {
        // No tty in CI → 80×24 fallback; assert the shape + positivity (not exact
        // values, which vary by environment).
        let out = run(r#"
import { init } from "std/tui"
let [term, _] = init()
let s = term.size()
print(s.width > 0)
print(s.height > 0)
print(type(s.width))
print(type(s.height))
"#)
        .await;
        assert_eq!(out, "true\ntrue\nnumber\nnumber\n");
    }

    #[tokio::test]
    async fn buffer_makes_offscreen_handle_with_given_size() {
        // tui.buffer(w,h) returns the handle DIRECTLY (not a [term, err] pair) —
        // it can only fail on bad args (which panic), so a pair would be noise.
        let out = run(r#"
import { buffer } from "std/tui"
let term = buffer(10, 3)
print(type(term))
let s = term.size()
print(s.width)
print(s.height)
"#)
        .await;
        assert_eq!(out, "terminal\n10\n3\n");
    }

    #[tokio::test]
    async fn buffer_rejects_bad_args_with_panic() {
        // Non-number / negative / huge → Tier-2 panic (want_u16-style validation).
        for bad in ["buffer(\"x\", 3)", "buffer(-1, 3)", "buffer(10, 0)"] {
            let src = format!("import {{ buffer }} from \"std/tui\"\nlet t = {}\n", bad);
            let msg = run_err(&src).await;
            assert!(
                msg.contains("buffer") || msg.contains("must be"),
                "for `{}` got: {}",
                bad,
                msg
            );
        }
    }

    #[tokio::test]
    async fn buffer_supports_drawing_and_dump() {
        let out = run(r#"
import { buffer } from "std/tui"
let term = buffer(4, 3)
term.box(0, 0, 4, 3, { fg: "cyan" })
print(term.dump())
"#)
        .await;
        // dump() ends each row with "\n" (3 rows); print adds one more "\n".
        assert_eq!(out, "┌──┐\n│  │\n└──┘\n\n");
    }

    #[tokio::test]
    async fn use_after_close_is_a_panic() {
        let msg = run_err(
            r#"
import { init } from "std/tui"
let [term, _] = init()
term.close()
term.size()
"#,
        )
        .await;
        assert!(msg.contains("use after close"), "got: {}", msg);
    }

    #[test]
    fn terminal_state_sizes_both_buffers() {
        let s = TerminalState::new(40, 12);
        assert_eq!(s.back.width, 40);
        assert_eq!(s.back.height, 12);
        assert_eq!(s.flushed.width, 40);
        assert_eq!(s.flushed.height, 12);
        assert_eq!(s.cursor, (0, 0));
        assert!(!s.raw);
        assert!(!s.alt);
    }

    // ---- Buffer drawing primitives (exact, no tty) ----

    #[test]
    fn set_cell_in_bounds_and_oob_noop() {
        let mut b = Buffer::new(4, 2);
        b.set_cell(1, 0, 'A', Style::default());
        assert_eq!(b.get(1, 0).unwrap().ch, 'A');
        // OOB → no-op (no panic, no change).
        let before = b.clone();
        b.set_cell(99, 0, 'Z', Style::default());
        b.set_cell(0, 99, 'Z', Style::default());
        assert_eq!(b, before);
    }

    #[test]
    fn text_writes_left_to_right_and_clips_at_row_end() {
        let mut b = Buffer::new(5, 1);
        b.text(2, 0, "Hi", Style::default());
        assert_eq!(b.dump_row(0), "  Hi");
        // Clip at right edge: "World" from x=3 in a width-5 buffer keeps "Wo".
        let mut b2 = Buffer::new(5, 1);
        b2.text(3, 0, "World", Style::default());
        assert_eq!(b2.dump_row(0), "   Wo");
    }

    #[test]
    fn text_offscreen_is_noop() {
        let mut b = Buffer::new(5, 2);
        let before = b.clone();
        b.text(0, 9, "off", Style::default()); // y past height
        assert_eq!(b, before);
    }

    #[test]
    fn box_draws_border_only() {
        let mut b = Buffer::new(4, 3);
        b.draw_box(0, 0, 4, 3, Style::default());
        assert_eq!(b.dump_row(0), "┌──┐");
        assert_eq!(b.dump_row(1), "│  │");
        assert_eq!(b.dump_row(2), "└──┘");
    }

    #[test]
    fn fill_fills_rect() {
        let mut b = Buffer::new(4, 4);
        b.fill(1, 1, 2, 2, '#', Style::default());
        assert_eq!(b.dump_row(0), "");
        assert_eq!(b.dump_row(1), " ##");
        assert_eq!(b.dump_row(2), " ##");
        assert_eq!(b.dump_row(3), "");
    }

    #[test]
    fn hline_and_vline() {
        let mut b = Buffer::new(3, 1);
        b.hline(0, 0, 3, '─', Style::default());
        assert_eq!(b.dump_row(0), "───");

        let mut b2 = Buffer::new(2, 2);
        b2.vline(0, 0, 2, '│', Style::default());
        assert_eq!(b2.dump_row(0), "│");
        assert_eq!(b2.dump_row(1), "│");
    }

    #[test]
    fn dump_trims_trailing_spaces_per_row() {
        let mut b = Buffer::new(5, 2);
        b.text(0, 0, "ab", Style::default());
        // Two rows: row 0 is "ab" (trailing spaces trimmed), row 1 is empty.
        assert_eq!(b.dump(), "ab\n\n");
    }

    // ---- styling applied to cells ----

    #[test]
    fn text_applies_style_to_cells() {
        let mut b = Buffer::new(5, 1);
        let style = Style {
            fg: Color::Named(Ct::DarkRed),
            bg: Color::Reset,
            attrs: Attrs {
                bold: true,
                ..Attrs::default()
            },
        };
        b.text(0, 0, "X", style);
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.ch, 'X');
        assert_eq!(c.fg, Color::Named(Ct::DarkRed));
        assert!(c.attrs.bold);
    }

    // ---- interp e2e ----

    #[tokio::test]
    async fn e2e_draw_box_and_text_dump() {
        let out = run(r#"
import { init } from "std/tui"
let [term, _] = init()
term.box(0, 0, 10, 3)
term.text(2, 1, "Hello")
print(term.dump())
"#)
        .await;
        assert!(out.contains("┌────────┐"), "got: {}", out);
        assert!(out.contains("Hello"), "got: {}", out);
    }

    #[tokio::test]
    async fn e2e_setcell_multichar_takes_first_char() {
        let out = run(r#"
import { init } from "std/tui"
let [term, _] = init()
term.setCell(0, 0, "abc")
print(term.dumpRow(0))
"#)
        .await;
        assert_eq!(out, "a\n");
    }

    #[tokio::test]
    async fn e2e_setcell_empty_string_is_noop() {
        let out = run(r#"
import { init } from "std/tui"
let [term, _] = init()
term.setCell(0, 0, "")
print("[" + term.dumpRow(0) + "]")
"#)
        .await;
        assert_eq!(out, "[]\n");
    }

    // ---- diff computation (pure, no tty) ----

    #[test]
    fn diff_lists_only_changed_cells() {
        let prev = Buffer::new(5, 2);
        let mut back = prev.clone();
        back.set_cell(1, 0, 'A', Style::default());
        back.set_cell(3, 1, 'B', Style::default());
        let mut d = back.diff(&prev);
        d.sort_by_key(|&(x, y, _)| (y, x));
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].0, d[0].1, d[0].2.ch), (1, 0, 'A'));
        assert_eq!((d[1].0, d[1].1, d[1].2.ch), (3, 1, 'B'));
    }

    #[test]
    fn diff_detects_style_only_changes() {
        let prev = Buffer::new(3, 1);
        let mut back = prev.clone();
        // Same char, different fg → still a change.
        back.set_cell(
            0,
            0,
            ' ',
            Style {
                fg: Color::Rgb(1, 2, 3),
                ..Style::default()
            },
        );
        let d = back.diff(&prev);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].0, d[0].1), (0, 0));
    }

    #[test]
    fn diff_empty_after_notional_flush() {
        let mut flushed = Buffer::new(6, 3);
        let mut back = flushed.clone();
        back.draw_box(0, 0, 6, 3, Style::default());
        back.text(2, 1, "hi", Style::default());
        assert!(!back.diff(&flushed).is_empty());
        // Notional flush: copy back→flushed (what flush() does), then no changes.
        flushed = back.clone();
        assert!(back.diff(&flushed).is_empty());
        // A further identical draw still produces no diff.
        back.text(2, 1, "hi", Style::default());
        assert!(back.diff(&flushed).is_empty());
    }

    #[test]
    fn diff_full_repaint_on_dim_change() {
        let prev = Buffer::new(2, 2); // 4 cells
        let back = Buffer::new(3, 2); // 6 cells, different dims
                                      // Mismatched dims → every cell of `back` reported.
        assert_eq!(back.diff(&prev).len(), 6);
    }

    // ---- event_to_value conversion (pure, no tty) ----

    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    };

    /// Read a string field from an event object Value.
    fn field_str(v: &Value, key: &str) -> String {
        let Value::Object(o) = v else {
            panic!("not an object: {:?}", v)
        };
        match o.borrow().get(key) {
            Some(Value::Str(s)) => s.to_string(),
            other => panic!("field {} not a string: {:?}", key, other),
        }
    }
    fn field_bool(v: &Value, key: &str) -> bool {
        let Value::Object(o) = v else {
            panic!("not an object")
        };
        match o.borrow().get(key) {
            Some(Value::Bool(b)) => *b,
            other => panic!("field {} not a bool: {:?}", key, other),
        }
    }
    fn field_num(v: &Value, key: &str) -> f64 {
        let Value::Object(o) = v else {
            panic!("not an object")
        };
        match o.borrow().get(key) {
            Some(Value::Float(n)) => *n,
            other => panic!("field {} not a number: {:?}", key, other),
        }
    }
    fn field_is_nil(v: &Value, key: &str) -> bool {
        let Value::Object(o) = v else {
            panic!("not an object")
        };
        matches!(o.borrow().get(key), Some(Value::Nil) | None)
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn event_char_with_ctrl() {
        let v = event_to_value(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(field_str(&v, "type"), "key");
        assert_eq!(field_str(&v, "key"), "a");
        assert!(field_bool(&v, "ctrl"));
        assert!(!field_bool(&v, "alt"));
        assert!(!field_bool(&v, "shift"));
    }

    #[test]
    fn event_named_keys() {
        assert_eq!(
            field_str(
                &event_to_value(key(KeyCode::Enter, KeyModifiers::NONE)),
                "key"
            ),
            "Enter"
        );
        assert_eq!(
            field_str(&event_to_value(key(KeyCode::Up, KeyModifiers::NONE)), "key"),
            "Up"
        );
        assert_eq!(
            field_str(
                &event_to_value(key(KeyCode::Esc, KeyModifiers::NONE)),
                "key"
            ),
            "Esc"
        );
        assert_eq!(
            field_str(
                &event_to_value(key(KeyCode::Tab, KeyModifiers::NONE)),
                "key"
            ),
            "Tab"
        );
        assert_eq!(
            field_str(
                &event_to_value(key(KeyCode::F(5), KeyModifiers::NONE)),
                "key"
            ),
            "F5"
        );
        assert_eq!(
            field_str(
                &event_to_value(key(KeyCode::PageDown, KeyModifiers::NONE)),
                "key"
            ),
            "PageDown"
        );
    }

    #[test]
    fn event_key_modifiers_combine() {
        let v = event_to_value(key(
            KeyCode::Char('Z'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(field_str(&v, "key"), "Z");
        assert!(!field_bool(&v, "ctrl"));
        assert!(field_bool(&v, "alt"));
        assert!(field_bool(&v, "shift"));
    }

    #[test]
    fn event_mouse_left_down() {
        let v = event_to_value(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(field_str(&v, "type"), "mouse");
        assert_eq!(field_num(&v, "x"), 3.0);
        assert_eq!(field_num(&v, "y"), 4.0);
        assert_eq!(field_str(&v, "kind"), "down");
        assert_eq!(field_str(&v, "button"), "left");
    }

    #[test]
    fn event_mouse_scroll_has_nil_button() {
        let v = event_to_value(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(field_str(&v, "kind"), "scrollUp");
        assert!(field_is_nil(&v, "button"));
    }

    #[test]
    fn event_resize() {
        let v = event_to_value(Event::Resize(100, 40));
        assert_eq!(field_str(&v, "type"), "resize");
        assert_eq!(field_num(&v, "width"), 100.0);
        assert_eq!(field_num(&v, "height"), 40.0);
    }

    #[test]
    fn surfaces_filters_key_release_only() {
        // Press + Repeat surface (so held keys auto-repeat); Release is dropped.
        let press = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        let repeat = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        });
        let release = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert!(surfaces(&press), "Press should surface");
        assert!(surfaces(&repeat), "Repeat should surface (auto-repeat)");
        assert!(!surfaces(&release), "Release must be dropped");
        // Non-key events always surface.
        assert!(surfaces(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })));
        assert!(surfaces(&Event::Resize(10, 5)));
    }

    #[test]
    fn event_focus_and_paste() {
        let g = event_to_value(Event::FocusGained);
        assert_eq!(field_str(&g, "type"), "focus");
        assert!(field_bool(&g, "focused"));
        let p = event_to_value(Event::Paste("hi".into()));
        assert_eq!(field_str(&p, "type"), "paste");
        assert_eq!(field_str(&p, "text"), "hi");
    }

    // ---- pollEvent on a non-tty returns without hanging/panicking ----

    #[tokio::test]
    async fn poll_event_zero_timeout_does_not_hang() {
        // On a non-tty (CI), poll(0ms) returns Ok(false) (→ [nil, nil]) or Err (→ a
        // Tier-1 [nil, {message}]); either is fine. The point: it returns promptly
        // with no event (no panic, no hang). `err` is nil or an error object.
        let out = run(r#"
import { init } from "std/tui"
let [term, _] = init()
let [ev, err] = term.pollEvent(0)
print(ev == nil)
print(err == nil || type(err) == "object")
"#)
        .await;
        assert_eq!(out, "true\ntrue\n");
    }

    // ---- flush syncs flushed←back (the write isn't asserted on a non-tty) ----

    #[test]
    fn flush_changes_on_empty_diff_is_ok() {
        // No changes → flush_changes only resets styling + parks the cursor; on a
        // captured (non-tty) stdout this still returns Ok.
        assert!(flush_changes(&[], (0, 0)).is_ok());
    }

    #[tokio::test]
    async fn e2e_invalid_color_panics() {
        let msg = run_err(
            r#"
import { init } from "std/tui"
let [term, _] = init()
term.text(0, 0, "x", { fg: "banana" })
"#,
        )
        .await;
        assert!(
            msg.contains("banana") || msg.contains("color"),
            "got: {}",
            msg
        );
    }
}
