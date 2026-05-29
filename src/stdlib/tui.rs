//! `std/tui` — terminal control + a hand-rolled double-buffered screen (feature
//! `tui`), spec §11.2. Backed by `crossterm` 0.28 for raw mode / alt screen /
//! cursor / styling; the screen `Buffer` (a `width × height` grid of `Cell`s) is
//! hand-rolled so it bridges cleanly to AScript's value model.
//!
//! Task 1 (this file's first slice) lands the `Terminal` `Value::Native` handle
//! plus the screen `Buffer`/`Cell` types and the terminal lifecycle methods
//! (size / clear / raw / alt screen / cursor / restore). The buffer, the size
//! query, and the close lifecycle are unit-testable WITHOUT a real tty (`size()`
//! falls back to an 80x24 default on a non-tty); drawing + styling arrive in Task 2.
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
    #[allow(dead_code)] // used by Task 2's flush diff.
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
        Cell { ch: ' ', fg: Color::Reset, bg: Color::Reset, attrs: Attrs::default() }
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
        Buffer { width, height, cells: vec![Cell::default(); count] }
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
    vec![("init", bi("tui.init"))]
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
        &mut self,
        func: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "init" => self.tui_init(),
            _ => Err(AsError::at(format!("std/tui has no function '{}'", func), span).into()),
        }
    }

    /// `init() -> [term, err]`. Query the terminal size (80×24 fallback on a
    /// non-tty), build a `TerminalState` sized to it, register the handle.
    fn tui_init(&mut self) -> Result<Value, Control> {
        let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
        let state = TerminalState::new(w, h);
        let handle = self.register_resource(
            NativeKind::Terminal,
            indexmap::IndexMap::new(),
            ResourceState::Terminal(Box::new(state)),
        );
        Ok(make_pair(handle, Value::Nil))
    }

    /// Dispatch a method on a `Terminal` handle. Async-signature for dispatch
    /// uniformity though every Task-1 op is synchronous.
    pub(crate) async fn call_terminal_method(
        &mut self,
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
                            last_err = Some(format!("terminal.{}: disable raw mode: {}", m.method, e));
                        }
                    }
                    if state.alt {
                        if let Err(e) =
                            crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)
                        {
                            last_err = Some(format!("terminal.{}: leave alt screen: {}", m.method, e));
                        }
                    }
                    if let Err(e) = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show) {
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
                map.insert("width".to_string(), Value::Number(state.back.width as f64));
                map.insert("height".to_string(), Value::Number(state.back.height as f64));
                Ok(Value::Object(Rc::new(std::cell::RefCell::new(map))))
            }
            "clear" => {
                let state = self.terminal_mut(id).expect("checked present");
                state.back.clear();
                Ok(Value::Nil)
            }
            "moveCursor" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.moveCursor x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.moveCursor y")?;
                let state = self.terminal_mut(id).expect("checked present");
                state.cursor = (x, y);
                Ok(Value::Nil)
            }
            "enterRaw" => {
                match crossterm::terminal::enable_raw_mode() {
                    Ok(()) => {
                        self.terminal_mut(id).expect("checked present").raw = true;
                        Ok(make_pair(Value::Nil, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("terminal.enterRaw failed: {}", e))),
                }
            }
            "leaveRaw" => {
                match crossterm::terminal::disable_raw_mode() {
                    Ok(()) => {
                        self.terminal_mut(id).expect("checked present").raw = false;
                        Ok(make_pair(Value::Nil, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("terminal.leaveRaw failed: {}", e))),
                }
            }
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
            other => {
                Err(AsError::at(format!("terminal has no method '{}'", other), span).into())
            }
        }
    }
}

/// A non-negative integer coordinate in `0..=u16::MAX` (Tier-2 on misuse).
fn want_u16(v: &Value, span: Span, ctx: &str) -> Result<u16, Control> {
    let n = super::want_number(v, span, ctx)?;
    if n < 0.0 || n.fract() != 0.0 || n > u16::MAX as f64 {
        return Err(AsError::at(
            format!("{} must be an integer 0..={}", ctx, u16::MAX),
            span,
        )
        .into());
    }
    Ok(n as u16)
}

/// A boolean argument (Tier-2 on misuse).
fn want_bool(v: &Value, span: Span, ctx: &str) -> Result<bool, Control> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(AsError::at(
            format!("{} expects a boolean, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_has_dims_and_blank_cells() {
        let b = Buffer::new(10, 3);
        assert_eq!(b.width, 10);
        assert_eq!(b.height, 3);
        assert_eq!(b.cells.len(), 30);
        assert!(b.cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(Cell::default().ch, ' ');
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
        let out = run(
            r#"
import { init } from "std/tui"
let [term, err] = init()
print(err)
print(type(term))
"#,
        )
        .await;
        assert_eq!(out, "nil\nterminal\n");
    }

    #[tokio::test]
    async fn size_returns_positive_dims_on_non_tty() {
        // No tty in CI → 80×24 fallback; assert the shape + positivity (not exact
        // values, which vary by environment).
        let out = run(
            r#"
import { init } from "std/tui"
let [term, _] = init()
let s = term.size()
print(s.width > 0)
print(s.height > 0)
print(type(s.width))
print(type(s.height))
"#,
        )
        .await;
        assert_eq!(out, "true\ntrue\nnumber\nnumber\n");
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
}
