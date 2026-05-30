// tui_dashboard.as
// ---------------------------------------------------------------------------
// Draw a dashboard into an OFF-SCREEN tui buffer and render it as text.
//
// `tui.buffer(width, height)` returns a terminal handle DIRECTLY (not a pair):
// it draws into an in-memory back buffer with NO real terminal, NO raw mode,
// so it runs fine non-interactively and produces deterministic output.
//
// Drawing methods (all 0-based x,y; style object is optional):
//   term.box(x, y, w, h, style)             - a bordered rectangle
//   term.text(x, y, str, style)             - write a string
//   term.hline(x, y, len, char?, style)     - horizontal rule
//   term.vline(x, y, len, char?, style)     - vertical rule
//   term.fill(x, y, w, h, char, style)      - fill a region (e.g. a bar)
//   term.dump()                             - render the buffer to a string
//
// Style keys: fg / bg (color names like "cyan","green"), bold, underline, etc.
// ---------------------------------------------------------------------------

import * as tui from "std/tui"

const W = 48
const H = 12

fn main() {
  let term = tui.buffer(W, H)

  // Outer frame + title.
  term.box(0, 0, W, H, { fg: "cyan" })
  term.text(2, 1, "AScript Dashboard", { bold: true, fg: "brightwhite" })
  term.text(W - 9, 1, "v1.0", { fg: "brightblack" })

  // Separator under the title (inside the border).
  term.hline(1, 2, W - 2, "─", { fg: "cyan" })

  // Left column: labeled stats.
  term.text(2, 3, "Status", { underline: true, fg: "brightcyan" })
  term.text(2, 4, "CPU", {})
  term.text(2, 5, "Memory", {})
  term.text(2, 6, "Requests", {})
  term.text(2, 7, "Errors", {})

  // A vertical separator between the labels and the gauges.
  term.vline(22, 3, 6, "│", { fg: "brightblack" })

  // Right side: simple horizontal bar gauges drawn with fill().
  // Each bar's filled length encodes a percentage of a 16-wide track.
  drawBar(term, 24, 4, 16, 11, "green")    // CPU ~69%
  drawBar(term, 24, 5, 16, 14, "yellow")   // Memory ~88%
  drawBar(term, 24, 6, 16, 16, "cyan")     // Requests (full)
  drawBar(term, 24, 7, 16, 2, "red")       // Errors (low, good)

  // Numeric readouts next to nothing — put them on the footer row.
  term.hline(1, 9, W - 2, "─", { fg: "cyan" })
  term.text(2, 10, "uptime 4d 02h", { fg: "green" })
  term.text(W - 16, 10, "load 0.42 OK", { fg: "brightgreen" })

  // Render the off-screen buffer to text and print it.
  print(term.dump())
}

// Draw a [####------] style bar: `value` filled cells out of `width`,
// the rest shown as light track cells.
fn drawBar(term, x, y, width, value, color) {
  // Filled portion.
  if (value > 0) {
    term.fill(x, y, value, 1, "█", { fg: color })
  }
  // Empty track for the remainder.
  let rest = width - value
  if (rest > 0) {
    term.fill(x + value, y, rest, 1, "░", { fg: "brightblack" })
  }
}

main()
