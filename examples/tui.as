// std/tui — self-contained, deterministic demo.
//
// Uses tui.buffer(w, h) to draw into a fixed-size OFF-SCREEN buffer (no real
// terminal, no raw mode, no alt screen, no flush), then prints the dump() — so
// the output is deterministic and CI-friendly. (init() is the real-terminal
// variant; flush() needs a real tty and is intentionally not used here.)
import * as tui from "std/tui"

let term = tui.buffer(14, 4)
term.box(0, 0, 14, 4, { fg: "cyan" })
term.text(2, 1, "AScript", { bold: true })
term.text(2, 2, "TUI demo")
print(term.dump())
