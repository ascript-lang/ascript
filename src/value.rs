//! Runtime values. Kinds: nil, bool, number, string, builtin, function, array,
//! object, map, enum, enum-variant, class, instance, bound-method, super-ref.

use crate::ast::Stmt;
use crate::env::Environment;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A hashable map key. Maps key on `nil`/`bool`/`number`/`string` (spec §11.2);
/// other value kinds are not hashable and panic at insertion time.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Nil,
    Bool(bool),
    Num(u64), // canonicalized f64 bits (−0.0→+0.0, all NaNs→one canonical NaN)
    Str(Rc<str>),
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

pub struct Class {
    pub name: String,
    pub superclass: Option<Rc<Class>>,
    pub methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: IndexMap<String, Value>,
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

/// A user-defined function with its captured (closure) environment.
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
    pub is_async: bool,
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
    /// A user-defined function carrying its closure environment.
    Function(Rc<Function>),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<IndexMap<String, Value>>>),
    // IndexMap (not HashMap) is deliberate: insertion order is required for
    // deterministic keys/values/entries/display and to match `Object`.
    Map(Rc<RefCell<IndexMap<MapKey, Value>>>),
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
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            // Functions compare by identity.
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
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
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Builtin(name) => write!(f, "Builtin({:?})", name),
            Value::Function(func) => {
                write!(f, "Function({})", func.name.as_deref().unwrap_or("<anonymous>"))
            }
            Value::Array(a) => write!(f, "Array(len {})", a.borrow().len()),
            Value::Object(o) => write!(f, "Object(len {})", o.borrow().len()),
            Value::Map(m) => write!(f, "Map(len {})", m.borrow().len()),
            Value::Bytes(b) => write!(f, "Bytes(len {})", b.borrow().len()),
            #[cfg(feature = "data")]
            Value::Regex(r) => write!(f, "Regex({:?})", r.source),
            Value::Native(n) => write!(f, "Native({} #{})", n.kind.type_name(), n.id),
            Value::NativeMethod(m) => write!(f, "NativeMethod({}.{})", m.receiver.kind.type_name(), m.method),
            Value::Enum(e) => write!(f, "Enum({})", e.name),
            Value::EnumVariant(v) => write!(f, "EnumVariant({}.{})", v.enum_name, v.name),
            Value::Class(c) => write!(f, "Class({})", c.name),
            Value::Instance(i) => write!(f, "Instance({})", i.borrow().class.name),
            Value::BoundMethod(b) => write!(f, "BoundMethod({})", b.name),
            Value::Super(_) => write!(f, "Super"),
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
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
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
        assert_eq!(Value::Builtin("print".into()), Value::Builtin("print".into()));
        assert_ne!(Value::Builtin("print".into()), Value::Builtin("len".into()));
        assert!(Value::Builtin("print".into()).is_truthy());
        assert_eq!(Value::Builtin("print".into()).to_string(), "<builtin print>");
    }

    #[test]
    fn arrays_compare_by_identity_and_display() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let a = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0), Value::Str("two".into())])));
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
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(Rc::new(RefCell::new(m)));
        assert_eq!(o.to_string(), "{a: 1, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }
}
