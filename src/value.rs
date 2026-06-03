//! Runtime values. Kinds: nil, bool, number, decimal, string, builtin, function,
//! array, object, map, set, enum, enum-variant, class, instance, bound-method,
//! super-ref, future.

use crate::ast::Stmt;
use crate::env::Environment;
use indexmap::{IndexMap, IndexSet};
use rust_decimal::Decimal;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::rc::Rc;

/// The heap payload behind `Value::Object`. Wraps the insertion-ordered key→value
/// map together with a `shape` id (V11-T2 hidden classes). The `shape` identifies
/// the object's key-LAYOUT in the VM's per-VM `ShapeRegistry`; V11-T3 inline caches
/// validate `obj.shape == cached_shape` then read a value by index.
///
/// `shape` defaults to `0` (the empty/unset layout). The TREE-WALKER never reads or
/// writes it (its objects keep shape 0); only VM code paths assign shapes. The
/// `borrow`/`borrow_mut` helpers mirror the old `Rc<RefCell<IndexMap>>` API so the
/// vast majority of access sites are unchanged.
pub struct ObjectCell {
    pub map: RefCell<IndexMap<String, Value>>,
    pub shape: Cell<u32>,
}

impl ObjectCell {
    /// Wrap an `IndexMap` into a shared `ObjectCell` with shape `0` (unset).
    pub fn new(map: IndexMap<String, Value>) -> Rc<ObjectCell> {
        Rc::new(ObjectCell {
            map: RefCell::new(map),
            shape: Cell::new(0),
        })
    }

    /// Immutable borrow of the entry map (drop-in for the old `Rc<RefCell<…>>`).
    pub fn borrow(&self) -> Ref<'_, IndexMap<String, Value>> {
        self.map.borrow()
    }

    /// Mutable borrow of the entry map (drop-in for the old `Rc<RefCell<…>>`).
    pub fn borrow_mut(&self) -> RefMut<'_, IndexMap<String, Value>> {
        self.map.borrow_mut()
    }
}

/// A hashable map key. Maps key on `nil`/`bool`/`number`/`decimal`/`string`
/// (spec §11.2 + decimal extension). Number and Decimal are distinct key kinds.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Nil,
    Bool(bool),
    Num(u64), // canonicalized f64 bits (−0.0→+0.0, all NaNs→one canonical NaN)
    Str(Rc<str>),
    /// Exact decimal key. Distinct from `Num` — `Decimal("0.1")` ≠ `Num(0.1f64)`.
    Decimal(Decimal),
}

impl MapKey {
    /// Convert a value to a key, or `None` if its kind is not hashable.
    pub fn from_value(v: &Value) -> Option<MapKey> {
        match v {
            Value::Nil => Some(MapKey::Nil),
            Value::Bool(b) => Some(MapKey::Bool(*b)),
            Value::Number(n) => {
                let canon = if *n == 0.0 {
                    0.0f64.to_bits()
                } else if n.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    n.to_bits()
                };
                Some(MapKey::Num(canon))
            }
            Value::Str(s) => Some(MapKey::Str(s.clone())),
            Value::Decimal(d) => Some(MapKey::Decimal(*d)),
            _ => None,
        }
    }

    /// Recover the value form of a key (for `keys`/`entries`/display/contracts).
    pub fn to_value(&self) -> Value {
        match self {
            MapKey::Nil => Value::Nil,
            MapKey::Bool(b) => Value::Bool(*b),
            MapKey::Num(bits) => Value::Number(f64::from_bits(*bits)),
            MapKey::Str(s) => Value::Str(s.clone()),
            MapKey::Decimal(d) => Value::Decimal(*d),
        }
    }
}

pub struct EnumDef {
    pub name: String,
    pub variants: IndexMap<String, Value>, // each is a Value::EnumVariant
}

pub struct EnumVariant {
    pub enum_name: String,
    pub name: String,
    pub value: Value, // backing value, or Nil
}

pub struct Method {
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub is_async: bool,
}

#[derive(Clone)]
pub struct FieldSchema {
    pub ty: crate::ast::Type,
    pub default: Option<crate::ast::Expr>,
}

pub struct Class {
    pub name: String,
    pub superclass: Option<Rc<Class>>,
    pub fields: IndexMap<String, FieldSchema>,
    pub methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: IndexMap<String, Value>,
    /// The instance's key-layout id (V11-T2 hidden classes). Defaults to `0`
    /// (unset); the tree-walker leaves it at `0`, the VM assigns the class's base
    /// shape (and transitions it if a field is added). `Cell` so a `&self` VM
    /// method can update it without a mutable instance borrow.
    pub shape_id: Cell<u32>,
}

pub struct BoundMethod {
    pub receiver: Value,
    pub method: Rc<Method>,
    pub defining_class: Rc<Class>,
    pub name: String,
}

pub struct SuperRef {
    pub receiver: Value,
    pub start: Option<Rc<Class>>,
}

/// A compiled regular expression (spec §11.2). Immutable; identity equality.
/// Gated on the `data` feature because `regex::Regex` only exists with it.
#[cfg(feature = "data")]
pub struct RegexHandle {
    pub re: regex::Regex,
    pub source: String,
}

/// A native resource handle (sqlite connection/statement, process child/reader/writer,
/// and — in M14 — http bodies/sse/sockets). The non-Clone OS resource lives in the
/// interp's `resources` table keyed by `id`; this value is a cheap clonable handle.
pub struct NativeObject {
    pub id: u64,
    pub kind: NativeKind,
    /// Plain readable fields (e.g. a child's `pid`); methods are resolved separately.
    pub fields: indexmap::IndexMap<String, Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // Some variants are only constructed by feature-gated modules (sqlite/process).
pub enum NativeKind {
    SqliteConnection,
    SqliteStatement,
    ChildProcess,
    Reader,
    Writer,
    // M14 networking handles (registered only under feature `net`).
    TcpListener,
    TcpStream,
    HttpResponse,
    // A streaming HTTP response body reader (`resp.body` when `opts.stream:true`).
    // Follows the §11.4 reader idiom over a chunked byte stream.
    HttpBody,
    // A cancellation token for in-flight HTTP requests (`http.cancelToken()`).
    CancelHandle,
    // A first-class Server-Sent Events client stream (`http.sse(url, opts?)`).
    // `next()` yields parsed `{event,data,id,retry}` events; `lastEventId` is a
    // readable property; auto-reconnects on disconnect (see std/net/http).
    SseStream,
    // M14 std/http/server: a server handle holding registered routes + middleware
    // and (after `bind`) the live `TcpListener`. Methods: route/use/bind/serve/listen.
    HttpServer,
    // M14 std/http/server: the `next` callable handed to a middleware. Calling it
    // (as a `NativeMethod`) advances the middleware chain → matched route handler.
    HttpNext,
    // M14 std/net/ws: a connected WebSocket (client `connect` or server `accept`).
    // Methods: send/recv/close. Unifies the client/server stream types behind one
    // boxed Sink+Stream of `Message` (see net_ws::WsConnState).
    WsConnection,
    // M14 std/net/ws: an accept-based WebSocket server listener (binds a TcpListener;
    // `accept()` performs the handshake → WsConnection). Carries a `port` field.
    WsListener,
    // M15 std/tui: a terminal handle owning the back/flushed screen buffers, the
    // cursor position, and the active raw/alt-screen flags. Methods: size/clear/
    // moveCursor/enterRaw/leaveRaw/enterAltScreen/leaveAltScreen/showCursor/draw
    // (setCell/text/hline/vline/box/fill)/flush/pollEvent/readEvent/restore/close.
    // Registered only under feature `tui`.
    Terminal,
    // std/sync: a FIFO channel (VecDeque + Rc<Notify>). Not feature-gated.
    Channel,
    // std/sync: a counting semaphore (RefCell<usize> + Rc<Notify>). Not feature-gated.
    Semaphore,
    // std/time: a repeating timer handle. `.tick()` awaits the next tick.
    // Not feature-gated (tokio timers are always available).
    Interval,
    // std/time: a debounce wrapper (trailing-edge). Callable as `wrapper(args)`.
    DebounceWrapper,
    // std/time: a throttle wrapper (leading-edge). Callable as `wrapper(args)`.
    ThrottleWrapper,
    // std/sync: a token-bucket rate limiter. `.acquire()` awaits a token; the
    // bucket refills `count` tokens every `window_ms` milliseconds (monotonic
    // clock — no background task). Not feature-gated.
    RateLimiter,
    // std/net/udp: a bound UDP socket. Methods: send/recv/localAddr/close.
    // Registered only under feature `net`.
    UdpSocket,
    // std/stream: a lazy pull-based stream (a source + a chain of combinator
    // stages). Driven by terminals via `Interp::pull_next`. Not feature-gated.
    Stream,
}

impl NativeKind {
    pub fn type_name(self) -> &'static str {
        match self {
            NativeKind::SqliteConnection => "connection",
            NativeKind::SqliteStatement => "statement",
            NativeKind::ChildProcess => "childProcess",
            NativeKind::Reader => "reader",
            NativeKind::Writer => "writer",
            NativeKind::TcpListener => "tcpListener",
            NativeKind::TcpStream => "tcpStream",
            NativeKind::HttpResponse => "httpResponse",
            NativeKind::HttpBody => "httpBody",
            NativeKind::CancelHandle => "cancelHandle",
            NativeKind::SseStream => "sseStream",
            NativeKind::HttpServer => "httpServer",
            NativeKind::HttpNext => "httpNext",
            NativeKind::WsConnection => "wsConnection",
            NativeKind::WsListener => "wsListener",
            NativeKind::Terminal => "terminal",
            NativeKind::Channel => "channel",
            NativeKind::Semaphore => "semaphore",
            NativeKind::Interval => "interval",
            NativeKind::DebounceWrapper => "debounce",
            NativeKind::ThrottleWrapper => "throttle",
            NativeKind::RateLimiter => "rateLimiter",
            NativeKind::UdpSocket => "udpSocket",
            NativeKind::Stream => "stream",
        }
    }
}

/// A method bound to a native handle (e.g. `child.wait`), dispatched async.
pub struct NativeMethod {
    pub receiver: std::rc::Rc<NativeObject>,
    pub method: String,
}

/// Walk a class chain for a method, returning it plus the class that defined it.
pub fn find_method(class: &Rc<Class>, name: &str) -> Option<(Rc<Method>, Rc<Class>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(m) = c.methods.get(name) {
            return Some((m.clone(), c.clone()));
        }
        cur = c.superclass.clone();
    }
    None
}

/// Merge the declared field schemas across a class's inheritance chain,
/// **base-class first** so a subclass declaration overrides a base one with the
/// same name. Each entry carries the class that declared it, so callers that
/// evaluate field defaults can use the *defining* class's `def_env`. Insertion
/// order is base-first, then subclass (a subclass override keeps the field's
/// original position, matching `IndexMap::insert` semantics).
pub fn merged_field_schema(class: &Rc<Class>) -> IndexMap<String, (FieldSchema, Rc<Class>)> {
    let mut chain = Vec::new();
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        cur = c.superclass.clone();
        chain.push(c);
    }
    let mut schema: IndexMap<String, (FieldSchema, Rc<Class>)> = IndexMap::new();
    for c in chain.into_iter().rev() {
        for (n, s) in &c.fields {
            schema.insert(n.clone(), (s.clone(), c.clone()));
        }
    }
    schema
}

/// A user-defined function with its captured (closure) environment.
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
    pub is_async: bool,
    pub is_generator: bool,
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    /// Exact decimal arithmetic (96-bit scaled integer via `rust_decimal`).
    /// `Copy` — no heap allocation; `Hash + Eq + Ord` via the inner type.
    /// Participates in operator overloading with `Number` via coercion.
    Decimal(Decimal),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
    /// A user-defined function carrying its closure environment.
    Function(Rc<Function>),
    /// A bytecode-VM closure: a function prototype plus its captured upvalue
    /// cells. Behaves like `Function` to the user (same `type()`/display);
    /// identity equality. Produced by the VM (V4+); inert in the tree-walker.
    Closure(Rc<crate::vm::value_ext::Closure>),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<ObjectCell>),
    // IndexMap (not HashMap) is deliberate: insertion order is required for
    // deterministic keys/values/entries/display and to match `Object`.
    Map(Rc<RefCell<IndexMap<MapKey, Value>>>),
    /// An insertion-ordered hash set of hashable values (spec §11.2).
    /// Elements use the same `MapKey` type as Map keys.
    /// Identity equality (like Array/Map/Bytes).
    Set(Rc<RefCell<IndexSet<MapKey>>>),
    /// A mutable byte buffer (spec §11.2). Identity equality, like Array/Map.
    Bytes(Rc<RefCell<Vec<u8>>>),
    /// A compiled regular expression (spec §11.2). Identity equality.
    #[cfg(feature = "data")]
    Regex(Rc<RegexHandle>),
    /// A native resource handle (spec §11.2/§11.4). Always compiled; only the
    /// feature-gated modules (sqlite/process) construct one. Identity equality.
    Native(Rc<NativeObject>),
    /// A method bound to a native handle, dispatched by the async `call_native_method`.
    NativeMethod(Rc<NativeMethod>),
    Enum(Rc<EnumDef>),
    EnumVariant(Rc<EnumVariant>),
    Class(Rc<Class>),
    Instance(Rc<RefCell<Instance>>),
    BoundMethod(Rc<BoundMethod>),
    Super(Rc<SuperRef>),
    /// A pending or completed async computation (spec §7, M17 Phase 2). Produced
    /// by calling a script `async fn` and driven by `await`. Identity equality.
    Future(crate::task::SharedFuture),
    /// A running script generator (spec §7, M17 Phase 4). Produced by calling a
    /// `fn*` / `async fn*`; consumed by `for await` or `gen.next(v)`. Holds the
    /// rendezvous channel to the spawned body task. Identity equality.
    Generator(Rc<crate::coro::GeneratorHandle>),
    /// A method bound to a generator handle (e.g. `gen.next`), dispatched by the
    /// async `call_generator_method`. Generators have no `NativeObject`, so they
    /// can't reuse `NativeMethod`; this is the parallel binding for them.
    GeneratorMethod(Rc<crate::coro::GeneratorHandle>, &'static str),
    /// A class associated function bound to its class, e.g. `User.from`.
    ClassMethod(Rc<Class>, &'static str),
}

impl Value {
    /// Spec §4: only `nil` and `false` are falsy. Everything else
    /// (including `0` and `""`) is truthy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            // Decimal: same-type value equality by the Decimal's own PartialEq.
            // Cross-type Number↔Decimal equality is handled in the evaluator's
            // Eq/Ne path, not here.
            (Value::Decimal(a), Value::Decimal(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            // Functions compare by identity.
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => Rc::ptr_eq(a, b),
            (Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),
            #[cfg(feature = "data")]
            (Value::Regex(a), Value::Regex(b)) => Rc::ptr_eq(a, b),
            // Native handles and bound native methods compare by identity.
            (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
            (Value::NativeMethod(a), Value::NativeMethod(b)) => Rc::ptr_eq(a, b),
            // Enums and their (interned) variants compare by identity.
            (Value::Enum(a), Value::Enum(b)) => Rc::ptr_eq(a, b),
            (Value::EnumVariant(a), Value::EnumVariant(b)) => Rc::ptr_eq(a, b),
            // Classes/instances/bound-methods/super compare by identity.
            (Value::Class(a), Value::Class(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => Rc::ptr_eq(a, b),
            (Value::BoundMethod(a), Value::BoundMethod(b)) => Rc::ptr_eq(a, b),
            (Value::Super(a), Value::Super(b)) => Rc::ptr_eq(a, b),
            // Futures compare by identity (same completion cell).
            (Value::Future(a), Value::Future(b)) => a.ptr_eq(b),
            // Generators compare by identity (same body channel).
            (Value::Generator(a), Value::Generator(b)) => Rc::ptr_eq(a, b),
            (Value::GeneratorMethod(a, an), Value::GeneratorMethod(b, bn)) => {
                Rc::ptr_eq(a, b) && an == bn
            }
            (Value::ClassMethod(a, an), Value::ClassMethod(b, bn)) => Rc::ptr_eq(a, b) && an == bn,
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Number(n) => write!(f, "Number({})", n),
            Value::Decimal(d) => write!(f, "Decimal({})", d),
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Builtin(name) => write!(f, "Builtin({:?})", name),
            Value::Function(func) => {
                write!(
                    f,
                    "Function({})",
                    func.name.as_deref().unwrap_or("<anonymous>")
                )
            }
            Value::Closure(_) => write!(f, "Closure(<anonymous>)"),
            Value::Array(a) => write!(f, "Array(len {})", a.borrow().len()),
            Value::Object(o) => write!(f, "Object(len {})", o.borrow().len()),
            Value::Map(m) => write!(f, "Map(len {})", m.borrow().len()),
            Value::Set(s) => write!(f, "Set(len {})", s.borrow().len()),
            Value::Bytes(b) => write!(f, "Bytes(len {})", b.borrow().len()),
            #[cfg(feature = "data")]
            Value::Regex(r) => write!(f, "Regex({:?})", r.source),
            Value::Native(n) => write!(f, "Native({} #{})", n.kind.type_name(), n.id),
            Value::NativeMethod(m) => write!(
                f,
                "NativeMethod({}.{})",
                m.receiver.kind.type_name(),
                m.method
            ),
            Value::Enum(e) => write!(f, "Enum({})", e.name),
            Value::EnumVariant(v) => write!(f, "EnumVariant({}.{})", v.enum_name, v.name),
            Value::Class(c) => write!(f, "Class({})", c.name),
            Value::Instance(i) => write!(f, "Instance({})", i.borrow().class.name),
            Value::BoundMethod(b) => write!(f, "BoundMethod({})", b.name),
            Value::Super(_) => write!(f, "Super"),
            Value::Future(_) => write!(f, "Future"),
            Value::Generator(_) => write!(f, "Generator"),
            Value::GeneratorMethod(_, m) => write!(f, "GeneratorMethod({})", m),
            Value::ClassMethod(c, m) => write!(f, "ClassMethod({}.{})", c.name, m),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_display(f, &mut Vec::new())
    }
}

impl Value {
    fn write_display(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            // Rust's f64 Display already prints 7.0 as "7" and 2.5 as "2.5".
            Value::Number(n) => write!(f, "{}", n),
            // Decimal: print the canonical string (scale preserved, e.g. "1.50").
            Value::Decimal(d) => write!(f, "{}", d),
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
            // A VM closure has no name on its proto, so it displays exactly like
            // an anonymous `Function`. (Same concept to the user.)
            Value::Closure(_) => write!(f, "<function>"),
            Value::Array(a) => {
                let ptr = Rc::as_ptr(a) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "[...]");
                }
                seen.push(ptr);
                write!(f, "[")?;
                for (i, v) in a.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.write_element(f, seen)?;
                }
                write!(f, "]")?;
                seen.pop();
                Ok(())
            }
            Value::Object(o) => {
                let ptr = Rc::as_ptr(o) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "{{...}}");
                }
                seen.push(ptr);
                write!(f, "{{")?;
                for (i, (k, v)) in o.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
            Value::Map(m) => {
                let ptr = Rc::as_ptr(m) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "map {{...}}");
                }
                seen.push(ptr);
                write!(f, "map {{")?;
                for (i, (k, v)) in m.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    k.to_value().write_element(f, seen)?;
                    write!(f, ": ")?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
            Value::Set(s) => {
                let ptr = Rc::as_ptr(s) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "set {{...}}");
                }
                seen.push(ptr);
                write!(f, "set {{")?;
                for (i, k) in s.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    k.to_value().write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
            Value::Bytes(b) => write!(f, "<bytes len {}>", b.borrow().len()),
            #[cfg(feature = "data")]
            Value::Regex(r) => write!(f, "<regex {}>", r.source),
            Value::Native(n) => write!(f, "<native {} #{}>", n.kind.type_name(), n.id),
            Value::NativeMethod(m) => write!(f, "<native method {}>", m.method),
            Value::Enum(e) => write!(f, "<enum {}>", e.name),
            Value::EnumVariant(v) => write!(f, "{}.{}", v.enum_name, v.name),
            Value::Class(c) => write!(f, "<class {}>", c.name),
            Value::Instance(i) => write!(f, "<{} instance>", i.borrow().class.name),
            Value::BoundMethod(b) => write!(f, "<method {}>", b.name),
            Value::Super(_) => write!(f, "<super>"),
            Value::Future(_) => write!(f, "<future>"),
            Value::Generator(_) => write!(f, "<generator>"),
            Value::GeneratorMethod(_, m) => write!(f, "<generator method {}>", m),
            Value::ClassMethod(c, m) => write!(f, "<class method {}.{}>", c.name, m),
        }
    }

    /// Like `write_display`, but quotes bare strings (used for nested elements
    /// so `[1, "two"]` shows the quotes while top-level `print("x")` stays raw).
    fn write_element(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{:?}", s),
            _ => self.write_display(f, seen),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_values_like_a_script_language() {
        assert_eq!(Value::Number(7.0).to_string(), "7");
        assert_eq!(Value::Number(2.5).to_string(), "2.5");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Nil.to_string(), "nil");
        assert_eq!(Value::Str("hi".into()).to_string(), "hi");
    }

    #[test]
    fn truthiness_follows_spec() {
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Number(0.0).is_truthy());
        assert!(Value::Str("".into()).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
    }

    #[test]
    fn equality_is_structural_and_cross_kind_is_false() {
        assert_eq!(Value::Number(1.0), Value::Number(1.0));
        assert_eq!(Value::Str("a".into()), Value::Str("a".into()));
        assert_ne!(Value::Number(1.0), Value::Str("1".into()));
        assert_ne!(Value::Bool(true), Value::Number(1.0));
    }

    #[test]
    fn builtins_compare_by_name_and_are_truthy() {
        assert_eq!(
            Value::Builtin("print".into()),
            Value::Builtin("print".into())
        );
        assert_ne!(Value::Builtin("print".into()), Value::Builtin("len".into()));
        assert!(Value::Builtin("print".into()).is_truthy());
        assert_eq!(
            Value::Builtin("print".into()).to_string(),
            "<builtin print>"
        );
    }

    #[test]
    fn arrays_compare_by_identity_and_display() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let a = Value::Array(Rc::new(RefCell::new(vec![
            Value::Number(1.0),
            Value::Str("two".into()),
        ])));
        assert_eq!(a.to_string(), "[1, \"two\"]");
        // identity: a clone of the SAME Rc is equal; a fresh array is not
        assert_eq!(a.clone(), a);
        let b = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0)])));
        assert_ne!(a, b);
        assert!(a.is_truthy());
    }

    #[test]
    fn maps_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert(MapKey::Str("a".into()), Value::Number(1.0));
        m.insert(MapKey::Num(0.0f64.to_bits()), Value::Str("zero".into()));
        let map = Value::Map(Rc::new(RefCell::new(m)));
        assert_eq!(map.to_string(), "map {\"a\": 1, 0: \"zero\"}");
        assert_eq!(map.clone(), map);
        assert!(map.is_truthy());
        assert!(MapKey::from_value(&Value::Number(0.0)).is_some());
        assert!(MapKey::from_value(&Value::Array(Rc::new(RefCell::new(vec![])))).is_none());
    }

    #[test]
    fn mapkey_number_and_decimal_are_distinct() {
        use rust_decimal::Decimal;
        // Number 1 and Decimal 1 must produce DIFFERENT map keys, so they index
        // distinct slots in a Map/Set. This pins the MapKey::Decimal claim directly.
        // (MapKey intentionally has no Debug derive, so compare via bool to avoid
        // requiring it in assert_eq!/assert_ne!.)
        let num_key = MapKey::from_value(&Value::Number(1.0)).expect("number is hashable");
        let dec_key =
            MapKey::from_value(&Value::Decimal(Decimal::from(1))).expect("decimal is hashable");
        assert!(
            num_key != dec_key,
            "number 1 and decimal 1 must be distinct map keys"
        );
        // Two equal Decimals produce the same key (round-trips through to_value).
        let a = MapKey::from_value(&Value::Decimal(Decimal::from(1)));
        let b = MapKey::from_value(&Value::Decimal(Decimal::from(1)));
        assert!(a == b);
        assert_eq!(dec_key.to_value(), Value::Decimal(Decimal::from(1)));
    }

    #[test]
    fn closure_behaves_like_an_anonymous_function() {
        use crate::vm::chunk::{Chunk, FnProto};
        use crate::vm::value_ext::Closure;

        let proto = Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        let a = Closure::new(proto);
        let cv = Value::Closure(a.clone());

        // Display mirrors an anonymous Function exactly.
        assert_eq!(cv.to_string(), "<function>");
        assert_eq!(Value::Function(anon_function()).to_string(), "<function>");

        // type() reports "function", like a Function.
        assert_eq!(crate::interp::type_name(&cv), "function");
        assert_eq!(
            crate::interp::type_name(&Value::Function(anon_function())),
            "function"
        );

        // Pointer identity: same Rc is equal; a distinct closure is not.
        assert_eq!(Value::Closure(a.clone()), Value::Closure(a.clone()));
        let b = Closure::new(Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        }));
        assert_ne!(Value::Closure(a), Value::Closure(b));

        // Not a valid map key (mirrors Function).
        assert!(MapKey::from_value(&cv).is_none());

        // Truthy, like any callable.
        assert!(cv.is_truthy());
    }

    fn anon_function() -> Rc<Function> {
        Rc::new(Function {
            name: None,
            params: vec![],
            ret: None,
            body: vec![],
            closure: Environment::global(),
            is_async: false,
            is_generator: false,
        })
    }

    #[test]
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(ObjectCell::new(m));
        assert_eq!(o.to_string(), "{a: 1, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }
}
