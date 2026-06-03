//! The bytecode disassembler — the substrate for compiler unit tests (assert
//! emitted opcodes by reading them back) and the primary VM debugging tool.
//!
//! [`disasm`] renders an entire [`Chunk`] (header, every instruction, then each
//! nested proto recursively). [`disasm_at`] renders a single instruction and
//! advances the offset, so the VM trace loop can reuse the exact same formatting.
//!
//! Instruction format:
//! ```text
//! 0000 OP_NAME    operand ; comment
//! ```
//! a zero-padded 4-digit offset, the op name left-justified to a fixed width, the
//! inline operand(s), then a `;` comment giving context (the const value, the
//! absolute jump target, the proto index/name, ...).

use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;
use std::fmt::Write as _;

/// Column width the opcode name is padded to (keeps operands aligned).
const OP_COL: usize = 16;

/// Disassemble an entire chunk: a header line, every instruction in order, then
/// each nested proto recursively (each under its own header).
pub fn disasm(chunk: &Chunk) -> String {
    let mut out = String::new();
    disasm_into(chunk, "<script>", &mut out);
    out
}

/// Render `chunk` (with the given display `title`) into `out`, then recurse into
/// its nested protos.
fn disasm_into(chunk: &Chunk, title: &str, out: &mut String) {
    let _ = writeln!(out, "== {title} ==");

    let mut offset = 0;
    while offset < chunk.code.len() {
        let line = disasm_at(chunk, &mut offset);
        out.push_str(&line);
        out.push('\n');
    }

    for (i, proto) in chunk.protos.iter().enumerate() {
        out.push('\n');
        let name = proto.chunk.name.as_deref().unwrap_or("<anonymous>");
        let title = format!("fn {name} (proto #{i})");
        disasm_into(&proto.chunk, &title, out);
    }
}

/// Disassemble exactly one instruction at `*offset`, advancing `*offset` past it
/// (opcode byte + [`Op::operand_width`]). Returns the formatted line (no trailing
/// newline).
///
/// If the byte at `*offset` does not decode to an [`Op`], emits `?? <byte>` and
/// advances a single byte (defensive — should not happen on a valid chunk).
pub fn disasm_at(chunk: &Chunk, offset: &mut usize) -> String {
    let at = *offset;
    let byte = chunk.code[at];

    let Some(op) = Op::from_u8(byte) else {
        *offset = at + 1;
        return format!("{at:04} ?? {byte}");
    };

    let width = op.operand_width();
    let name = op_name(op);
    // Advance past the opcode byte and its inline operands.
    *offset = at + 1 + width;

    let mut line = format!("{at:04} {name:<OP_COL$}");

    match op {
        // u16 const-pool index → show the referenced value.
        Op::Const | Op::GetGlobal | Op::SetGlobal => {
            let idx = chunk.read_u16(at + 1);
            let _ = write!(line, "{idx:>5} ; {}", const_repr(chunk, idx));
        }
        // i16 relative jump → show the absolute target offset.
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNotNil | Op::Loop => {
            let disp = chunk.read_i16(at + 1);
            let after = at + 1 + width;
            let target = (after as i64 + disp as i64) as usize;
            let _ = write!(line, "{disp:>5} ; -> {target:04}");
        }
        // u8 call argument count.
        Op::Call => {
            let argc = chunk.read_u8(at + 1);
            let _ = write!(line, "{argc:>5}");
        }
        // u16 method-name const index + u8 argc → show the name and arg count.
        Op::CallMethod => {
            let idx = chunk.read_u16(at + 1);
            let argc = chunk.read_u8(at + 3);
            let _ = write!(line, "{idx:>5} {argc} ; .{}", const_repr(chunk, idx));
        }
        // u16 proto-table index → show the proto and (if any) its name.
        Op::Closure => {
            let idx = chunk.read_u16(at + 1);
            let _ = write!(line, "{idx:>5} ; proto #{idx}");
            if let Some(proto) = chunk.protos.get(idx as usize) {
                if let Some(name) = proto.chunk.name.as_deref() {
                    let _ = write!(line, " {name}");
                }
            }
        }
        // u16 const-pool index (a destructure key, or the bound-keys array) →
        // show the referenced value.
        Op::ObjectKey | Op::ObjectRest => {
            let idx = chunk.read_u16(at + 1);
            let _ = write!(line, "{idx:>5} ; {}", const_repr(chunk, idx));
        }
        // Other u16-operand ops: just show the operand (no special comment).
        _ if width == 2 => {
            let idx = chunk.read_u16(at + 1);
            let _ = write!(line, "{idx:>5}");
        }
        // Other u8-operand ops: just show the byte.
        _ if width == 1 => {
            let b = chunk.read_u8(at + 1);
            let _ = write!(line, "{b:>5}");
        }
        // Zero-operand ops: nothing further.
        _ => {}
    }

    // Trim any trailing padding spaces left when an op carries no operand.
    let trimmed_len = line.trim_end().len();
    line.truncate(trimmed_len);
    line
}

/// A short, readable representation of `consts[idx]` for an instruction comment:
/// the `Value`'s `Display`, with strings quoted so they stand out, falling back
/// to `Debug` for an out-of-range index (defensive).
fn const_repr(chunk: &Chunk, idx: u16) -> String {
    match chunk.consts.get(idx as usize) {
        Some(crate::value::Value::Str(s)) => format!("{s:?}"),
        Some(v) => v.to_string(),
        None => format!("?? const {idx}"),
    }
}

/// The canonical `OP_NAME` (screaming snake case) for an opcode, used in
/// disassembly and traces.
fn op_name(op: Op) -> &'static str {
    use Op::*;
    match op {
        Const => "CONST",
        Nil => "NIL",
        True => "TRUE",
        False => "FALSE",
        Pop => "POP",
        Dup => "DUP",
        Swap => "SWAP",
        Rot3 => "ROT3",
        GetLocal => "GET_LOCAL",
        SetLocal => "SET_LOCAL",
        GetUpvalue => "GET_UPVALUE",
        SetUpvalue => "SET_UPVALUE",
        CloseUpvalue => "CLOSE_UPVALUE",
        GetGlobal => "GET_GLOBAL",
        SetGlobal => "SET_GLOBAL",
        Add => "ADD",
        Sub => "SUB",
        Mul => "MUL",
        Div => "DIV",
        Mod => "MOD",
        Pow => "POW",
        Neg => "NEG",
        Not => "NOT",
        Eq => "EQ",
        Ne => "NE",
        Lt => "LT",
        Le => "LE",
        Gt => "GT",
        Ge => "GE",
        Range => "RANGE",
        CheckNumbers => "CHECK_NUMBERS",
        Jump => "JUMP",
        JumpIfFalse => "JUMP_IF_FALSE",
        JumpIfTrue => "JUMP_IF_TRUE",
        JumpIfNotNil => "JUMP_IF_NOT_NIL",
        Loop => "LOOP",
        Call => "CALL",
        CallMethod => "CALL_METHOD",
        Return => "RETURN",
        Closure => "CLOSURE",
        NewArray => "NEW_ARRAY",
        NewObject => "NEW_OBJECT",
        Spread => "SPREAD",
        GetIndex => "GET_INDEX",
        SetIndex => "SET_INDEX",
        GetProp => "GET_PROP",
        SetProp => "SET_PROP",
        GetPropOpt => "GET_PROP_OPT",
        Class => "CLASS",
        Method => "METHOD",
        GetSuper => "GET_SUPER",
        InstanceOf => "INSTANCE_OF",
        Template => "TEMPLATE",
        Await => "AWAIT",
        Yield => "YIELD",
        MakeGenerator => "MAKE_GENERATOR",
        Propagate => "PROPAGATE",
        Unwrap => "UNWRAP",
        Import => "IMPORT",
        GetIter => "GET_ITER",
        IterNext => "ITER_NEXT",
        IterClose => "ITER_CLOSE",
        IterSnapshot => "ITER_SNAPSHOT",
        ArrayLen => "ARRAY_LEN",
        GetLocalCell => "GET_LOCAL_CELL",
        SetLocalCell => "SET_LOCAL_CELL",
        FreshCell => "FRESH_CELL",
        CheckArrayDestructure => "CHECK_ARRAY_DESTRUCTURE",
        CheckObjectDestructure => "CHECK_OBJECT_DESTRUCTURE",
        ArrayElem => "ARRAY_ELEM",
        ObjectKey => "OBJECT_KEY",
        ArrayRest => "ARRAY_REST",
        ObjectRest => "OBJECT_REST",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::value::Value;
    use crate::vm::chunk::{Chunk, FnProto};
    use std::rc::Rc;

    fn s() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn disasm_const_add_return() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(1.0));
        let b = c.add_const(Value::Number(2.0));
        assert_eq!((a, b), (0, 1));
        c.emit_u16(Op::Const, a, s()); // offset 0, 3 bytes
        c.emit_u16(Op::Const, b, s()); // offset 3, 3 bytes
        c.emit(Op::Add, s()); // offset 6
        c.emit(Op::Return, s()); // offset 7

        let text = disasm(&c);
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines[0].starts_with("== <script> =="));
        // Monotonic, correctly-sized offsets.
        assert!(lines[1].starts_with("0000 "), "got {:?}", lines[1]);
        assert!(lines[2].starts_with("0003 "), "got {:?}", lines[2]);
        assert!(lines[3].starts_with("0006 "), "got {:?}", lines[3]);
        assert!(lines[4].starts_with("0007 "), "got {:?}", lines[4]);

        assert!(lines[1].contains("CONST") && lines[1].ends_with("; 1"));
        assert!(lines[2].contains("CONST") && lines[2].ends_with("; 2"));
        assert!(lines[3].contains("ADD"));
        assert!(lines[4].contains("RETURN"));
    }

    #[test]
    fn disasm_forward_jump_shows_absolute_target() {
        let mut c = Chunk::new();
        let site = c.emit_jump(Op::Jump, s()); // op at 0, operand at 1, 3 bytes total
        c.emit(Op::Nil, s()); // offset 3
        c.emit(Op::Pop, s()); // offset 4
        c.patch_jump(site); // target = 5

        let text = disasm(&c);
        let jump_line = text.lines().find(|l| l.contains("JUMP")).unwrap();
        assert!(jump_line.contains("-> 0005"), "got {jump_line:?}");
    }

    #[test]
    fn disasm_loop_shows_absolute_target() {
        let mut c = Chunk::new();
        let top = c.code.len(); // 0
        c.emit(Op::Nil, s()); // offset 0
        c.emit_loop(Op::Loop, top, s()); // op at 1

        let text = disasm(&c);
        let loop_line = text.lines().find(|l| l.contains("LOOP")).unwrap();
        assert!(loop_line.contains("-> 0000"), "got {loop_line:?}");
    }

    #[test]
    fn disasm_call_shows_argc() {
        let mut c = Chunk::new();
        c.emit_u8(Op::Call, 3, s());
        let text = disasm(&c);
        let call_line = text.lines().find(|l| l.contains("CALL")).unwrap();
        assert!(call_line.trim_end().ends_with("3"), "got {call_line:?}");
    }

    #[test]
    fn disasm_string_const_is_quoted() {
        let mut c = Chunk::new();
        let i = c.add_const(Value::Str(Rc::from("hi")));
        c.emit_u16(Op::GetGlobal, i, s());
        let text = disasm(&c);
        let line = text.lines().find(|l| l.contains("GET_GLOBAL")).unwrap();
        assert!(line.contains("; \"hi\""), "got {line:?}");
    }

    #[test]
    fn disasm_recurses_into_nested_proto() {
        // Nested proto with its own body and a name.
        let mut inner = Chunk::new();
        inner.name = Some("greet".to_string());
        inner.emit(Op::Nil, s());
        inner.emit(Op::Return, s());
        let proto = Rc::new(FnProto {
            chunk: inner,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });

        let mut c = Chunk::new();
        let pi = c.add_proto(proto);
        c.emit_u16(Op::Closure, pi, s());

        let text = disasm(&c);
        // The CLOSURE line references the proto by index and name.
        let closure_line = text.lines().find(|l| l.contains("CLOSURE")).unwrap();
        assert!(closure_line.contains("proto #0"), "got {closure_line:?}");
        assert!(closure_line.contains("greet"), "got {closure_line:?}");

        // The nested proto gets its own header and instructions.
        assert!(text.contains("== fn greet (proto #0) =="), "missing proto header in:\n{text}");
        // Two scripts' worth of RETURN: the inner proto's body is present.
        assert!(text.lines().any(|l| l.contains("NIL")), "missing inner NIL in:\n{text}");
    }

    #[test]
    fn disasm_at_advances_offset() {
        let mut c = Chunk::new();
        let i = c.add_const(Value::Number(7.0));
        c.emit_u16(Op::Const, i, s()); // 3 bytes
        c.emit(Op::Pop, s()); // 1 byte

        let mut off = 0;
        let l1 = disasm_at(&c, &mut off);
        assert_eq!(off, 3, "CONST advances 3 bytes");
        assert!(l1.starts_with("0000 ") && l1.contains("CONST"));

        let l2 = disasm_at(&c, &mut off);
        assert_eq!(off, 4, "POP advances 1 byte");
        assert!(l2.starts_with("0003 ") && l2.contains("POP"));
    }

    #[test]
    fn disasm_at_handles_undecodable_byte() {
        let mut c = Chunk::new();
        c.code.push(0xFF); // not a valid opcode
        let mut off = 0;
        let line = disasm_at(&c, &mut off);
        assert_eq!(off, 1, "undecodable byte advances exactly 1");
        assert!(line.contains("?? 255"), "got {line:?}");
    }
}
