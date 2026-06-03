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

use crate::ast::{ArrayElem, CallArg, Expr, ExprKind, ObjEntry, Param, Type, UnOp};
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
pub const ASO_FORMAT_VERSION: u32 = 1;

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
        }
    }
}

impl std::error::Error for AsoError {}

// ---- constant-pool value tags ------------------------------------------------

const TAG_NIL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_NUMBER: u8 = 2;
const TAG_STR: u8 = 3;
const TAG_DECIMAL: u8 = 4;
const TAG_ENUM: u8 = 5;
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
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
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
        self.u32(u32::try_from(b.len()).expect(".aso byte field exceeds u32::MAX"));
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
        self.u32(u32::try_from(n).expect(".aso collection exceeds u32::MAX"));
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
    /// # Panics
    /// If the constant pool holds a non-literal value (a compiler invariant
    /// violation). Use [`Chunk::check_consts_literal_only`] for a non-panicking
    /// check. (The literal-only invariant is also asserted per-value during
    /// encoding via [`write_value`].)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.buf.extend_from_slice(&ASO_MAGIC);
        w.u32(ASO_FORMAT_VERSION);
        write_chunk(&mut w, self).expect("constant pool must be literals-only (compiler invariant)");
        w.buf
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
        Value::Number(_) => Ok("number"),
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
    c.consts.reserve(n);
    for _ in 0..n {
        c.consts.push(read_value(r)?);
    }
    // protos
    let n = r.len()?;
    c.protos.reserve(n);
    for _ in 0..n {
        c.protos.push(Rc::new(read_proto(r)?));
    }
    // class_protos
    let n = r.len()?;
    c.class_protos.reserve(n);
    for _ in 0..n {
        c.class_protos.push(Rc::new(read_class_proto(r)?));
    }
    // imports
    let n = r.len()?;
    c.imports.reserve(n);
    for _ in 0..n {
        c.imports.push(read_import(r)?);
    }
    // spans
    let n = r.len()?;
    c.spans.reserve(n);
    for _ in 0..n {
        let off = r.usize()?;
        let start = r.usize()?;
        let end = r.usize()?;
        c.spans.push((off, Span::new(start, end)));
    }
    // upvalues
    let n = r.len()?;
    c.upvalues.reserve(n);
    for _ in 0..n {
        c.upvalues.push(read_upvalue(r)?);
    }
    // cell_slots
    let n = r.len()?;
    c.cell_slots.reserve(n);
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
        Value::Number(n) => {
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
                    other => return Err(AsoError::NonLiteralConst(
                        literal_kind(other).err().unwrap_or("enum-variant-payload"),
                    )),
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
        TAG_NUMBER => Value::Number(r.f64()?),
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
            let mut variants = indexmap::IndexMap::with_capacity(n);
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
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n {
                elems.push(read_value(r)?);
            }
            Value::Array(Rc::new(std::cell::RefCell::new(elems)))
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
        | (u8::from(p.is_generator) << 2);
    w.u8(flags);
    // params
    w.len(p.params.len());
    for param in &p.params {
        write_param(w, param);
    }
    // ret
    write_opt_type(w, p.ret.as_ref());
    // recursive chunk
    write_chunk(w, &p.chunk)
}

fn read_proto(r: &mut Reader) -> Result<FnProto, AsoError> {
    let arity = r.u8()?;
    let flags = r.u8()?;
    let has_rest = flags & 1 != 0;
    let is_async = flags & 2 != 0;
    let is_generator = flags & 4 != 0;
    let n = r.len()?;
    let mut params = Vec::with_capacity(n);
    for _ in 0..n {
        params.push(read_param(r)?);
    }
    let ret = read_opt_type(r)?;
    let chunk = read_chunk(r)?;
    Ok(FnProto {
        chunk,
        arity,
        has_rest,
        is_async,
        is_generator,
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
}

fn read_param(r: &mut Reader) -> Result<Param, AsoError> {
    let name = r.str()?;
    let ty = read_opt_type(r)?;
    let start = r.usize()?;
    let end = r.usize()?;
    let rest = r.u8()? != 0;
    Ok(Param {
        name,
        ty,
        name_span: Span::new(start, end),
        rest,
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
            let mut ts = Vec::with_capacity(n);
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
        ExprKind::Number(n) => {
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
                    ArrayElem::Item(e) => write_expr(w, e)?,
                    // `cst_default_expr` rejects spread; reject here too.
                    ArrayElem::Spread(_) => {
                        return Err(AsoError::NonLiteralConst("array-spread-default"))
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
                        w.str(k);
                        write_expr(w, v)?;
                    }
                    ObjEntry::Spread(_) => {
                        return Err(AsoError::NonLiteralConst("object-spread-default"))
                    }
                }
            }
        }
        ExprKind::Member { object, name } => {
            w.u8(EX_MEMBER);
            w.str(name);
            write_expr(w, object)?;
        }
        ExprKind::Call { callee, args } => {
            w.u8(EX_CALL);
            write_expr(w, callee)?;
            w.len(args.len());
            for a in args {
                match a {
                    CallArg::Pos(e) => write_expr(w, e)?,
                    CallArg::Spread(_) => {
                        return Err(AsoError::NonLiteralConst("call-spread-default"))
                    }
                }
            }
        }
        // Any other ExprKind cannot occur in a `cst_default_expr`-produced default.
        _ => return Err(AsoError::NonLiteralConst("unsupported-default-expr")),
    }
    Ok(())
}

fn read_expr(r: &mut Reader) -> Result<Expr, AsoError> {
    let start = r.usize()?;
    let end = r.usize()?;
    let span = Span::new(start, end);
    let tag = r.u8()?;
    let kind = match tag {
        EX_NUMBER => ExprKind::Number(r.f64()?),
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
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n {
                elems.push(ArrayElem::Item(read_expr(r)?));
            }
            ExprKind::Array(elems)
        }
        EX_OBJECT => {
            let n = r.len()?;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let k = r.str()?;
                let v = read_expr(r)?;
                entries.push(ObjEntry::KV(k, v));
            }
            ExprKind::Object(entries)
        }
        EX_MEMBER => {
            let name = r.str()?;
            let object = Box::new(read_expr(r)?);
            ExprKind::Member { object, name }
        }
        EX_CALL => {
            let callee = Box::new(read_expr(r)?);
            let n = r.len()?;
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(CallArg::Pos(read_expr(r)?));
            }
            ExprKind::Call { callee, args }
        }
        tag => return Err(AsoError::BadTag { what: "expr", tag }),
    };
    Ok(Expr { kind, span })
}

// ---- ClassProto / Class / FieldSchema ----------------------------------------

fn write_class_proto(w: &mut Writer, cp: &ClassProto) -> Result<(), AsoError> {
    write_class(w, &cp.class)?;
    w.len(cp.default_fields.len());
    for f in &cp.default_fields {
        w.str(f);
    }
    w.len(cp.method_names.len());
    for m in &cp.method_names {
        w.str(m);
    }
    w.u8(u8::from(cp.has_super));
    Ok(())
}

fn read_class_proto(r: &mut Reader) -> Result<ClassProto, AsoError> {
    let class = read_class(r)?;
    let n = r.len()?;
    let mut default_fields = Vec::with_capacity(n);
    for _ in 0..n {
        default_fields.push(r.str()?);
    }
    let n = r.len()?;
    let mut method_names = Vec::with_capacity(n);
    for _ in 0..n {
        method_names.push(r.str()?);
    }
    let has_super = r.u8()? != 0;
    Ok(ClassProto {
        class: Rc::new(class),
        default_fields,
        method_names,
        has_super,
    })
}

fn write_class(w: &mut Writer, c: &Class) -> Result<(), AsoError> {
    // The compiler builds the ClassProto's class with `superclass: None`,
    // `methods` empty, and `def_env = global_env()` placeholder. Serialize only
    // the name + field schemas; the rest is rebuilt as the same inert template.
    w.str(&c.name);
    w.len(c.fields.len());
    for (fname, schema) in &c.fields {
        w.str(fname);
        write_type(w, &schema.ty);
        match &schema.default {
            Some(e) => {
                w.u8(1);
                write_expr(w, e)?;
            }
            None => w.u8(0),
        }
    }
    Ok(())
}

fn read_class(r: &mut Reader) -> Result<Class, AsoError> {
    let name = r.str()?;
    let n = r.len()?;
    let mut fields = indexmap::IndexMap::with_capacity(n);
    for _ in 0..n {
        let fname = r.str()?;
        let ty = read_type(r)?;
        let default = match r.u8()? {
            0 => None,
            1 => Some(read_expr(r)?),
            tag => return Err(AsoError::BadTag { what: "field-default", tag }),
        };
        fields.insert(fname, FieldSchema { ty, default });
    }
    Ok(Class {
        name,
        superclass: None,
        fields,
        methods: indexmap::IndexMap::new(),
        def_env: crate::interp::global_env(),
    })
}

// ---- ImportDesc --------------------------------------------------------------

fn write_import(w: &mut Writer, imp: &ImportDesc) {
    match imp {
        ImportDesc::Named { source, names } => {
            w.u8(IMP_NAMED);
            w.str(source);
            w.len(names.len());
            for (name, slot, is_cell) in names {
                w.str(name);
                w.u16(*slot);
                w.u8(u8::from(*is_cell));
            }
        }
        ImportDesc::Namespace {
            source,
            slot,
            is_cell,
        } => {
            w.u8(IMP_NAMESPACE);
            w.str(source);
            w.u16(*slot);
            w.u8(u8::from(*is_cell));
        }
    }
}

fn read_import(r: &mut Reader) -> Result<ImportDesc, AsoError> {
    match r.u8()? {
        IMP_NAMED => {
            let source = r.str()?;
            let n = r.len()?;
            let mut names = Vec::with_capacity(n);
            for _ in 0..n {
                let name = r.str()?;
                let slot = r.u16()?;
                let is_cell = r.u8()? != 0;
                names.push((name, slot, is_cell));
            }
            Ok(ImportDesc::Named { source, names })
        }
        IMP_NAMESPACE => {
            let source = r.str()?;
            let slot = r.u16()?;
            let is_cell = r.u8()? != 0;
            Ok(ImportDesc::Namespace {
                source,
                slot,
                is_cell,
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
        UpvalueDescriptor::ParentLocal(i) => {
            w.u8(UV_PARENT_LOCAL);
            w.u32(*i);
        }
        UpvalueDescriptor::ParentUpvalue(i) => {
            w.u8(UV_PARENT_UPVALUE);
            w.u32(*i);
        }
    }
}

fn read_upvalue(r: &mut Reader) -> Result<UpvalueDescriptor, AsoError> {
    match r.u8()? {
        UV_PARENT_LOCAL => Ok(UpvalueDescriptor::ParentLocal(r.u32()?)),
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
        let bytes = chunk.to_bytes();
        assert_eq!(&bytes[0..4], &ASO_MAGIC);
        let ver = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(ver, ASO_FORMAT_VERSION);
    }

    #[test]
    fn roundtrip_structural_equality_simple() {
        let original = compile("let a = 1\nlet b = \"hi\"\nprint(a)\nprint(b)");
        let bytes = original.to_bytes();
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(disasm(&original), disasm(&rt), "disasm fingerprint differs");
    }

    #[test]
    fn roundtrip_structural_equality_complex() {
        let original = compile(COMPLEX);
        let bytes = original.to_bytes();
        let rt = Chunk::from_bytes(&bytes).expect("decode");
        assert_eq!(disasm(&original), disasm(&rt), "disasm fingerprint differs");
    }

    #[test]
    fn roundtrip_produces_same_output() {
        // compile→run  vs  compile→to_bytes→from_bytes→run must be byte-identical.
        let direct = run_chunk(compile(COMPLEX));
        let viaso = run_chunk(Chunk::from_bytes(&compile(COMPLEX).to_bytes()).expect("decode"));
        assert_eq!(direct, viaso, "output differs after .aso round-trip");
    }

    #[test]
    fn double_roundtrip_is_stable() {
        let original = compile(COMPLEX);
        let once = Chunk::from_bytes(&original.to_bytes()).expect("decode 1");
        let twice = Chunk::from_bytes(&once.to_bytes()).expect("decode 2");
        assert_eq!(disasm(&original), disasm(&twice));
        // Bytes themselves are stable across re-encode.
        assert_eq!(once.to_bytes(), twice.to_bytes());
    }

    #[test]
    fn version_mismatch_detected() {
        let mut bytes = compile("print(1)").to_bytes();
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
        let mut bytes = compile("print(1)").to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            Chunk::from_bytes(&bytes),
            Err(AsoError::BadMagic(_))
        ));
        // Too short for even the magic.
        assert!(matches!(Chunk::from_bytes(&[0, 1]), Err(AsoError::Truncated)));
    }

    #[test]
    fn truncated_detected() {
        let bytes = compile(COMPLEX).to_bytes();
        // Drop the tail — header is intact but body is short.
        let half = &bytes[..bytes.len() / 2];
        assert!(matches!(
            Chunk::from_bytes(half),
            Err(AsoError::Truncated)
        ));
    }

    #[test]
    fn trailing_bytes_detected() {
        let mut bytes = compile("print(1)").to_bytes();
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

        let rt = Chunk::from_bytes(&original.to_bytes()).expect("decode");
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
        let rt = Chunk::from_bytes(&original.to_bytes()).expect("decode");
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
        c.add_const(Value::Number(f64::NAN));
        c.add_const(Value::Number(-0.0));
        c.add_const(Value::Number(f64::INFINITY));
        c.add_const(Value::Decimal(Decimal::from_str("1.50").unwrap()));
        let rt = Chunk::from_bytes(&c.to_bytes()).expect("decode");
        assert!(matches!(rt.consts[0], Value::Number(n) if n.is_nan()));
        assert!(matches!(rt.consts[1], Value::Number(n) if n == 0.0 && n.is_sign_negative()));
        assert!(matches!(rt.consts[2], Value::Number(n) if n.is_infinite()));
        match &rt.consts[3] {
            Value::Decimal(d) => assert_eq!(d.to_string(), "1.50"),
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn non_literal_const_self_check_fails() {
        let mut c = Chunk::new();
        // An Object is never poolable.
        c.consts
            .push(Value::Object(crate::value::ObjectCell::new(indexmap::IndexMap::new())));
        assert_eq!(c.check_consts_literal_only(), Err("object"));
    }

    #[test]
    fn array_of_str_const_roundtrips() {
        // The object-rest bound-key list is an `Array` of literal `Str`s; it must
        // pass the literal-only check and round-trip byte-stably.
        let mut c = Chunk::new();
        let keys = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![
            Value::Str(std::rc::Rc::from("a")),
            Value::Str(std::rc::Rc::from("b")),
        ])));
        c.add_const(keys);
        assert_eq!(c.check_consts_literal_only(), Ok(()));
        let rt = Chunk::from_bytes(&c.to_bytes()).expect("decode");
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
            .push(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![
                Value::Object(crate::value::ObjectCell::new(indexmap::IndexMap::new())),
            ]))));
        assert_eq!(c.check_consts_literal_only(), Err("object"));
    }
}
