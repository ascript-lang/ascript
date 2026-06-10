//! The debuggee thread: build the instrumented `Vm`, set break-on-entry, run the
//! program to completion, then ship `Output` + `Terminated` and drop the hook.
//!
//! This MIRRORS [`crate::run_file_on_vm_with_packages`] (compile → bind source →
//! build entry proto → `Closure`/`Fiber` → `vm.run` inside a `LocalSet`) but on a
//! dedicated thread with a `DebuggerHook` armed, exactly like the launch tests in
//! `src/vm/run.rs`.

use crate::vm::instrument::{DebugCommand, DebugEvent, DebuggerHook, Instrumentation};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// The controller ends the DAP server holds after spawning the debuggee.
pub struct DebuggeeHandle {
    /// Commands TO the parked VM (Continue / Step / SetBreakpoints / Clear).
    pub cmd_tx: Sender<DebugCommand>,
    /// Events FROM the VM (Stopped / BreakpointsVerified / Output / Terminated).
    pub evt_rx: Receiver<DebugEvent>,
    /// The debuggee thread join handle (joined on disconnect).
    pub join: std::thread::JoinHandle<()>,
}

/// Spawn the debuggee on its own dedicated, named, `WORKER_STACK_SIZE` thread with a
/// fresh current-thread tokio runtime + `LocalSet`. The program is compiled, the proto
/// tree registered, a break-on-entry breakpoint patched at the entry proto's offset 0,
/// and `vm.run` driven to completion. After the run, the captured program output is
/// shipped as [`DebugEvent::Output`] and a [`DebugEvent::Terminated`] carries the exit
/// code; then the hook (and its event `Sender`) is dropped so the pump thread sees the
/// channel close.
///
/// On a compile error (or unreadable file) we still ship a single `Output` line with the
/// error text and a non-zero `Terminated` so the editor session ends cleanly — there is
/// no proto tree to stop in.
pub fn spawn_debuggee(
    program: PathBuf,
    script_args: Vec<String>,
    caps: Option<crate::stdlib::caps::CapSet>,
) -> DebuggeeHandle {
    let (hook, cmd_tx, evt_rx) = DebuggerHook::new();
    let join = std::thread::Builder::new()
        .name("ascript-debuggee".to_string())
        .stack_size(crate::interp::WORKER_STACK_SIZE)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("debuggee tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run_program(program, script_args, caps, hook));
        })
        .expect("spawn debuggee thread");
    DebuggeeHandle {
        cmd_tx,
        evt_rx,
        join,
    }
}

/// The async body that runs on the debuggee thread. Owns the `hook` so its event
/// `Sender` lives exactly as long as the run (dropped at the end → pump sees EOF).
async fn run_program(
    program: PathBuf,
    script_args: Vec<String>,
    caps: Option<crate::stdlib::caps::CapSet>,
    mut hook: DebuggerHook,
) {
    use crate::error::SourceInfo;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;
    use std::rc::Rc;

    // Capture program output (so it rides DAP `output` events, OFF the protocol
    // stdout which the framing owns). `Interp::new()` uses the Capture sink.
    let interp = Rc::new(crate::interp::Interp::new());
    interp.set_cli_args(&script_args);
    // FFI §4.5 / Gate-0 (review F2): install the CLI/manifest-composed capability set
    // BEFORE running any code, so a debugged program is sandboxed EXACTLY like the same
    // program run normally (`--deny`/`--sandbox`/`--deny-net`/`--deny-fs` are honored
    // under `--inspect`). `None` keeps the default all-granted set.
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }

    // Acquire the entry chunk. A `.as` file is compiled from source; a `.aso` file is
    // loaded directly (no compile step) — its OPTIONAL embedded debug section (DBG Task 6)
    // re-binds the module source onto the chunk tree, so an `.aso`-only debug session has
    // line/variable info. A `--strip`ped `.aso` simply has no bound source, so the
    // debugger degrades gracefully (`build_line_starts` is empty → "no debug info"), it
    // does not error.
    let is_aso = program.extension().and_then(|e| e.to_str()) == Some("aso");
    let chunk = if is_aso {
        let bytes = match std::fs::read(&program) {
            Ok(b) => b,
            Err(e) => {
                let _ = hook.events.send(DebugEvent::Output {
                    text: format!("cannot read {}: {}\n", program.display(), e),
                    stderr: true,
                });
                let _ = hook.events.send(DebugEvent::Terminated { exit_code: 1 });
                return;
            }
        };
        let chunk = match crate::vm::chunk::Chunk::from_bytes_verified(&bytes) {
            Ok(c) => c,
            Err(e) => {
                let _ = hook.events.send(DebugEvent::Output {
                    text: format!("cannot load {}: {}\n", program.display(), e),
                    stderr: true,
                });
                let _ = hook.events.send(DebugEvent::Terminated { exit_code: 1 });
                return;
            }
        };
        // Workers Spec A (.aso path): retain the raw bytes so a `worker fn` can rebuild
        // its slice without source (mirrors `run_aso_file`).
        interp.set_worker_aso_bytes(Rc::from(bytes.into_boxed_slice()));
        chunk
    } else {
        let src = match std::fs::read_to_string(&program) {
            Ok(s) => s,
            Err(e) => {
                let _ = hook.events.send(DebugEvent::Output {
                    text: format!("cannot read {}: {}\n", program.display(), e),
                    stderr: true,
                });
                let _ = hook.events.send(DebugEvent::Terminated { exit_code: 1 });
                return;
            }
        };
        let src_info = Rc::new(SourceInfo {
            path: program.display().to_string(),
            text: src.clone(),
        });
        let chunk = match crate::compile::compile_source(&src) {
            Ok(c) => c,
            Err(e) => {
                let _ = hook.events.send(DebugEvent::Output {
                    text: format!("compile error: {}\n", e.message),
                    stderr: true,
                });
                let _ = hook.events.send(DebugEvent::Terminated { exit_code: 1 });
                return;
            }
        };
        chunk.set_module_source(&src_info);
        interp.set_worker_source(&src);
        chunk
    };

    interp.install_self();

    let vm = Vm::with_instrument(interp.clone(), Instrumentation::empty());
    if let Some(dir) = program.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }

    let entry = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });

    // Register the whole proto tree so the parked VM can resolve any (file,line).
    vm.register_debug_protos(&entry);

    // ---- break-on-entry: patch the entry proto's offset 0 BEFORE run -------
    // The first instruction traps → the existing Op::Break trap stops at entry → a
    // Stopped event ships. The entry breakpoint un-patches itself on first hit (v1
    // trap-once, already implemented in debug_stop's caller). Install the armed hook
    // into the VM's instrumentation.
    let entry_id = Rc::as_ptr(&entry) as *const () as usize;
    hook.set_breakpoint_shared(entry_id, 0, &entry.chunk);
    vm.install_debugger_hook(hook);

    // ---- run to completion -------------------------------------------------
    let closure = Closure::new(entry);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let result = vm.run(&mut fiber).await;
    crate::gc::collect();

    // Reclaim the hook (its event Sender) from the VM so we can ship the terminal
    // events and then drop it (closing the channel → pump thread ends).
    let hook = vm.take_debugger_hook();

    let exit_code = match result {
        Ok(RunOutcome::Done(_)) => 0,
        Ok(RunOutcome::Yielded(_)) => 0,
        Err(crate::interp::Control::Exit(code)) => code,
        Err(crate::interp::Control::Propagate(_)) => 0,
        Err(crate::interp::Control::Panic(_)) => 1,
    };
    let panic_text = match &result {
        // Only the message is shipped (the stderr `output` category) — the ariadne
        // source caret a CLI run renders is not reproduced here, so the chunk's bound
        // source (present for `.as` and a debug-info `.aso`, absent for a stripped one)
        // is not needed.
        Err(crate::interp::Control::Panic(e)) => Some(format!("{}\n", e.message)),
        _ => None,
    };

    if let Some(hook) = hook {
        // v1 trade-off (DOCUMENTED): the debuggee uses a Capture sink, so program
        // output is shipped as ONE `Output` chunk at termination — it is byte-identical
        // in CONTENT to a normal Live run (the Gate-9 observation test), but it does NOT
        // stream while paused at a breakpoint and is not interleaved with `stopped`
        // events. Incremental streaming (a channel-backed Live sink) is a follow-up.
        let out = interp.output();
        if !out.is_empty() {
            let _ = hook.events.send(DebugEvent::Output {
                text: out,
                stderr: false,
            });
        }
        // An uncaught Tier-2 panic goes to the stderr category (its message, sans the
        // ariadne caret a CLI run would render).
        if let Some(text) = panic_text {
            let _ = hook.events.send(DebugEvent::Output { text, stderr: true });
        }
        let _ = hook.events.send(DebugEvent::Terminated { exit_code });
        // `hook` drops here → its event Sender drops → pump thread sees the channel
        // close and ends.
    }
}
