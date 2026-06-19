//! CNTR §6 — inbound signal handlers (`process.on` / `process.off`).
//!
//! These tests spawn the built `ascript` binary running a script that registers a
//! signal handler, then send a REAL OS signal to the child and assert behavior.
//! Inherently Unix + timing-based: the parent must wait for the child to install its
//! handler before signalling (the script prints a "ready" line we block on), and the
//! timeouts are generous so a slow CI box does not flake. Gated `#[cfg(unix)]`.
//!
//! Coverage:
//!   - a SIGTERM handler runs, receives the signal NAME, and `exit(0)` ends clean;
//!   - an UNREGISTERED SIGTERM still kills with the OS default (exit 143 = 128+15);
//!   - `process.off("SIGTERM")` then SIGTERM → exit 143 (the §6.3 emulated restore);
//!   - a SIGINT handler runs;
//!   - last-wins: two `on`s for the same signal, the SECOND handler runs.
//!
//! Tier-2 cases (unknown name / SIGKILL / worker refusal) are in-process `run_source`
//! tests in `src/stdlib/process.rs`; this file is the real-signal half.

#![cfg(all(unix, feature = "sys"))]

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

/// Write `src` to a temp file and spawn `ascript run <file>` with stdout piped.
fn spawn_script(name: &str, src: &str) -> (Child, BufReader<std::process::ChildStdout>) {
    let file = std::env::temp_dir().join(name);
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ascript");
    let stdout = child.stdout.take().expect("piped stdout");
    (child, BufReader::new(stdout))
}

/// Block until the child prints a line == `marker` (it signals "handler installed").
/// Panics on EOF / timeout so a hung test fails loudly rather than racing.
fn wait_for_line(reader: &mut BufReader<std::process::ChildStdout>, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if Instant::now() > deadline {
            panic!("timed out waiting for line {marker:?}");
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read child stdout");
        if n == 0 {
            panic!("child closed stdout before printing {marker:?}");
        }
        if line.trim_end() == marker {
            return;
        }
    }
}

/// Drain the rest of the child's stdout to a String.
fn drain(reader: &mut BufReader<std::process::ChildStdout>) -> String {
    let mut rest = String::new();
    let _ = reader.read_to_string(&mut rest);
    rest
}

fn send(child: &Child, sig: i32) {
    let pid = child.id() as i32;
    // SAFETY: thin syscall wrapper; pid is our own child.
    unsafe {
        libc_kill(pid, sig);
    }
}

/// Wait for the child to exit, returning its raw exit code (or signal-derived code).
fn wait_code(mut child: Child) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not exit in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sigterm_handler_runs_receives_name_and_exits_clean() {
    let src = r#"
import { on } from "std/process"
import { sleep } from "std/time"
on("SIGTERM", (sig) => {
  print(sig)
  exit(0)
})
print("ready")
let i = 0
while (i < 100000) {
  await sleep(50)
  i = i + 1
}
"#;
    let (child, mut reader) = spawn_script("sig_term_handler.as", src);
    wait_for_line(&mut reader, "ready");
    send(&child, SIGTERM);
    let rest = drain(&mut reader);
    let code = wait_code(child);
    assert!(
        rest.contains("SIGTERM"),
        "handler should receive the signal NAME, got stdout {rest:?}"
    );
    assert_eq!(code, 0, "handler called exit(0), child must exit 0");
}

#[test]
fn unregistered_sigterm_kills_with_os_default() {
    // No `on`: SIGTERM is NOT intercepted; the OS default kills → 128+15 = 143.
    let src = r#"
import { sleep } from "std/time"
print("ready")
let i = 0
while (i < 100000) {
  await sleep(50)
  i = i + 1
}
"#;
    let (child, mut reader) = spawn_script("sig_unregistered.as", src);
    wait_for_line(&mut reader, "ready");
    send(&child, SIGTERM);
    let code = wait_code(child);
    assert_eq!(code, 143, "unregistered SIGTERM = OS default kill (128+15)");
}

#[test]
fn off_restores_os_default_kill() {
    // Register, then `off`: the next SIGTERM prints nothing and exits 128+15 = 143.
    let src = r#"
import { on, off } from "std/process"
import { sleep } from "std/time"
on("SIGTERM", (sig) => { print("handled") })
off("SIGTERM")
print("ready")
let i = 0
while (i < 100000) {
  await sleep(50)
  i = i + 1
}
"#;
    let (child, mut reader) = spawn_script("sig_off_restore.as", src);
    wait_for_line(&mut reader, "ready");
    send(&child, SIGTERM);
    let rest = drain(&mut reader);
    let code = wait_code(child);
    assert!(
        !rest.contains("handled"),
        "after off(), the handler must NOT run, got {rest:?}"
    );
    assert_eq!(code, 143, "off() restores OS-default kill (128+15)");
}

#[test]
fn sigint_handler_runs() {
    let src = r#"
import { on } from "std/process"
import { sleep } from "std/time"
on("SIGINT", (sig) => {
  print(sig)
  exit(0)
})
print("ready")
let i = 0
while (i < 100000) {
  await sleep(50)
  i = i + 1
}
"#;
    let (child, mut reader) = spawn_script("sig_int_handler.as", src);
    wait_for_line(&mut reader, "ready");
    send(&child, SIGINT);
    let rest = drain(&mut reader);
    let code = wait_code(child);
    assert!(
        rest.contains("SIGINT"),
        "SIGINT handler should run + receive name, got {rest:?}"
    );
    assert_eq!(code, 0);
}

#[test]
fn registered_handler_does_not_keep_process_alive() {
    // CNTR §6 exit-hang regression: a registered signal handler is a DAEMON listener —
    // it must NOT keep the process alive on its own. When the main program flow
    // completes, the process exits cleanly (Node semantics: the listener is aborted at
    // program end, not awaited forever). Before the fix this hangs on `local.await`.
    let src = r#"
import { on } from "std/process"
on("SIGTERM", (sig) => { print("h") })
print("done")
"#;
    let (child, mut reader) = spawn_script("sig_no_keepalive.as", src);
    wait_for_line(&mut reader, "done");
    // No signal sent: the program is over. It must EXIT cleanly within a few seconds.
    let deadline = Instant::now() + Duration::from_secs(8);
    use std::os::unix::process::ExitStatusExt;
    let mut child = child;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            let code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            assert_eq!(code, 0, "program ended normally → exit 0, not {code}");
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("process HANGS after main completes — signal listener kept it alive");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn registered_handler_no_keepalive_tree_walker() {
    // Same as above but on the legacy tree-walker engine (the other real run path).
    let src = r#"
import { on } from "std/process"
on("SIGTERM", (sig) => { print("h") })
print("done")
"#;
    let file = std::env::temp_dir().join("sig_no_keepalive_tw.as");
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut child = Command::new(bin)
        .arg("run")
        .arg("--tree-walker")
        .arg(&file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ascript");
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    wait_for_line(&mut reader, "done");
    let deadline = Instant::now() + Duration::from_secs(8);
    use std::os::unix::process::ExitStatusExt;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            let code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            assert_eq!(code, 0, "tree-walker: program ended normally → exit 0, not {code}");
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("tree-walker: process HANGS after main completes");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn last_on_wins_second_handler_runs() {
    // Two `on`s for SIGTERM: the SECOND handler is the live one (registry swap).
    let src = r#"
import { on } from "std/process"
import { sleep } from "std/time"
on("SIGTERM", (sig) => { print("first"); exit(0) })
on("SIGTERM", (sig) => { print("second"); exit(0) })
print("ready")
let i = 0
while (i < 100000) {
  await sleep(50)
  i = i + 1
}
"#;
    let (child, mut reader) = spawn_script("sig_last_wins.as", src);
    wait_for_line(&mut reader, "ready");
    send(&child, SIGTERM);
    let rest = drain(&mut reader);
    let code = wait_code(child);
    assert!(
        rest.contains("second") && !rest.contains("first"),
        "last `on` wins: only the second handler runs, got {rest:?}"
    );
    assert_eq!(code, 0);
}
