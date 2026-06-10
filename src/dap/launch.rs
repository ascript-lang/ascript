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
pub fn spawn_debuggee(program: PathBuf, script_args: Vec<String>) -> DebuggeeHandle {
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
            local.block_on(&rt, run_program(program, script_args, hook));
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
async fn run_program(program: PathBuf, script_args: Vec<String>, mut hook: DebuggerHook) {
    use crate::error::SourceInfo;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;
    use std::rc::Rc;

    // ---- compile (mirrors run_file_on_vm_with_packages) --------------------
    let src = match std::fs::read_to_string(&program) {
        Ok(s) => s,
        Err(e) => {
            let _ = hook.events.send(DebugEvent::Output {
                text: format!("cannot read {}: {}\n", program.display(), e),
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
            });
            let _ = hook.events.send(DebugEvent::Terminated { exit_code: 1 });
            return;
        }
    };
    chunk.set_module_source(&src_info);

    // Capture program output (so it rides DAP `output` events, OFF the protocol
    // stdout which the framing owns). `Interp::new()` uses the Capture sink.
    let interp = Rc::new(crate::interp::Interp::new());
    interp.set_cli_args(&script_args);
    interp.set_worker_source(&src);
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
        Err(crate::interp::Control::Panic(e)) => {
            Some(format!("{}\n", e.clone().with_source(src_info).message))
        }
        _ => None,
    };

    if let Some(hook) = hook {
        // Ship the captured program output as a single `output` chunk.
        let out = interp.output();
        if !out.is_empty() {
            let _ = hook.events.send(DebugEvent::Output { text: out });
        }
        if let Some(text) = panic_text {
            let _ = hook.events.send(DebugEvent::Output { text });
        }
        let _ = hook.events.send(DebugEvent::Terminated { exit_code });
        // `hook` drops here → its event Sender drops → pump thread sees the channel
        // close and ends.
    }
}
