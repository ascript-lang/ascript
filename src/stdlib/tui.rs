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
            let Some(cx) = x.checked_add(i as u16) else { break };
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
            "setCell" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.setCell x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.setCell y")?;
                let ch = want_char(&super::arg(&args, 2), span, "terminal.setCell char")?;
                let style = parse_style(&super::arg(&args, 3), span)?;
                if let Some(ch) = ch {
                    self.terminal_mut(id).expect("checked present").back.set_cell(x, y, ch, style);
                }
                Ok(Value::Nil)
            }
            "text" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.text x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.text y")?;
                let s = super::want_string(&super::arg(&args, 2), span, "terminal.text str")?;
                let style = parse_style(&super::arg(&args, 3), span)?;
                self.terminal_mut(id).expect("checked present").back.text(x, y, &s, style);
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
                    self.terminal_mut(id).expect("checked present").back.hline(x, y, len, ch, style);
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
                    self.terminal_mut(id).expect("checked present").back.vline(x, y, len, ch, style);
                }
                Ok(Value::Nil)
            }
            "box" => {
                let x = want_u16(&super::arg(&args, 0), span, "terminal.box x")?;
                let y = want_u16(&super::arg(&args, 1), span, "terminal.box y")?;
                let w = want_u16(&super::arg(&args, 2), span, "terminal.box w")?;
                let h = want_u16(&super::arg(&args, 3), span, "terminal.box h")?;
                let style = parse_style(&super::arg(&args, 4), span)?;
                self.terminal_mut(id).expect("checked present").back.draw_box(x, y, w, h, style);
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
                    self.terminal_mut(id).expect("checked present").back.fill(x, y, w, h, ch, style);
                }
                Ok(Value::Nil)
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
            AsError::at(format!("terminal style: unknown color name '{}' for '{}'", s, field), span).into()
        }),
        Value::Number(n) => {
            if *n < 0.0 || n.fract() != 0.0 || *n > 255.0 {
                return Err(AsError::at(
                    format!("terminal style: '{}' color index must be an integer 0..=255, got {}", field, n),
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
                    format!("terminal style: '{}' rgb array must have 3 elements, got {}", field, a.len()),
                    span,
                )
                .into());
            }
            let mut parts = [0u8; 3];
            for (i, item) in a.iter().enumerate() {
                let Value::Number(n) = item else {
                    return Err(AsError::at(
                        format!("terminal style: '{}' rgb component must be a number", field),
                        span,
                    )
                    .into());
                };
                if *n < 0.0 || n.fract() != 0.0 || *n > 255.0 {
                    return Err(AsError::at(
                        format!("terminal style: '{}' rgb component must be an integer 0..=255, got {}", field, n),
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
fn parse_flag(map: &indexmap::IndexMap<String, Value>, key: &str, span: Span) -> Result<bool, Control> {
    match map.get(key) {
        None | Some(Value::Nil) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(AsError::at(
            format!("terminal style: '{}' must be a boolean, got {}", key, crate::interp::type_name(other)),
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
                format!("terminal style must be an object, got {}", crate::interp::type_name(other)),
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
            format!("{} expects a boolean, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
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
        Value::Object(Rc::new(std::cell::RefCell::new(m)))
    }

    fn arr(items: Vec<f64>) -> Value {
        Value::Array(Rc::new(std::cell::RefCell::new(
            items.into_iter().map(Value::Number).collect(),
        )))
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
        let v = obj(vec![("fg", Value::Str("red".into())), ("bold", Value::Bool(true))]);
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
        let v = obj(vec![("bg", Value::Number(200.0))]);
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
        let style = Style { fg: Color::Named(Ct::DarkRed), bg: Color::Reset, attrs: Attrs { bold: true, ..Attrs::default() } };
        b.text(0, 0, "X", style);
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.ch, 'X');
        assert_eq!(c.fg, Color::Named(Ct::DarkRed));
        assert!(c.attrs.bold);
    }

    // ---- interp e2e ----

    #[tokio::test]
    async fn e2e_draw_box_and_text_dump() {
        let out = run(
            r#"
import { init } from "std/tui"
let [term, _] = init()
term.box(0, 0, 10, 3)
term.text(2, 1, "Hello")
print(term.dump())
"#,
        )
        .await;
        assert!(out.contains("┌────────┐"), "got: {}", out);
        assert!(out.contains("Hello"), "got: {}", out);
    }

    #[tokio::test]
    async fn e2e_setcell_multichar_takes_first_char() {
        let out = run(
            r#"
import { init } from "std/tui"
let [term, _] = init()
term.setCell(0, 0, "abc")
print(term.dumpRow(0))
"#,
        )
        .await;
        assert_eq!(out, "a\n");
    }

    #[tokio::test]
    async fn e2e_setcell_empty_string_is_noop() {
        let out = run(
            r#"
import { init } from "std/tui"
let [term, _] = init()
term.setCell(0, 0, "")
print("[" + term.dumpRow(0) + "]")
"#,
        )
        .await;
        assert_eq!(out, "[]\n");
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
        assert!(msg.contains("banana") || msg.contains("color"), "got: {}", msg);
    }
}
