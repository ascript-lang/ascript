//! `.aso` — AScript Object (compiled bytecode) serialization.
//!
//! A [`Chunk`] is serializable to a compact, self-contained binary because its
//! constant pool holds only COMPILE-TIME values (literal scalars + name strings +
//! prebuilt `enum` values) and its nested function bodies live in the `protos`
//! table (recursively serializable [`FnProto`]s). The *runtime* side tables — the
//! inline caches, adaptive arithmetic/global caches, and hidden-class shapes — are
//! deliberately NOT serialized: an `.aso` carries the GENERIC chunk, stable across
//! IC/specialization evolution. They are re-created EMPTY on load and the VM
//! re-populates them as the program runs.
//!
//! # Format
//!
//! ```text
//! magic:   b"ASO\0"                 (4 bytes)
//! version: u32 LE  = ASO_FORMAT_VERSION
//! <chunk>                           (see `write_chunk` / `read_chunk`)
//! ```
//!
//! A chunk serializes (in order):
//! `code` (len:u32 + bytes) · `consts` (count:u32 + tagged values) · `protos`
//! (count:u32 + each: flags + arity + params + ret + recursive chunk) ·
//! `class_protos` (count:u32 + each) · `imports` (count:u32 + each) · `spans`
//! (count:u32 + each `(u64,u64)`) · `upvalues` (count:u32 + tag + u32) ·
//! `cell_slots` (count:u32 + u32) · `slot_count` (u16) · `ic_count` (u16) ·
//! `name` (opt string).
//!
//! All multi-byte integers are little-endian; lengths are `u32`; `usize` source
//! offsets are widened to `u64` on write and checked-narrowed on read (so an `.aso`
//! built on a 64-bit host loads on any host).
//!
//! # Version policy
//!
//! [`ASO_FORMAT_VERSION`] is a hand-maintained monotonic counter. **Bump it on ANY
//! change to the opcode set, the `Value`/`Chunk`/`FnProto`/`ClassProto`/`ImportDesc`
//! layout, or this serialization format.** A version mismatch makes
//! [`Chunk::from_bytes`] return [`AsoError::VersionMismatch`] so the caller can
//! recompile from source (or hard-error) — stale bytecode is NEVER run.

use crate::ast::{
    ArrayElem, BinOp, CallArg, Expr, ExprKind, ObjEntry, Param, TemplatePart, Type, UnOp,
};
use crate::span::Span;
use crate::syntax::resolve::types::UpvalueDescriptor;
use crate::value::{Class, FieldSchema, Value};
use crate::vm::chunk::{Chunk, ClassProto, FnProto, ImportDesc};
use std::rc::Rc;

/// The `.aso` file magic: `ASO\0`.
pub const ASO_MAGIC: [u8; 4] = *b"ASO\0";

/// The serialization format version. BUMP THIS on any opcode-set or value/chunk
/// layout change, or any change to the byte layout below; a mismatch recompiles or
/// errors, never runs stale bytecode.
///
/// History:
/// - 1: initial `.aso` format (V12-T2).
/// - 2: (pre-existing).
/// - 3: field-default expr now covers the full `cst_default_expr` lowering — new
///   tags for binary/range/index/ternary/template/optmember/try/unwrap/await/
///   assign and spread elements in array/object/call defaults.
/// - 7: inclusive `..=` ranges — new `Op::RangeInclusive` opcode (shifts opcode
///   byte values) and a new `EX_RANGE` field-default expr tag (value-position
///   `a..b`/`a..=b` defaults serialize as `ExprKind::Range`, not `BinOp::Range`).
/// - 8: stepped ranges — three new opcodes (`RangeStepValue`/`RangeResolveStep`/
///   `RangeHasNext`) for signed-`step` value materialization + for-range iteration
///   (shifts opcode byte values for everything after `CheckNumbers`). The
///   field-default `EX_RANGE` byte layout is unchanged (step still rejected in
///   field-default position by `cst_default_expr`).
/// - 9: stepped match-range patterns — `Op::MatchRange`'s u8 operand changed from a
///   plain `inclusive` flag to a `flags` byte (bit0 = inclusive, bit1 = step
///   PRESENT) and its stack shape grew from `subject lo hi` to `subject lo hi step`
///   (a `nil` step placeholder when omitted). Opcode byte values are unchanged.
/// - 10: static methods (SP1 §3) — `ClassProto` gained a `static_method_names`
///   list (written right after `method_names`), so the class-proto byte layout
///   grew. Opcode byte values are unchanged.
/// - 11: arrow/`match` field defaults (SP1 §4) — a new `EX_REPARSE` field-default
///   expr tag carries the default's left-padded source text inline; on load it is
///   re-lowered through the legacy front-end. The class byte layout is unchanged
///   (the source rides inside the field-default expr stream). Opcode byte values
///   are unchanged.
/// - 12: `instanceof` operator (SP2 §1) — the compiler now legitimately emits the
///   formerly-dead `Op::InstanceOf` opcode, and the field-default binop wire-tag
///   set gained tag 16 (`BinOp::InstanceOf`). Old chunks never contained either, so
///   older readers must reject a v12 chunk. This is the single SP2 bump; later SP2
///   phases that touch emitted bytecode reuse it.
/// - 14: `#{…}` map literals (SP2 §3) — two new opcodes (`Op::NewMap`/`Op::MapEntry`,
///   inserted after `NewObject`, shifting all later opcode byte values) and a new
///   `EX_MAP` field-default expr tag (a `#{…}` literal in field-default position
///   serializes as `ExprKind::Map`). Old chunks never contained either, so older
///   readers must reject a v14 chunk.
/// - 15: SP8 #136 capture-by-value — `UpvalueDescriptor::ParentLocal` gained a
///   `by_value: bool`, serialized as a trailing u8 after the slot. The descriptor
///   layout changed (a v14 reader would mis-parse the extra byte), so older readers
///   must reject a v15 chunk and vice versa.
/// - 16: FnProto flags byte gained bit3 = is_worker (Workers Spec A).
/// - 17: FnProto flags byte gained bit4 = owning_class_present; if set, a
///   length-prefixed UTF-8 string follows the params/ret, carrying the
///   enclosing class name for `static worker fn` methods. Required so the VM
///   can route higher-order worker dispatch to
///   `build_code_slice_for_static_method` instead of `build_code_slice`.
/// - 18: the class template gained a `worker class` flag byte (Workers Spec B,
///   `Class.is_worker`), written by `write_class` before the field list. Drives
///   `ClassName.spawn(args)` actor routing on both engines.
/// - 19: NUM — the numeric model split `Value::Number(f64)` into the two kinds
///   `Value::Int(i64)` and `Value::Float(f64)`. The constant pool gained
///   `TAG_INT`, the field-default expr stream gained `EX_INT`, and the former
///   `TAG_NUMBER`/`EX_NUMBER` tags now carry `Float` (value-identical bytes).
pub const ASO_FORMAT_VERSION: u32 = 19;

/// An error from decoding (or, for [`AsoError::NonLiteralConst`], encoding) an
/// `.aso` byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsoError {
    /// The leading 4 bytes were not [`ASO_MAGIC`].
    BadMagic([u8; 4]),
    /// The header version did not match [`ASO_FORMAT_VERSION`].
    VersionMismatch { found: u32, expected: u32 },
    /// The byte stream ended before a field could be fully read.
    Truncated,
    /// A constant-pool tag byte did not name a known literal kind.
    BadConst(u8),
    /// A discriminant byte (value tag / type tag / expr tag / upvalue tag /
    /// import tag) was out of range.
    BadTag { what: &'static str, tag: u8 },
    /// A UTF-8 string field was not valid UTF-8.
    BadUtf8,
    /// A `u64` source offset / length did not fit the host `usize` (loading a
    /// 64-bit `.aso` on a smaller host).
    Overflow,
    /// Trailing bytes remained after the chunk was fully decoded.
    TrailingBytes,
    /// A non-literal `Value` was found in the constant pool during ENCODING — a
    /// compiler invariant violation (the pool must hold only literal scalars +
    /// prebuilt enums). Carries a short description of the offending kind.
    NonLiteralConst(&'static str),
    /// A field exceeded the `.aso` wire-format capacity during ENCODING (SP3 §A):
    /// a single string/bytes literal > `u32::MAX` bytes (`what = "byte field"`), or
    /// a serialized collection with > `u32::MAX` entries (`what = "collection"`).
    /// Returned from `to_bytes` instead of panicking; the `build` command maps it
    /// to a clean message + non-zero exit.
    TooLarge { what: &'static str, len: usize },
}

impl std::fmt::Display for AsoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsoError::BadMagic(got) => {
                write!(f, "not an .aso file (bad magic {got:?}, expected {ASO_MAGIC:?})")
            }
            AsoError::VersionMismatch { found, expected } => write!(
                f,
                ".aso format version mismatch: file is v{found}, this build expects v{expected} (recompile from source)"
            ),
            AsoError::Truncated => write!(f, ".aso byte stream truncated"),
            AsoError::BadConst(tag) => write!(f, "unknown constant-pool tag {tag}"),
            AsoError::BadTag { what, tag } => write!(f, "invalid {what} tag {tag}"),
            AsoError::BadUtf8 => write!(f, "invalid UTF-8 in .aso string field"),
            AsoError::Overflow => write!(f, ".aso length/offset exceeds host usize"),
            AsoError::TrailingBytes => write!(f, "trailing bytes after .aso chunk"),
            AsoError::NonLiteralConst(kind) => {
                write!(f, "non-literal value ({kind}) in constant pool (compiler bug)")
            }
            AsoError::TooLarge { what, .. } => match *what {
                "byte field" => write!(
                    f,
                    "value too large to serialize (a single string or bytes literal exceeds 4 GB)"
                ),
                _ => write!(
                    f,
                    "module too large to serialize (a table exceeds 4 billion entries); split the module into smaller files"
                ),
            },
        }
    }
}

impl std::error::Error for AsoError {}

// ---- constant-pool value tags ------------------------------------------------

const TAG_NIL: u8 = 0;
const TAG_BOOL: u8 = 1;
/// `Value::Float` (the former `Value::Number`; tag value unchanged so existing
/// float constants keep the same wire byte — NUM §8 "the `Float` tag is the
/// former `Number` tag, value-identical").
const TAG_NUMBER: u8 = 2;
const TAG_STR: u8 = 3;
const TAG_DECIMAL: u8 = 4;
const TAG_ENUM: u8 = 5;
/// `Value::Int` (NUM §8): a 64-bit signed integer constant. New tag.
const TAG_INT: u8 = 7;
/// An `Array` whose elements are themselves literal constants. Emitted by the
/// compiler for the object-rest bound-key list (`let {a, ...rest} = obj` lowers
/// to an `Op::ObjectRest` whose operand is a const-pool `Array` of key `Str`s).
/// The array is mutable at runtime (`Rc<RefCell<Vec<Value>>>`) but its *contents*
/// at compile time are a literal key list, so it round-trips byte-stably.
const TAG_ARRAY: u8 = 6;

// ---- type tags ---------------------------------------------------------------

const TY_NUMBER: u8 = 0;
const TY_STRING: u8 = 1;
const TY_BOOL: u8 = 2;
const TY_NIL: u8 = 3;
const TY_ANY: u8 = 4;
const TY_FN: u8 = 5;
const TY_OBJECT: u8 = 6;
const TY_ERROR: u8 = 7;
const TY_ARRAY: u8 = 8;
const TY_RESULT: u8 = 9;
const TY_TUPLE: u8 = 10;
const TY_UNION: u8 = 11;
const TY_NAMED: u8 = 12;
const TY_MAP: u8 = 13;
const TY_FUTURE: u8 = 14;
const TY_OPTIONAL: u8 = 15;

// ---- field-default expr tags (the subset `cst_default_expr` emits) -----------

/// A `float` literal default (`ExprKind::Float`; the former `ExprKind::Number`
/// tag, value-identical — NUM §8).
const EX_NUMBER: u8 = 0;
const EX_STR: u8 = 1;
const EX_BOOL: u8 = 2;
const EX_NIL: u8 = 3;
const EX_IDENT: u8 = 4;
const EX_PAREN: u8 = 5;
const EX_UNARY: u8 = 6;
const EX_ARRAY: u8 = 7;
const EX_OBJECT: u8 = 8;
const EX_MEMBER: u8 = 9;
const EX_CALL: u8 = 10;
const EX_BINARY: u8 = 11;
const EX_INDEX: u8 = 12;
const EX_OPTMEMBER: u8 = 13;
const EX_TERNARY: u8 = 14;
const EX_TEMPLATE: u8 = 15;
const EX_TRY: u8 = 16;
const EX_UNWRAP: u8 = 17;
const EX_AWAIT: u8 = 18;
const EX_ASSIGN: u8 = 19;
/// A value-position range `a..b` / `a..=b` (`ExprKind::Range`); `step` defaults
/// never reach here (`cst_default_expr` rejects a stepped default).
const EX_RANGE: u8 = 20;
/// An ARROW or `match` field default, lowered by RE-PARSING source text (SP1 §4,
/// format v11). The payload is the left-padded source string (`reparse_default_source`);
/// on load it is re-lowered through the legacy front-end (`reparse_default_from_source`)
/// to the identical `Expr`. Emitted by `write_field_default` (consulting the class's
/// `default_sources`), NOT by the generic `write_expr` (which never sees these forms).
const EX_REPARSE: u8 = 21;
/// A `#{…}` map literal field default (`ExprKind::Map`, SP2 §3). Followed by a
/// `len`-prefixed sequence of (key-expr, value-expr) pairs.
const EX_MAP: u8 = 22;
/// An `int` literal default (`ExprKind::Int`, NUM §3.1). New tag. The payload is
/// the i64 stored as u64 bits.
const EX_INT: u8 = 23;

// Template-part tags (within an `EX_TEMPLATE`).
const TP_LIT: u8 = 0;
const TP_EXPR: u8 = 1;

// Array/object/call element tags so spreads round-trip (`cst_default_expr` lowers
// spread elements in array/object/call defaults).
const EL_ITEM: u8 = 0;
const EL_SPREAD: u8 = 1;

// ---- upvalue / import tags ---------------------------------------------------

const UV_PARENT_LOCAL: u8 = 0;
const UV_PARENT_UPVALUE: u8 = 1;

const IMP_NAMED: u8 = 0;
const IMP_NAMESPACE: u8 = 1;

// =============================================================================
// Writer
// =============================================================================

/// A minimal little-endian byte sink.
struct Writer {
    buf: Vec<u8>,
    /// The FIRST capacity overflow hit while writing (SP3 §A), sticky / first-wins.
    /// `bytes`/`len` record here and write a placeholder length instead of
    /// panicking; `to_bytes` checks it once after writing the whole chunk and
    /// returns the typed error. `None` for every in-range module.
    overflow: Option<AsoError>,
}

impl Writer {
    fn new() -> Self {
        Writer {
            buf: Vec::new(),
            overflow: None,
        }
    }
    /// Encode `n` as a `u32` length prefix; on overflow (> `u32::MAX`) record a
    /// sticky [`AsoError::TooLarge`] (`what`) and write the clamped placeholder.
    /// Pure (no allocation), so the > 4 GB capacity path is unit-testable without
    /// materializing the data.
    fn write_len(&mut self, n: usize, what: &'static str) {
        match u32::try_from(n) {
            Ok(v) => self.u32(v),
            Err(_) => {
                if self.overflow.is_none() {
                    self.overflow = Some(AsoError::TooLarge { what, len: n });
                }
                self.u32(u32::MAX);
            }
        }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        // Bit pattern (so NaN/-0.0 round-trip exactly).
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.write_len(b.len(), "byte field");
        self.buf.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    /// An `Option<String>` as a 1-byte present flag + (if present) the string.
    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    /// A `usize` source offset, widened to `u64`.
    fn usize(&mut self, v: usize) {
        self.u64(v as u64);
    }
    fn len(&mut self, n: usize) {
        self.write_len(n, "collection");
    }
}

// =============================================================================
// Reader
// =============================================================================

/// A bounds-checked little-endian byte source over a borrowed slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], AsoError> {
        let end = self.pos.checked_add(n).ok_or(AsoError::Overflow)?;
        if end > self.buf.len() {
            return Err(AsoError::Truncated);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, AsoError> {
        Ok(self.take(1)?[0])
    }
    /// Read the next byte WITHOUT advancing (so the caller can branch on a tag and
    /// then consume it or fall through to a uniform reader).
    fn peek_u8(&self) -> Result<u8, AsoError> {
        self.buf.get(self.pos).copied().ok_or(AsoError::Truncated)
    }
    fn u16(&mut self) -> Result<u16, AsoError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, AsoError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, AsoError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn f64(&mut self) -> Result<f64, AsoError> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn usize(&mut self) -> Result<usize, AsoError> {
        usize::try_from(self.u64()?).map_err(|_| AsoError::Overflow)
    }
    /// A `u32` length narrowed to `usize`.
    fn len(&mut self) -> Result<usize, AsoError> {
        usize::try_from(self.u32()?).map_err(|_| AsoError::Overflow)
    }
    /// Bytes left unread — the hard ceiling on any length-driven pre-allocation.
    /// A declared element count larger than this cannot possibly be satisfied (every
    /// element is ≥ 1 byte), so clamping `reserve`/`with_capacity` to `remaining()`
    /// turns an attacker-controlled length into a bounded allocation; the per-element
    /// decode loop then reports the short read as a clean `Truncated` error instead of
    /// the process aborting on a multi-gigabyte allocation. The real (unclamped) count
    /// still drives the loop, so a well-formed stream is unaffected.
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    fn bytes(&mut self) -> Result<&'a [u8], AsoError> {
        let n = self.len()?;
        self.take(n)
    }
    fn str(&mut self) -> Result<String, AsoError> {
        let b = self.bytes()?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| AsoError::BadUtf8)
    }
    fn opt_str(&mut self) -> Result<Option<String>, AsoError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.str()?)),
            tag => Err(AsoError::BadTag {
                what: "opt-string",
                tag,
            }),
        }
    }
    fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }
}

// =============================================================================
// Chunk (de)serialization
// =============================================================================

impl Chunk {
    /// Serialize this chunk to a self-contained `.aso` byte vector: the magic
    /// header, the format version, then the chunk body.
    ///
    /// Returns [`AsoError::TooLarge`] if a single string/bytes literal exceeds 4 GB
    /// or a serialized table exceeds `u32::MAX` entries (SP3 §A) — a clean error,
    /// never a panic.
    ///
    /// # Panics
    /// If the constant pool holds a non-literal value (a compiler invariant
    /// violation). Use [`Chunk::check_consts_literal_only`] for a non-panicking
    /// check. (The literal-only invariant is also asserted per-value during
    /// encoding via [`write_value`].)
    pub fn to_bytes(&self) -> Result<Vec<u8>, AsoError> {
        let mut w = Writer::new();
        w.buf.extend_from_slice(&ASO_MAGIC);
        w.u32(ASO_FORMAT_VERSION);
        write_chunk(&mut w, self)
            .expect("constant pool must be literals-only (compiler invariant)");
        // SP3 §A: surface a wire-format capacity overflow as a clean typed error.
        if let Some(e) = w.overflow {
            return Err(e);
        }
        Ok(w.buf)
    }

    /// Deserialize a chunk from an `.aso` byte stream, validating the magic and
    /// format version first. The runtime side tables (inline caches, adaptive
    /// arith/global caches) are rebuilt EMPTY — the VM re-populates them on run.
    pub fn from_bytes(bytes: &[u8]) -> Result<Chunk, AsoError> {
        let mut r = Reader::new(bytes);
        let magic = r.take(4)?;
        if magic != ASO_MAGIC {
            return Err(AsoError::BadMagic([magic[0], magic[1], magic[2], magic[3]]));
        }
        let version = r.u32()?;
        if version != ASO_FORMAT_VERSION {
            return Err(AsoError::VersionMismatch {
                found: version,
                expected: ASO_FORMAT_VERSION,
            });
        }
        let chunk = read_chunk(&mut r)?;
        if !r.at_end() {
            return Err(AsoError::TrailingBytes);
        }
        Ok(chunk)
    }

    /// Verify that every constant-pool value is a serializable literal
    /// (`Nil`/`Bool`/`Number`/`Str`/`Decimal`/`Enum`). Returns the offending kind
    /// description on the first violation. A passing chunk is guaranteed to
    /// `to_bytes` without panicking on its pool.
    pub fn check_consts_literal_only(&self) -> Result<(), &'static str> {
        for v in &self.consts {
            literal_kind(v)?;
        }
        Ok(())
    }
}

/// The serializable "literal kind" name for a constant-pool value, or an error
/// naming the non-literal variant.
fn literal_kind(v: &Value) -> Result<&'static str, &'static str> {
    match v {
        Value::Nil => Ok("nil"),
        Value::Bool(_) => Ok("bool"),
        Value::Int(_) => Ok("int"),
        Value::Float(_) => Ok("number"),
        Value::Str(_) => Ok("string"),
        Value::Decimal(_) => Ok("decimal"),
        Value::Enum(_) => Ok("enum"),
        Value::Builtin(_) => Err("builtin"),
        Value::Function(_) => Err("function"),
        Value::Closure(_) => Err("closure"),
        // An Array constant is serializable iff every element is itself a literal
        // (the compiler only ever pools the object-rest bound-key list, an array
        // of `Str`s; recurse so a non-literal element is still rejected cleanly).
        Value::Array(a) => {
            for e in a.borrow().iter() {
                literal_kind(e)?;
            }
            Ok("array")
        }
        Value::Object(_) => Err("object"),
        Value::Map(_) => Err("map"),
        Value::Bytes(_) => Err("bytes"),
        Value::EnumVariant(_) => Err("enum-variant"),
        Value::Class(_) => Err("class"),
        Value::Instance(_) => Err("instance"),
        _ => Err("non-literal"),
    }
}

fn write_chunk(w: &mut Writer, c: &Chunk) -> Result<(), AsoError> {
    // code
    w.bytes(&c.code);
    // consts
    w.len(c.consts.len());
    for v in &c.consts {
        write_value(w, v)?;
    }
    // protos (recursive)
    w.len(c.protos.len());
    for p in &c.protos {
        write_proto(w, p)?;
    }
    // class_protos
    w.len(c.class_protos.len());
    for cp in &c.class_protos {
        write_class_proto(w, cp)?;
    }
    // imports
    w.len(c.imports.len());
    for imp in &c.imports {
        write_import(w, imp);
    }
    // spans
    w.len(c.spans.len());
    for (off, span) in &c.spans {
        w.usize(*off);
        w.usize(span.start);
        w.usize(span.end);
    }
    // upvalues
    w.len(c.upvalues.len());
    for uv in &c.upvalues {
        write_upvalue(w, uv);
    }
    // cell_slots
    w.len(c.cell_slots.len());
    for slot in &c.cell_slots {
        w.u32(*slot);
    }
    // scalars
    w.u16(c.slot_count);
    w.u16(c.ic_count);
    // name
    w.opt_str(c.name.as_deref());
    Ok(())
}

fn read_chunk(r: &mut Reader) -> Result<Chunk, AsoError> {
    let mut c = Chunk::new();
    // code
    c.code = r.bytes()?.to_vec();
    // consts
    let n = r.len()?;
    c.consts.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.consts.push(read_value(r)?);
    }
    // protos
    let n = r.len()?;
    c.protos.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.protos.push(Rc::new(read_proto(r)?));
    }
    // class_protos
    let n = r.len()?;
    c.class_protos.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.class_protos.push(Rc::new(read_class_proto(r)?));
    }
    // imports
    let n = r.len()?;
    c.imports.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.imports.push(read_import(r)?);
    }
    // spans
    let n = r.len()?;
    c.spans.reserve(n.min(r.remaining()));
    for _ in 0..n {
        let off = r.usize()?;
        let start = r.usize()?;
        let end = r.usize()?;
        c.spans.push((off, Span::new(start, end)));
    }
    // upvalues
    let n = r.len()?;
    c.upvalues.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.upvalues.push(read_upvalue(r)?);
    }
    // cell_slots
    let n = r.len()?;
    c.cell_slots.reserve(n.min(r.remaining()));
    for _ in 0..n {
        c.cell_slots.push(r.u32()?);
    }
    // scalars
    c.slot_count = r.u16()?;
    c.ic_count = r.u16()?;
    // name
    c.name = r.opt_str()?;
    // runtime side tables are left at their `Default` (empty) — the VM re-fills
    // them on run. `Chunk::new()` already gives empty RefCell maps:
    debug_assert!(c.field_ics.borrow().is_empty());
    debug_assert!(c.method_ics.borrow().is_empty());
    debug_assert!(c.arith_caches.borrow().is_empty());
    debug_assert!(c.global_caches.borrow().is_empty());
    Ok(c)
}

// ---- Value (constant pool) ---------------------------------------------------

fn write_value(w: &mut Writer, v: &Value) -> Result<(), AsoError> {
    match v {
        Value::Nil => w.u8(TAG_NIL),
        Value::Bool(b) => {
            w.u8(TAG_BOOL);
            w.u8(u8::from(*b));
        }
        Value::Int(i) => {
            w.u8(TAG_INT);
            w.u64(*i as u64);
        }
        Value::Float(n) => {
            w.u8(TAG_NUMBER);
            w.f64(*n);
        }
        Value::Str(s) => {
            w.u8(TAG_STR);
            w.str(s);
        }
        Value::Decimal(d) => {
            w.u8(TAG_DECIMAL);
            w.buf.extend_from_slice(&d.serialize());
        }
        Value::Enum(e) => {
            w.u8(TAG_ENUM);
            w.str(&e.name);
            w.len(e.variants.len());
            for (vname, variant) in &e.variants {
                w.str(vname);
                // Each variant is a Value::EnumVariant; serialize its fields.
                match variant {
                    Value::EnumVariant(ev) => {
                        w.str(&ev.enum_name);
                        w.str(&ev.name);
                        write_value(w, &ev.value)?;
                    }
                    other => {
                        return Err(AsoError::NonLiteralConst(
                            literal_kind(other).err().unwrap_or("enum-variant-payload"),
                        ))
                    }
                }
            }
        }
        Value::Array(a) => {
            // Only literal-element arrays are poolable (the object-rest key list).
            // Each element is re-checked via `write_value`, so a non-literal
            // element surfaces as `NonLiteralConst` rather than silently encoding.
            w.u8(TAG_ARRAY);
            let elems = a.borrow();
            w.len(elems.len());
            for e in elems.iter() {
                write_value(w, e)?;
            }
        }
        other => {
            let kind = literal_kind(other).err().unwrap_or("non-literal");
            return Err(AsoError::NonLiteralConst(kind));
        }
    }
    Ok(())
}

fn read_value(r: &mut Reader) -> Result<Value, AsoError> {
    let tag = r.u8()?;
    let v = match tag {
        TAG_NIL => Value::Nil,
        TAG_BOOL => Value::Bool(r.u8()? != 0),
        TAG_INT => Value::Int(r.u64()? as i64),
        TAG_NUMBER => Value::Float(r.f64()?),
        TAG_STR => Value::Str(Rc::from(r.str()?.as_str())),
        TAG_DECIMAL => {
            let b = r.take(16)?;
            let mut arr = [0u8; 16];
            arr.copy_from_slice(b);
            Value::Decimal(rust_decimal::Decimal::deserialize(arr))
        }
        TAG_ENUM => {
            let name = r.str()?;
            let n = r.len()?;
            let mut variants = indexmap::IndexMap::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let key = r.str()?;
                let enum_name = r.str()?;
                let vname = r.str()?;
                let backing = read_value(r)?;
                variants.insert(
                    key,
                    Value::EnumVariant(Rc::new(crate::value::EnumVariant {
                        enum_name,
                        name: vname,
                        value: backing,
                    })),
                );
            }
            Value::Enum(Rc::new(crate::value::EnumDef { name, variants }))
        }
        TAG_ARRAY => {
            let n = r.len()?;
            let mut elems = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                elems.push(read_value(r)?);
            }
            Value::Array(crate::value::ArrayCell::new(elems))
        }
        other => return Err(AsoError::BadConst(other)),
    };
    Ok(v)
}

// ---- FnProto -----------------------------------------------------------------

fn write_proto(w: &mut Writer, p: &FnProto) -> Result<(), AsoError> {
    w.u8(p.arity);
    let flags = u8::from(p.has_rest)
        | (u8::from(p.is_async) << 1)
        | (u8::from(p.is_generator) << 2)
        | (u8::from(p.is_worker) << 3)
        | (u8::from(p.owning_class.is_some()) << 4);
    w.u8(flags);
    // params
    w.len(p.params.len());
    for param in &p.params {
        write_param(w, param);
    }
    // ret
    write_opt_type(w, p.ret.as_ref());
    // owning_class (v17+): present only when bit4 is set
    if let Some(cls) = &p.owning_class {
        w.str(cls);
    }
    // recursive chunk
    write_chunk(w, &p.chunk)
}

fn read_proto(r: &mut Reader) -> Result<FnProto, AsoError> {
    let arity = r.u8()?;
    let flags = r.u8()?;
    let has_rest = flags & 1 != 0;
    let is_async = flags & 2 != 0;
    let is_generator = flags & 4 != 0;
    let is_worker = flags & 8 != 0;
    let has_owning_class = flags & 16 != 0;
    let n = r.len()?;
    let mut params = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        params.push(read_param(r)?);
    }
    let ret = read_opt_type(r)?;
    // owning_class (v17+): present when bit4 is set
    let owning_class = if has_owning_class {
        Some(Rc::from(r.str()?.as_str()))
    } else {
        None
    };
    let chunk = read_chunk(r)?;
    Ok(FnProto {
        chunk,
        arity,
        has_rest,
        is_async,
        is_generator,
        is_worker,
        owning_class,
        params,
        ret,
    })
}

// ---- Param -------------------------------------------------------------------

fn write_param(w: &mut Writer, p: &Param) {
    w.str(&p.name);
    write_opt_type(w, p.ty.as_ref());
    w.usize(p.name_span.start);
    w.usize(p.name_span.end);
    w.u8(u8::from(p.rest));
    // Only the PRESENCE of a default matters here: the VM evaluates defaults via
    // the function body's compiled prologue (already serialized in the chunk
    // code), and `check_call_args` reads only `default.is_some()` to compute
    // min-arity. So serialize a single presence flag; reconstruct a placeholder
    // `Expr` on read (its content is never inspected by the VM).
    w.u8(u8::from(p.default.is_some()));
}

fn read_param(r: &mut Reader) -> Result<Param, AsoError> {
    let name = r.str()?;
    let ty = read_opt_type(r)?;
    let start = r.usize()?;
    let end = r.usize()?;
    let rest = r.u8()? != 0;
    let has_default = r.u8()? != 0;
    let default = has_default.then(|| crate::ast::Expr {
        kind: crate::ast::ExprKind::Nil,
        span: Span::new(start, end),
    });
    Ok(Param {
        name,
        ty,
        name_span: Span::new(start, end),
        rest,
        default,
    })
}

// ---- Type --------------------------------------------------------------------

fn write_opt_type(w: &mut Writer, t: Option<&Type>) {
    match t {
        Some(t) => {
            w.u8(1);
            write_type(w, t);
        }
        None => w.u8(0),
    }
}

fn read_opt_type(r: &mut Reader) -> Result<Option<Type>, AsoError> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(read_type(r)?)),
        tag => Err(AsoError::BadTag {
            what: "opt-type",
            tag,
        }),
    }
}

fn write_type(w: &mut Writer, t: &Type) {
    match t {
        Type::Number => w.u8(TY_NUMBER),
        Type::String => w.u8(TY_STRING),
        Type::Bool => w.u8(TY_BOOL),
        Type::Nil => w.u8(TY_NIL),
        Type::Any => w.u8(TY_ANY),
        Type::Fn => w.u8(TY_FN),
        Type::Object => w.u8(TY_OBJECT),
        Type::Error => w.u8(TY_ERROR),
        Type::Array(inner) => {
            w.u8(TY_ARRAY);
            write_type(w, inner);
        }
        Type::Result(inner) => {
            w.u8(TY_RESULT);
            write_type(w, inner);
        }
        Type::Tuple(ts) => {
            w.u8(TY_TUPLE);
            w.len(ts.len());
            for t in ts {
                write_type(w, t);
            }
        }
        Type::Union(a, b) => {
            w.u8(TY_UNION);
            write_type(w, a);
            write_type(w, b);
        }
        Type::Named(n) => {
            w.u8(TY_NAMED);
            w.str(n);
        }
        Type::Map(k, v) => {
            w.u8(TY_MAP);
            write_type(w, k);
            write_type(w, v);
        }
        Type::Future(inner) => {
            w.u8(TY_FUTURE);
            write_type(w, inner);
        }
        Type::Optional(inner) => {
            w.u8(TY_OPTIONAL);
            write_type(w, inner);
        }
    }
}

fn read_type(r: &mut Reader) -> Result<Type, AsoError> {
    let tag = r.u8()?;
    let t = match tag {
        TY_NUMBER => Type::Number,
        TY_STRING => Type::String,
        TY_BOOL => Type::Bool,
        TY_NIL => Type::Nil,
        TY_ANY => Type::Any,
        TY_FN => Type::Fn,
        TY_OBJECT => Type::Object,
        TY_ERROR => Type::Error,
        TY_ARRAY => Type::Array(Box::new(read_type(r)?)),
        TY_RESULT => Type::Result(Box::new(read_type(r)?)),
        TY_TUPLE => {
            let n = r.len()?;
            let mut ts = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                ts.push(read_type(r)?);
            }
            Type::Tuple(ts)
        }
        TY_UNION => {
            let a = read_type(r)?;
            let b = read_type(r)?;
            Type::Union(Box::new(a), Box::new(b))
        }
        TY_NAMED => Type::Named(r.str()?),
        TY_MAP => {
            let k = read_type(r)?;
            let v = read_type(r)?;
            Type::Map(Box::new(k), Box::new(v))
        }
        TY_FUTURE => Type::Future(Box::new(read_type(r)?)),
        TY_OPTIONAL => Type::Optional(Box::new(read_type(r)?)),
        tag => return Err(AsoError::BadTag { what: "type", tag }),
    };
    Ok(t)
}

// ---- field-default Expr (the `cst_default_expr` subset) -----------------------

fn write_expr(w: &mut Writer, e: &Expr) -> Result<(), AsoError> {
    w.usize(e.span.start);
    w.usize(e.span.end);
    match &e.kind {
        ExprKind::Int(i) => {
            w.u8(EX_INT);
            w.u64(*i as u64);
        }
        ExprKind::Float(n) => {
            w.u8(EX_NUMBER);
            w.f64(*n);
        }
        ExprKind::Str(s) => {
            w.u8(EX_STR);
            w.str(s);
        }
        ExprKind::Bool(b) => {
            w.u8(EX_BOOL);
            w.u8(u8::from(*b));
        }
        ExprKind::Nil => w.u8(EX_NIL),
        ExprKind::Ident(name) => {
            w.u8(EX_IDENT);
            w.str(name);
        }
        ExprKind::Paren(inner) => {
            w.u8(EX_PAREN);
            write_expr(w, inner)?;
        }
        ExprKind::Unary { op, expr } => {
            w.u8(EX_UNARY);
            w.u8(match op {
                UnOp::Neg => 0,
                UnOp::Not => 1,
            });
            write_expr(w, expr)?;
        }
        ExprKind::Array(elems) => {
            w.u8(EX_ARRAY);
            w.len(elems.len());
            for el in elems {
                match el {
                    ArrayElem::Item(e) => {
                        w.u8(EL_ITEM);
                        write_expr(w, e)?;
                    }
                    ArrayElem::Spread(e) => {
                        w.u8(EL_SPREAD);
                        write_expr(w, e)?;
                    }
                }
            }
        }
        ExprKind::Object(entries) => {
            w.u8(EX_OBJECT);
            w.len(entries.len());
            for ent in entries {
                match ent {
                    ObjEntry::KV(k, v) => {
                        w.u8(EL_ITEM);
                        w.str(k);
                        write_expr(w, v)?;
                    }
                    ObjEntry::Spread(e) => {
                        w.u8(EL_SPREAD);
                        write_expr(w, e)?;
                    }
                }
            }
        }
        ExprKind::Map(entries) => {
            w.u8(EX_MAP);
            w.len(entries.len());
            for ent in entries {
                write_expr(w, &ent.key)?;
                write_expr(w, &ent.value)?;
            }
        }
        ExprKind::Member { object, name } => {
            w.u8(EX_MEMBER);
            w.str(name);
            write_expr(w, object)?;
        }
        ExprKind::OptMember { object, name } => {
            w.u8(EX_OPTMEMBER);
            w.str(name);
            write_expr(w, object)?;
        }
        ExprKind::Index { object, index } => {
            w.u8(EX_INDEX);
            write_expr(w, object)?;
            write_expr(w, index)?;
        }
        ExprKind::Call { callee, args } => {
            w.u8(EX_CALL);
            write_expr(w, callee)?;
            w.len(args.len());
            for a in args {
                match a {
                    CallArg::Pos(e) => {
                        w.u8(EL_ITEM);
                        write_expr(w, e)?;
                    }
                    CallArg::Spread(e) => {
                        w.u8(EL_SPREAD);
                        write_expr(w, e)?;
                    }
                }
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            w.u8(EX_BINARY);
            w.u8(binop_tag(*op));
            write_expr(w, lhs)?;
            write_expr(w, rhs)?;
        }
        ExprKind::Ternary { cond, then, els } => {
            w.u8(EX_TERNARY);
            write_expr(w, cond)?;
            write_expr(w, then)?;
            write_expr(w, els)?;
        }
        ExprKind::Template { parts } => {
            w.u8(EX_TEMPLATE);
            w.len(parts.len());
            for part in parts {
                match part {
                    TemplatePart::Lit(s) => {
                        w.u8(TP_LIT);
                        w.str(s);
                    }
                    TemplatePart::Expr(e) => {
                        w.u8(TP_EXPR);
                        write_expr(w, e)?;
                    }
                }
            }
        }
        ExprKind::Try(e) => {
            w.u8(EX_TRY);
            write_expr(w, e)?;
        }
        ExprKind::Unwrap(e) => {
            w.u8(EX_UNWRAP);
            write_expr(w, e)?;
        }
        ExprKind::Await(e) => {
            w.u8(EX_AWAIT);
            write_expr(w, e)?;
        }
        ExprKind::Assign { target, value } => {
            w.u8(EX_ASSIGN);
            write_expr(w, target)?;
            write_expr(w, value)?;
        }
        // `Arrow` and `Match` defaults embed statement/pattern subtrees this flat
        // serializer does not encode structurally; they are serialized by
        // `write_field_default` as an `EX_REPARSE` source-text payload (SP1 §4) and
        // so never reach `write_expr`. Reaching here is a compiler invariant
        // violation (a default_sources/field mismatch). `Yield` never reaches here
        // (`cst_default_expr` rejects a `yield` default outright).
        ExprKind::Arrow { .. } => return Err(AsoError::NonLiteralConst("arrow-default")),
        ExprKind::Match { .. } => return Err(AsoError::NonLiteralConst("match-default")),
        ExprKind::Yield(_) => return Err(AsoError::NonLiteralConst("yield-default")),
        // A value-position range `a..b` / `a..=b` field default. `step` defaults are
        // rejected upstream (`cst_default_expr`), so only the inclusive flag varies.
        ExprKind::Range {
            start,
            end,
            inclusive,
            step: _,
        } => {
            w.u8(EX_RANGE);
            w.u8(u8::from(*inclusive));
            write_expr(w, start)?;
            write_expr(w, end)?;
        }
    }
    Ok(())
}

/// The wire tag for a [`BinOp`] (mirrored by `binop_from_tag` on read).
fn binop_tag(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::Pow => 5,
        BinOp::Lt => 6,
        BinOp::Le => 7,
        BinOp::Gt => 8,
        BinOp::Ge => 9,
        BinOp::Eq => 10,
        BinOp::Ne => 11,
        BinOp::And => 12,
        BinOp::Or => 13,
        BinOp::Coalesce => 14,
        BinOp::Range => 15,
        BinOp::InstanceOf => 16,
    }
}

/// The [`BinOp`] for a wire tag (inverse of `binop_tag`).
fn binop_from_tag(tag: u8) -> Result<BinOp, AsoError> {
    Ok(match tag {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::Mod,
        5 => BinOp::Pow,
        6 => BinOp::Lt,
        7 => BinOp::Le,
        8 => BinOp::Gt,
        9 => BinOp::Ge,
        10 => BinOp::Eq,
        11 => BinOp::Ne,
        12 => BinOp::And,
        13 => BinOp::Or,
        14 => BinOp::Coalesce,
        15 => BinOp::Range,
        16 => BinOp::InstanceOf,
        tag => return Err(AsoError::BadTag { what: "binop", tag }),
    })
}

fn read_expr(r: &mut Reader) -> Result<Expr, AsoError> {
    let start = r.usize()?;
    let end = r.usize()?;
    let span = Span::new(start, end);
    let tag = r.u8()?;
    let kind = read_expr_kind(r, tag)?;
    Ok(Expr { kind, span })
}

/// Decode an [`ExprKind`] given its already-read tag byte (the span header is read
/// by the caller). Split out so `read_field_default` can peek the tag to capture an
/// `EX_REPARSE` source before delegating here for the non-reparse forms.
fn read_expr_kind(r: &mut Reader, tag: u8) -> Result<ExprKind, AsoError> {
    let kind = match tag {
        EX_INT => ExprKind::Int(r.u64()? as i64),
        EX_NUMBER => ExprKind::Float(r.f64()?),
        EX_STR => ExprKind::Str(r.str()?),
        EX_BOOL => ExprKind::Bool(r.u8()? != 0),
        EX_NIL => ExprKind::Nil,
        EX_IDENT => ExprKind::Ident(r.str()?),
        EX_PAREN => ExprKind::Paren(Box::new(read_expr(r)?)),
        EX_UNARY => {
            let op = match r.u8()? {
                0 => UnOp::Neg,
                1 => UnOp::Not,
                tag => return Err(AsoError::BadTag { what: "unop", tag }),
            };
            ExprKind::Unary {
                op,
                expr: Box::new(read_expr(r)?),
            }
        }
        EX_ARRAY => {
            let n = r.len()?;
            let mut elems = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let el = match r.u8()? {
                    EL_ITEM => ArrayElem::Item(read_expr(r)?),
                    EL_SPREAD => ArrayElem::Spread(read_expr(r)?),
                    tag => {
                        return Err(AsoError::BadTag {
                            what: "array-elem",
                            tag,
                        })
                    }
                };
                elems.push(el);
            }
            ExprKind::Array(elems)
        }
        EX_OBJECT => {
            let n = r.len()?;
            let mut entries = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let ent = match r.u8()? {
                    EL_ITEM => {
                        let k = r.str()?;
                        ObjEntry::KV(k, read_expr(r)?)
                    }
                    EL_SPREAD => ObjEntry::Spread(read_expr(r)?),
                    tag => {
                        return Err(AsoError::BadTag {
                            what: "object-entry",
                            tag,
                        })
                    }
                };
                entries.push(ent);
            }
            ExprKind::Object(entries)
        }
        EX_MAP => {
            let n = r.len()?;
            let mut entries = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let key = read_expr(r)?;
                let value = read_expr(r)?;
                entries.push(crate::ast::MapEntry { key, value });
            }
            ExprKind::Map(entries)
        }
        EX_MEMBER => {
            let name = r.str()?;
            let object = Box::new(read_expr(r)?);
            ExprKind::Member { object, name }
        }
        EX_OPTMEMBER => {
            let name = r.str()?;
            let object = Box::new(read_expr(r)?);
            ExprKind::OptMember { object, name }
        }
        EX_INDEX => {
            let object = Box::new(read_expr(r)?);
            let index = Box::new(read_expr(r)?);
            ExprKind::Index { object, index }
        }
        EX_CALL => {
            let callee = Box::new(read_expr(r)?);
            let n = r.len()?;
            let mut args = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let a = match r.u8()? {
                    EL_ITEM => CallArg::Pos(read_expr(r)?),
                    EL_SPREAD => CallArg::Spread(read_expr(r)?),
                    tag => {
                        return Err(AsoError::BadTag {
                            what: "call-arg",
                            tag,
                        })
                    }
                };
                args.push(a);
            }
            ExprKind::Call { callee, args }
        }
        EX_BINARY => {
            let op = binop_from_tag(r.u8()?)?;
            let lhs = Box::new(read_expr(r)?);
            let rhs = Box::new(read_expr(r)?);
            ExprKind::Binary { op, lhs, rhs }
        }
        EX_TERNARY => {
            let cond = Box::new(read_expr(r)?);
            let then = Box::new(read_expr(r)?);
            let els = Box::new(read_expr(r)?);
            ExprKind::Ternary { cond, then, els }
        }
        EX_TEMPLATE => {
            let n = r.len()?;
            let mut parts = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let part = match r.u8()? {
                    TP_LIT => TemplatePart::Lit(r.str()?),
                    TP_EXPR => TemplatePart::Expr(Box::new(read_expr(r)?)),
                    tag => {
                        return Err(AsoError::BadTag {
                            what: "template-part",
                            tag,
                        })
                    }
                };
                parts.push(part);
            }
            ExprKind::Template { parts }
        }
        EX_TRY => ExprKind::Try(Box::new(read_expr(r)?)),
        EX_UNWRAP => ExprKind::Unwrap(Box::new(read_expr(r)?)),
        EX_AWAIT => ExprKind::Await(Box::new(read_expr(r)?)),
        EX_ASSIGN => {
            let target = Box::new(read_expr(r)?);
            let value = Box::new(read_expr(r)?);
            ExprKind::Assign { target, value }
        }
        EX_RANGE => {
            let inclusive = r.u8()? != 0;
            let start = Box::new(read_expr(r)?);
            let end = Box::new(read_expr(r)?);
            ExprKind::Range {
                start,
                end,
                inclusive,
                step: None,
            }
        }
        // `EX_REPARSE` (arrow/`match` field default, SP1 §4) only ever appears at the
        // TOP of a field-default expr stream, where `read_field_default` peeks and
        // handles it (capturing the source for re-serialization) BEFORE delegating
        // here. It is never nested inside another expr (`cst_default_expr` re-parses
        // the whole arrow/match node as one unit), so reaching it here is a corrupt
        // stream.
        EX_REPARSE => return Err(AsoError::BadTag { what: "expr", tag }),
        tag => return Err(AsoError::BadTag { what: "expr", tag }),
    };
    Ok(kind)
}

// ---- ClassProto / Class / FieldSchema ----------------------------------------

fn write_class_proto(w: &mut Writer, cp: &ClassProto) -> Result<(), AsoError> {
    // Build the field→source lookup for arrow/`match` defaults (SP1 §4). The
    // `write_class` default writer consults it to emit an `EX_REPARSE` tag carrying
    // the source inline instead of structurally serializing the (unrepresentable)
    // statement/pattern subtree.
    let sources: std::collections::HashMap<&str, &str> = cp
        .default_sources
        .iter()
        .map(|(f, s)| (f.as_str(), s.as_str()))
        .collect();
    write_class(w, &cp.class, &sources)?;
    w.len(cp.default_fields.len());
    for f in &cp.default_fields {
        w.str(f);
    }
    w.len(cp.method_names.len());
    for m in &cp.method_names {
        w.str(m);
    }
    // Static method names (SP1 §3, format v10): a separate namespace from
    // `method_names`; the static closures are pushed after the instance methods.
    w.len(cp.static_method_names.len());
    for m in &cp.static_method_names {
        w.str(m);
    }
    // Per-default capture plan (parallel to default_fields): the free names each
    // default expression captures + their upvalue index in that field's thunk.
    w.len(cp.default_captures.len());
    for caps in &cp.default_captures {
        w.len(caps.len());
        for (name, idx) in caps {
            w.str(name);
            w.u16(*idx);
        }
    }
    w.u8(u8::from(cp.has_super));
    Ok(())
}

fn read_class_proto(r: &mut Reader) -> Result<ClassProto, AsoError> {
    let (class, default_sources) = read_class(r)?;
    let n = r.len()?;
    let mut default_fields = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        default_fields.push(r.str()?);
    }
    let n = r.len()?;
    let mut method_names = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        method_names.push(r.str()?);
    }
    let n = r.len()?;
    let mut static_method_names = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        static_method_names.push(r.str()?);
    }
    let n = r.len()?;
    let mut default_captures = Vec::with_capacity(n.min(r.remaining()));
    for _ in 0..n {
        let m = r.len()?;
        let mut caps = Vec::with_capacity(m.min(r.remaining()));
        for _ in 0..m {
            let name = r.str()?;
            let idx = r.u16()?;
            caps.push((name, idx));
        }
        default_captures.push(caps);
    }
    let has_super = r.u8()? != 0;
    Ok(ClassProto {
        class: Rc::new(class),
        default_fields,
        // Recovered from the `EX_REPARSE` field-default payloads by `read_class`, so a
        // loaded chunk re-serializes byte-identically (round-trip idempotence).
        default_sources,
        method_names,
        static_method_names,
        default_captures,
        has_super,
    })
}

fn write_class(
    w: &mut Writer,
    c: &Class,
    sources: &std::collections::HashMap<&str, &str>,
) -> Result<(), AsoError> {
    // The compiler builds the ClassProto's class with `superclass: None`,
    // `methods` empty, and `def_env = global_env()` placeholder. Serialize only
    // the name + field schemas; the rest is rebuilt as the same inert template.
    w.str(&c.name);
    // Workers Spec B: the `worker class` flag (v18). One byte before the field list.
    w.u8(u8::from(c.is_worker));
    w.len(c.fields.len());
    for (fname, schema) in &c.fields {
        w.str(fname);
        write_type(w, &schema.ty);
        match &schema.default {
            Some(e) => {
                w.u8(1);
                write_field_default(w, fname, e, sources)?;
            }
            None => w.u8(0),
        }
    }
    Ok(())
}

/// Serialize a field's default expression. For an arrow/`match` default (the forms
/// `cst_default_expr` lowers by re-parsing source — they appear in `sources`) emit
/// an `EX_REPARSE` tag carrying the source text; everything else delegates to the
/// structural `write_expr`. The leading span (start/end) is written first either
/// way, matching `read_expr`'s uniform header.
fn write_field_default(
    w: &mut Writer,
    fname: &str,
    e: &Expr,
    sources: &std::collections::HashMap<&str, &str>,
) -> Result<(), AsoError> {
    if let Some(src) = sources.get(fname) {
        debug_assert!(
            matches!(e.kind, ExprKind::Arrow { .. } | ExprKind::Match { .. }),
            "default_sources entry for a non-arrow/match default"
        );
        w.usize(e.span.start);
        w.usize(e.span.end);
        w.u8(EX_REPARSE);
        w.str(src);
        Ok(())
    } else {
        write_expr(w, e)
    }
}

/// Read a class template plus the arrow/`match` default source-texts recovered from
/// the field-default stream (`EX_REPARSE`). The sources let a loaded chunk be
/// RE-serialized byte-identically (the `default_sources` lookup `write_field_default`
/// consults) — preserving `.aso` round-trip idempotence for these defaults.
fn read_class(r: &mut Reader) -> Result<(Class, Vec<(String, String)>), AsoError> {
    let name = r.str()?;
    let is_worker = r.u8()? != 0;
    let n = r.len()?;
    let mut fields = indexmap::IndexMap::with_capacity(n.min(r.remaining()));
    let mut sources: Vec<(String, String)> = Vec::new();
    for _ in 0..n {
        let fname = r.str()?;
        let ty = read_type(r)?;
        let default = match r.u8()? {
            0 => None,
            1 => {
                let (expr, src) = read_field_default(r)?;
                if let Some(src) = src {
                    sources.push((fname.clone(), src));
                }
                Some(expr)
            }
            tag => {
                return Err(AsoError::BadTag {
                    what: "field-default",
                    tag,
                })
            }
        };
        fields.insert(fname, FieldSchema { ty, default });
    }
    let class = Class {
        name,
        superclass: None,
        fields,
        methods: indexmap::IndexMap::new(),
        // Static methods (SP1 §3) round-trip via the static proto table, not this
        // class template; see C6.
        static_methods: indexmap::IndexMap::new(),
        def_env: crate::interp::global_env(),
        is_worker,
    };
    Ok((class, sources))
}

/// Read one field default. Returns the lowered [`Expr`] and, for an `EX_REPARSE`
/// (arrow/`match`) default, its original source text (so it can be re-serialized).
/// Mirrors `read_expr`'s uniform `(span, tag, ..)` framing but peeks the tag so the
/// `EX_REPARSE` source can be captured before re-lowering.
fn read_field_default(r: &mut Reader) -> Result<(Expr, Option<String>), AsoError> {
    let start = r.usize()?;
    let end = r.usize()?;
    if r.peek_u8()? == EX_REPARSE {
        let _ = r.u8()?; // consume the peeked tag
        let src = r.str()?;
        let expr = crate::compile::reparse_default_from_source(&src).map_err(|_| AsoError::BadTag {
            what: "reparse-default",
            tag: EX_REPARSE,
        })?;
        return Ok((expr, Some(src)));
    }
    // Not a reparse default: rewind to the span and read normally. The span bytes
    // were already consumed, so feed them back by reading the rest via `read_expr`'s
    // body. Reconstruct the full expr by reusing the already-read span.
    let tag = r.u8()?;
    let kind = read_expr_kind(r, tag)?;
    Ok((Expr { kind, span: Span::new(start, end) }, None))
}

// ---- ImportDesc --------------------------------------------------------------

fn write_import(w: &mut Writer, imp: &ImportDesc) {
    match imp {
        ImportDesc::Named { source, names } => {
            w.u8(IMP_NAMED);
            w.str(source);
            w.len(names.len());
            for (name, slot, is_cell, is_global) in names {
                w.str(name);
                w.u16(*slot);
                w.u8(u8::from(*is_cell));
                w.u8(u8::from(*is_global));
            }
        }
        ImportDesc::Namespace {
            source,
            alias,
            slot,
            is_cell,
            is_global,
        } => {
            w.u8(IMP_NAMESPACE);
            w.str(source);
            w.str(alias);
            w.u16(*slot);
            w.u8(u8::from(*is_cell));
            w.u8(u8::from(*is_global));
        }
    }
}

fn read_import(r: &mut Reader) -> Result<ImportDesc, AsoError> {
    match r.u8()? {
        IMP_NAMED => {
            let source = r.str()?;
            let n = r.len()?;
            let mut names = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let name = r.str()?;
                let slot = r.u16()?;
                let is_cell = r.u8()? != 0;
                let is_global = r.u8()? != 0;
                names.push((name, slot, is_cell, is_global));
            }
            Ok(ImportDesc::Named { source, names })
        }
        IMP_NAMESPACE => {
            let source = r.str()?;
            let alias = r.str()?;
            let slot = r.u16()?;
            let is_cell = r.u8()? != 0;
            let is_global = r.u8()? != 0;
            Ok(ImportDesc::Namespace {
                source,
                alias,
                slot,
                is_cell,
                is_global,
            })
        }
        tag => Err(AsoError::BadTag {
            what: "import",
            tag,
        }),
    }
}

// ---- UpvalueDescriptor -------------------------------------------------------

fn write_upvalue(w: &mut Writer, uv: &UpvalueDescriptor) {
    match uv {
        // SP8 #136: ParentLocal carries the `by_value` bit (u8: 1 = by value, copied
        // into a fresh private cell at Op::Closure; 0 = by reference, shared cell).
        UpvalueDescriptor::ParentLocal { slot, by_value } => {
            w.u8(UV_PARENT_LOCAL);
            w.u32(*slot);
            w.u8(u8::from(*by_value));
        }
        UpvalueDescriptor::ParentUpvalue(i) => {
            w.u8(UV_PARENT_UPVALUE);
            w.u32(*i);
        }
    }
}

fn read_upvalue(r: &mut Reader) -> Result<UpvalueDescriptor, AsoError> {
    match r.u8()? {
        UV_PARENT_LOCAL => {
            let slot = r.u32()?;
            let by_value = r.u8()? != 0;
            Ok(UpvalueDescriptor::ParentLocal { slot, by_value })
        }
        UV_PARENT_UPVALUE => Ok(UpvalueDescriptor::ParentUpvalue(r.u32()?)),
        tag => Err(AsoError::BadTag {
            what: "upvalue",
            tag,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::disasm::disasm;

    /// P0 (security): a crafted `.aso` that declares a gigantic element count over a
    /// short buffer must return a clean `Err`, NOT pre-allocate gigabytes and abort the
    /// process. Pre-fix, `read_chunk`'s `c.consts.reserve(u32::MAX)` allocated ~137 GB
    /// (≈4.3e9 × size_of::<Value>) → the allocator returns null → Rust aborts. The
    /// `remaining()` clamp bounds the pre-allocation; the per-element loop then reports
    /// the short read as `Truncated`. Removing the clamp re-aborts this test — that is
    /// the regression guard.
    #[test]
    fn reader_clamps_bomb_length_no_abort() {
        // code length = 0 (u32 LE), then a bomb const-pool count = u32::MAX (u32 LE).
        let buf = [0u8, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
        let mut r = Reader::new(&buf);
        assert!(
            matches!(read_chunk(&mut r), Err(AsoError::Truncated)),
            "a bomb length must decode to a clean Truncated error, never an abort"
        );
    }

    /// The clamp must not change behavior for a well-formed stream: a genuine large
    /// (but in-bounds) count still decodes fully. Guards against the clamp truncating
    /// valid data.
    #[test]
    fn reader_clamp_preserves_valid_decode() {
        // A real round-trip through a chunk with a non-trivial const pool proves the
        // clamp is a no-op when remaining() >= n.
        let original = compile("let xs = [1, 2, 3, 4, 5]; print(xs)");
        let bytes = original.to_bytes().expect("serialize");
        let back = Chunk::from_bytes(&bytes).expect("a valid .aso must still decode");
        assert_eq!(disasm(&original), disasm(&back), "valid decode unchanged by clamp");
    }

    /// Compile `src` to a top-level chunk via the real compiler.
    fn compile(src: &str) -> Chunk {
        crate::compile::compile_source(src)
            .unwrap_or_else(|e| panic!("compile failed: {} @ {:?}", e.message, e.span))
    }

    /// Run a chunk to completion, returning the captured stdout. Mirrors
    /// `vm_run_source_with` (specialize = true) so the result is the program's
    /// real output.
    fn run_chunk(chunk: Chunk) -> String {
        use crate::interp::Interp;
        use crate::vm::value_ext::{Closure, RunOutcome};
        use crate::vm::Vm;
        use std::rc::Rc;

        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
        });
        let closure = Closure::new(proto);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::with_specialize(interp.clone(), true);
        let mut fiber = crate::vm::fiber::Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        let result = local.block_on(&rt, vm.run(&mut fiber));
        match result {
            Ok(RunOutcome::Done(_)) => interp.output(),
            Ok(RunOutcome::Yielded(_)) => panic!("top-level cannot yield"),
            Err(crate::interp::Control::Propagate(_)) => interp.output(),
            Err(crate::interp::Control::Exit(_)) => interp.output(),
            Err(crate::interp::Control::Panic(e)) => panic!("vm panic: {e}"),
        }
    }

    /// A meaty program exercising literals, nested fns, control flow, classes
    /// (defaults + methods), enums, and imports — every serialized side table.
    const COMPLEX: &str = r#"
import { max } from "std/math"

enum Status { Ok = 200, NotFound = 404 }

class Point {
    x: number = 0
    y: number = 0
    label: string = "origin"
    fn dist(): number {
        return self.x * self.x + self.y * self.y
    }
}

fn make(n: number): fn {
    let scale = n
    return (v) => v * scale
}

fn run() {
    let dbl = make(2)
    let nums = [1, 2, 3]
    let total = 0
    for (x of nums) {
        if (x % 2 == 0) {
            total = total + dbl(x)
        } else {
            total = total + x
        }
    }
    let p = Point()
    p.x = 3
    p.y = 4
    print(total)
    print(p.dist())
    print(Status.Ok.value)
    print(max(7, 3))
    print(p.label)
}

run()
"#;

    #[test]
    fn header_layout() {
        let chunk = compile("print(1 + 2)");
        let bytes = chunk.to_bytes().expect("serialize");
        assert_eq!(&bytes[0..4], &ASO_MAGIC);
        let ver = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(ver, ASO_FORMAT_VERSION);
    }

    #[test]
    fn roundtrip_structural_equality_simple() {
        let original = compile("let a = 1\nlet b = \"hi\"\nprint(a)\nprint(b)");
        let bytes = original.to_bytes().expect("serialize");
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(disasm(&original), disasm(&rt), "disasm fingerprint differs");
    }

    #[test]
    fn roundtrip_structural_equality_complex() {
        let original = compile(COMPLEX);
        let bytes = original.to_bytes().expect("serialize");
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(disasm(&original), disasm(&rt), "disasm fingerprint differs");
    }

    #[test]
    fn roundtrip_produces_same_output() {
        // compile→run  vs  compile→to_bytes→from_bytes→run must be byte-identical.
        let direct = run_chunk(compile(COMPLEX));
        let viaso = run_chunk(Chunk::from_bytes(&compile(COMPLEX).to_bytes().expect("serialize")).expect("decode"));
        assert_eq!(direct, viaso, "output differs after .aso round-trip");
    }

    #[test]
    fn roundtrip_capture_by_value_upvalue() {
        // SP8 #136: a closure capturing a never-reassigned local (by VALUE) and one
        // capturing a reassigned local (by REFERENCE) both serialize their
        // `UpvalueDescriptor::ParentLocal { by_value }` bit and round-trip to the same
        // disasm fingerprint AND the same output. Guards the v15 descriptor layout.
        let src = "fn make() {\n let k = 10\n let n = 0\n return () => {\n n = n + 1\n \
                   return k + n\n }\n}\nlet c = make()\nprint(c())\nprint(c())\n";
        let original = compile(src);
        let bytes = original.to_bytes().expect("serialize");
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(disasm(&original), disasm(&rt), "disasm fingerprint differs");
        assert_eq!(
            run_chunk(original),
            run_chunk(rt),
            "output differs after capture-by-value .aso round-trip"
        );
    }

    #[test]
    fn proto_is_worker_survives_aso_roundtrip() {
        // Guards v16: FnProto flags bit3 = is_worker must survive write_proto →
        // read_proto. Uses the module-private Writer/Reader directly to test the
        // codec in isolation (independent of the full Chunk round-trip).
        let proto = FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: true,
            owning_class: None,
            params: Vec::new(),
            ret: None,
        };
        let mut w = Writer::new();
        write_proto(&mut w, &proto).unwrap();
        let mut r = Reader::new(&w.buf);
        let back = read_proto(&mut r).unwrap();
        assert!(back.is_worker, "is_worker must survive the .aso round-trip");
        // Also confirm the false case is still preserved.
        let proto_false = FnProto {
            is_worker: false,
            owning_class: None,
            ..FnProto {
                chunk: Chunk::new(),
                arity: 0,
                has_rest: false,
                is_async: false,
                is_generator: false,
                is_worker: false,
                owning_class: None,
                params: Vec::new(),
                ret: None,
            }
        };
        let mut w2 = Writer::new();
        write_proto(&mut w2, &proto_false).unwrap();
        let mut r2 = Reader::new(&w2.buf);
        let back_false = read_proto(&mut r2).unwrap();
        assert!(!back_false.is_worker, "is_worker=false must also be preserved");
    }

    #[test]
    fn proto_owning_class_survives_aso_roundtrip() {
        // Guards v17: FnProto flags bit4 = owning_class_present + trailing string
        // must survive write_proto → read_proto (used by static worker fn dispatch).
        let proto_with = FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: true,
            owning_class: Some(Rc::from("Img")),
            params: Vec::new(),
            ret: None,
        };
        let mut w = Writer::new();
        write_proto(&mut w, &proto_with).unwrap();
        let mut r = Reader::new(&w.buf);
        let back = read_proto(&mut r).unwrap();
        assert_eq!(back.owning_class.as_deref(), Some("Img"),
                   "owning_class must survive the .aso round-trip");

        // None case: no extra bytes written.
        let proto_none = FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: true,
            owning_class: None,
            params: Vec::new(),
            ret: None,
        };
        let mut w2 = Writer::new();
        write_proto(&mut w2, &proto_none).unwrap();
        let mut r2 = Reader::new(&w2.buf);
        let back_none = read_proto(&mut r2).unwrap();
        assert!(back_none.owning_class.is_none(),
                "owning_class=None must also be preserved");
    }

    #[test]
    fn class_is_worker_survives_aso_roundtrip() {
        // Guards v18: the runtime Class `is_worker` flag (a `worker class`) must
        // survive write_class → read_class. This is what lets a compiled `.aso`
        // recover the actor-class shape for `.aso`-mode actor spawn.
        let sources: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();

        let mk = |is_worker: bool| crate::value::Class {
            name: "C".to_string(),
            superclass: None,
            fields: indexmap::IndexMap::new(),
            methods: indexmap::IndexMap::new(),
            static_methods: indexmap::IndexMap::new(),
            def_env: crate::interp::global_env(),
            is_worker,
        };

        // true case round-trips as true.
        let mut w = Writer::new();
        write_class(&mut w, &mk(true), &sources).unwrap();
        let mut r = Reader::new(&w.buf);
        let (back, _) = read_class(&mut r).unwrap();
        assert!(back.is_worker, "Class.is_worker=true must survive the .aso round-trip");

        // false case round-trips as false.
        let mut w2 = Writer::new();
        write_class(&mut w2, &mk(false), &sources).unwrap();
        let mut r2 = Reader::new(&w2.buf);
        let (back_false, _) = read_class(&mut r2).unwrap();
        assert!(!back_false.is_worker, "Class.is_worker=false must also be preserved");
    }

    #[test]
    fn worker_class_program_roundtrips_is_worker() {
        // End-to-end: a `worker class` compiled to a full chunk must carry
        // is_worker=true through a complete Chunk::to_bytes → from_bytes cycle (the
        // class proto lives in the chunk's class-proto table).
        let chunk = compile("worker class Counter { count: number = 0 }");
        let restored = Chunk::from_bytes(&chunk.to_bytes().expect("serialize")).expect("decode");
        let cp = restored
            .class_protos
            .iter()
            .find(|cp| &*cp.class.name == "Counter")
            .expect("Counter class proto present after round-trip");
        assert!(cp.class.is_worker, "worker class must round-trip is_worker=true");
    }

    #[test]
    fn double_roundtrip_is_stable() {
        let original = compile(COMPLEX);
        let once = Chunk::from_bytes(&original.to_bytes().expect("serialize")).expect("decode 1");
        let twice = Chunk::from_bytes(&once.to_bytes().expect("serialize")).expect("decode 2");
        assert_eq!(disasm(&original), disasm(&twice));
        // Bytes themselves are stable across re-encode.
        assert_eq!(once.to_bytes().expect("serialize"), twice.to_bytes().expect("serialize"));
    }

    #[test]
    fn version_mismatch_detected() {
        let mut bytes = compile("print(1)").to_bytes().expect("serialize");
        // Corrupt the version u32 (bytes 4..8).
        bytes[4] = bytes[4].wrapping_add(1);
        match Chunk::from_bytes(&bytes) {
            Err(AsoError::VersionMismatch { found, expected }) => {
                assert_eq!(expected, ASO_FORMAT_VERSION);
                assert_ne!(found, ASO_FORMAT_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn bad_magic_detected() {
        let mut bytes = compile("print(1)").to_bytes().expect("serialize");
        bytes[0] = b'X';
        assert!(matches!(
            Chunk::from_bytes(&bytes),
            Err(AsoError::BadMagic(_))
        ));
        // Too short for even the magic.
        assert!(matches!(
            Chunk::from_bytes(&[0, 1]),
            Err(AsoError::Truncated)
        ));
    }

    #[test]
    fn truncated_detected() {
        let bytes = compile(COMPLEX).to_bytes().expect("serialize");
        // Drop the tail — header is intact but body is short.
        let half = &bytes[..bytes.len() / 2];
        assert!(matches!(Chunk::from_bytes(half), Err(AsoError::Truncated)));
    }

    #[test]
    fn trailing_bytes_detected() {
        let mut bytes = compile("print(1)").to_bytes().expect("serialize");
        bytes.push(0xAB);
        assert!(matches!(
            Chunk::from_bytes(&bytes),
            Err(AsoError::TrailingBytes)
        ));
    }

    #[test]
    fn const_pool_is_literals_only() {
        // The compiler must only place literal scalars + name strings + enums in
        // the pool; verify the self-check passes on a complex chunk (and recurse
        // into protos).
        fn check(c: &Chunk) {
            c.check_consts_literal_only().expect("pool literals-only");
            for p in &c.protos {
                check(&p.chunk);
            }
        }
        check(&compile(COMPLEX));
    }

    #[test]
    fn nested_protos_imports_classprotos_roundtrip() {
        let original = compile(COMPLEX);
        // Sanity: the program really does exercise each table.
        assert!(!original.imports.is_empty(), "expected imports");
        assert!(!original.class_protos.is_empty(), "expected class_protos");
        assert!(!original.protos.is_empty(), "expected nested protos");

        let rt = Chunk::from_bytes(&original.to_bytes().expect("serialize")).expect("decode");
        assert_eq!(original.imports.len(), rt.imports.len());
        assert_eq!(original.class_protos.len(), rt.class_protos.len());
        assert_eq!(original.protos.len(), rt.protos.len());
        // Field schemas (with defaults) survive: Point has 3 defaulted fields.
        let cp = &rt.class_protos[0];
        assert_eq!(cp.class.name, "Point");
        assert_eq!(cp.class.fields.len(), 3);
        assert_eq!(cp.default_fields.len(), 3);
        assert_eq!(cp.method_names, vec!["dist".to_string()]);
    }

    #[test]
    fn runtime_side_tables_not_serialized_and_rebuilt_empty() {
        let original = compile(COMPLEX);
        // Populate a runtime cache on the ORIGINAL chunk; it must NOT travel.
        original.set_field_ic(0, crate::vm::ic::InlineCache::default());
        let rt = Chunk::from_bytes(&original.to_bytes().expect("serialize")).expect("decode");
        assert!(rt.field_ics.borrow().is_empty());
        assert!(rt.method_ics.borrow().is_empty());
        assert!(rt.arith_caches.borrow().is_empty());
        assert!(rt.global_caches.borrow().is_empty());
    }

    #[test]
    fn decimal_and_special_floats_roundtrip() {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        let mut c = Chunk::new();
        c.add_const(Value::Float(f64::NAN));
        c.add_const(Value::Float(-0.0));
        c.add_const(Value::Float(f64::INFINITY));
        c.add_const(Value::Decimal(Decimal::from_str("1.50").unwrap()));
        let rt = Chunk::from_bytes(&c.to_bytes().expect("serialize")).expect("decode");
        assert!(matches!(rt.consts[0], Value::Float(n) if n.is_nan()));
        assert!(matches!(rt.consts[1], Value::Float(n) if n == 0.0 && n.is_sign_negative()));
        assert!(matches!(rt.consts[2], Value::Float(n) if n.is_infinite()));
        match &rt.consts[3] {
            Value::Decimal(d) => assert_eq!(d.to_string(), "1.50"),
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn int_constants_roundtrip() {
        // NUM §3.3: an `int` const pool entry round-trips exactly through
        // `TAG_INT`, distinct from a same-magnitude `Float`.
        let mut c = Chunk::new();
        c.add_const(Value::Int(0));
        c.add_const(Value::Int(42));
        c.add_const(Value::Int(-7));
        c.add_const(Value::Int(i64::MAX));
        c.add_const(Value::Int(i64::MIN));
        c.add_const(Value::Float(42.0));
        let rt = Chunk::from_bytes(&c.to_bytes().expect("serialize")).expect("decode");
        assert_eq!(rt.consts[0], Value::Int(0));
        assert_eq!(rt.consts[1], Value::Int(42));
        assert_eq!(rt.consts[2], Value::Int(-7));
        assert_eq!(rt.consts[3], Value::Int(i64::MAX));
        assert_eq!(rt.consts[4], Value::Int(i64::MIN));
        // The Float(42.0) entry stays a Float, NOT folded into the Int(42).
        assert!(matches!(rt.consts[5], Value::Float(n) if n == 42.0));
    }

    #[test]
    fn non_literal_const_self_check_fails() {
        let mut c = Chunk::new();
        // An Object is never poolable.
        c.consts.push(Value::Object(crate::value::ObjectCell::new(
            indexmap::IndexMap::new(),
        )));
        assert_eq!(c.check_consts_literal_only(), Err("object"));
    }

    #[test]
    fn array_of_str_const_roundtrips() {
        // The object-rest bound-key list is an `Array` of literal `Str`s; it must
        // pass the literal-only check and round-trip byte-stably.
        let mut c = Chunk::new();
        let keys = Value::Array(crate::value::ArrayCell::new(vec![
            Value::Str(std::rc::Rc::from("a")),
            Value::Str(std::rc::Rc::from("b")),
        ]));
        c.add_const(keys);
        assert_eq!(c.check_consts_literal_only(), Ok(()));
        let rt = Chunk::from_bytes(&c.to_bytes().expect("serialize")).expect("decode");
        match &rt.consts[0] {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a.len(), 2);
                assert!(matches!(&a[0], Value::Str(s) if &**s == "a"));
                assert!(matches!(&a[1], Value::Str(s) if &**s == "b"));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn array_const_with_nonliteral_element_rejected() {
        // An array containing a non-literal element is still rejected.
        let mut c = Chunk::new();
        c.consts
            .push(Value::Array(crate::value::ArrayCell::new(
                vec![Value::Object(crate::value::ObjectCell::new(
                    indexmap::IndexMap::new(),
                ))],
            )));
        assert_eq!(c.check_consts_literal_only(), Err("object"));
    }

    #[test]
    fn computed_field_defaults_roundtrip() {
        // A class whose fields use the full range of computed defaults the
        // `.aso` writer now serializes (binary, string concat, comparison,
        // logical, nullish, index, ternary, template, exclusive + inclusive
        // range, optional-member, `?`, `!`, `await`, assignment, and spreads). The
        // serialized field-default `ast::Expr` must round-trip structurally
        // (same disasm fingerprint) AND produce identical output when run.
        // NOTE: a bare `expr?`/`expr!` default at end of line followed by another
        // `name: type` field parses as a TERNARY (`expr ? name : type = ...`), since
        // the next field's `:` is taken as the ternary colon — that ambiguity is a
        // source-shape concern, not a serialization one. The `?`/`!` Try/Unwrap
        // round-trip is exercised by the explicit `EX_TRY`/`EX_UNWRAP` writer/reader
        // arms and the differential tests, so this class avoids that surface
        // collision and covers the remaining serialized forms (incl. `await`).
        let src = r#"
let PREFIX = "p-"
let BASE = 10
let SRC = [1, 2]
let OBJ = {a: 1}
fn add(a, b) { return a + b }

class Config {
    id: number = 1 + 1
    tag: string = PREFIX + "x"
    big: bool = BASE > 0
    pick: any = nil ?? "d"
    first: number = SRC[0]
    pri: number = BASE > 0 ? BASE * 2 : 0
    label: string = `b=${BASE}`
    seq: array<number> = 1..4
    iseq: array<number> = 1..=4
    a: number = OBJ?.a
    aw: number = await add(1, 2)
    xs: array<number> = [...SRC, 3]
    merged: object = {...OBJ, b: 2}
    summed: number = add(...SRC)
    fn init() {}
}

let c = Config()
print(c.id)
print(c.tag)
print(c.seq[2])
print(c.iseq)
print(c.merged)
"#;
        let original = compile(src);
        let bytes = original.to_bytes().expect("serialize");
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(
            disasm(&original),
            disasm(&rt),
            "computed-default disasm fingerprint differs after .aso round-trip"
        );
        // The field-default `ast::Expr` lives in a serialized ClassProto, not the
        // disasm, so also assert run-output parity through the round-trip.
        assert_eq!(
            run_chunk(compile(src)),
            run_chunk(Chunk::from_bytes(&compile(src).to_bytes().expect("serialize")).expect("decode")),
            "computed-default output differs after .aso round-trip"
        );
    }

    #[test]
    fn arrow_and_match_field_defaults_roundtrip(/* SP1 §4, format v11 */) {
        // Arrow + `match` field defaults are lowered by RE-PARSING source text; the
        // `.aso` writer persists that source (`EX_REPARSE`) and re-lowers it on load.
        // Assert (1) run-output parity through the round-trip and (2) that a LOADED
        // chunk re-serializes byte-identically (round-trip idempotence) — the
        // `default_sources` recovered by `read_class` make the second encode possible.
        let src = r#"
let base = 100

class C {
    f: fn = (n) => n + base
    label: string = match 2 { 1 => "one", 2 => "two", _ => "many" }
    fn init() {}
}

let c = C()
print(c.f(5))
print(c.label)
"#;
        assert_eq!(
            run_chunk(compile(src)),
            run_chunk(Chunk::from_bytes(&compile(src).to_bytes().expect("serialize")).expect("decode")),
            "arrow/match field-default output differs after .aso round-trip"
        );
        // Idempotence: encode → decode → encode must be byte-stable.
        let once = Chunk::from_bytes(&compile(src).to_bytes().expect("serialize")).expect("decode 1");
        let twice = Chunk::from_bytes(&once.to_bytes().expect("serialize")).expect("decode 2");
        assert_eq!(
            once.to_bytes().expect("serialize"),
            twice.to_bytes().expect("serialize"),
            "arrow/match field-default .aso re-serialization is not idempotent"
        );
    }

    // SP3 §A: the `.aso` writer's > u32::MAX capacity paths are exercised via a
    // FAKE length (no 4 GB allocation). `write_len` is pure: it records the sticky
    // overflow and writes a clamped placeholder, and `to_bytes` returns the typed
    // error. A length that fits is encoded with no overflow.

    #[test]
    fn write_len_byte_field_over_u32_is_clean_error() {
        let mut w = Writer::new();
        w.write_len((u32::MAX as usize) + 1, "byte field");
        match w.overflow {
            Some(AsoError::TooLarge { what, len }) => {
                assert_eq!(what, "byte field");
                assert_eq!(len, (u32::MAX as usize) + 1);
            }
            other => panic!("expected TooLarge byte-field overflow, got {other:?}"),
        }
    }

    #[test]
    fn write_len_collection_over_u32_is_clean_error() {
        let mut w = Writer::new();
        w.write_len((u32::MAX as usize) + 1, "collection");
        match w.overflow {
            Some(AsoError::TooLarge { what, len }) => {
                assert_eq!(what, "collection");
                assert_eq!(len, (u32::MAX as usize) + 1);
            }
            other => panic!("expected TooLarge collection overflow, got {other:?}"),
        }
    }

    #[test]
    fn write_len_in_range_does_not_overflow() {
        let mut w = Writer::new();
        w.write_len(42, "collection");
        assert!(w.overflow.is_none());
        assert_eq!(w.buf, 42u32.to_le_bytes());
    }

    #[test]
    fn too_large_messages_are_actionable() {
        let byte = AsoError::TooLarge {
            what: "byte field",
            len: 0,
        }
        .to_string();
        assert!(byte.contains("4 GB"), "byte-field message: {byte}");
        let coll = AsoError::TooLarge {
            what: "collection",
            len: 0,
        }
        .to_string();
        assert!(
            coll.contains("4 billion entries"),
            "collection message: {coll}"
        );
    }
}
