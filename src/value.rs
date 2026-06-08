//! Runtime values. Kinds: nil, bool, number, decimal, string, builtin, function,
//! array, object, map, set, enum, enum-variant, class, instance, bound-method,
//! super-ref, future.

use crate::ast::Stmt;
use crate::env::Environment;
use gcmodule::Cc;
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
    /// `object.freeze` flag (SP2 §4). Defaults `false`. A `Cell` (not `RefCell`)
    /// so a `&self` engine can set/read it without a borrow conflict and with no
    /// await-holding-borrow risk; it is `Copy` and adds no GC-traceable edge, so
    /// `Value::trace`/the cycle collector are unaffected.
    pub frozen: Cell<bool>,
}

impl ObjectCell {
    /// Wrap an `IndexMap` into a shared `ObjectCell` with shape `0` (unset),
    /// unfrozen.
    pub fn new(map: IndexMap<String, Value>) -> Cc<ObjectCell> {
        Cc::new(ObjectCell {
            map: RefCell::new(map),
            shape: Cell::new(0),
            frozen: Cell::new(false),
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

    /// Whether this object has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this object frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

/// The heap payload behind `Value::Array` (SP2 §4 / decision D3). Wraps the
/// element `Vec<Value>` together with an `object.freeze` flag. The wrapper exists
/// ONLY to carry the `frozen` flag beside the vector — exactly mirroring the
/// V11-T2 `ObjectCell` migration — so the `borrow()`/`borrow_mut()` shims keep the
/// vast majority of array access sites textually unchanged. `frozen` is a
/// `Cell<bool>` (`Copy`, no-op `Trace`): it adds no GC-traceable edge, so
/// `Value::trace` is unaffected.
pub struct ArrayCell {
    pub vec: RefCell<Vec<Value>>,
    pub frozen: Cell<bool>,
}

impl ArrayCell {
    /// Wrap a `Vec<Value>` into a shared, `Cc`-managed `ArrayCell` (unfrozen).
    pub fn new(vec: Vec<Value>) -> Cc<ArrayCell> {
        Cc::new(ArrayCell {
            vec: RefCell::new(vec),
            frozen: Cell::new(false),
        })
    }

    /// Immutable borrow of the element vector (drop-in for the old
    /// `Cc<RefCell<Vec<Value>>>`).
    pub fn borrow(&self) -> Ref<'_, Vec<Value>> {
        self.vec.borrow()
    }

    /// Mutable borrow of the element vector (drop-in for the old
    /// `Cc<RefCell<Vec<Value>>>`).
    pub fn borrow_mut(&self) -> RefMut<'_, Vec<Value>> {
        self.vec.borrow_mut()
    }

    /// Whether this array has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this array frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

/// The heap payload behind `Value::Map`. A thin newtype around the entry
/// `RefCell<IndexMap<…>>` whose only purpose is to carry a hand-written
/// [`gcmodule::Trace`] impl: `IndexMap` is a foreign type, so we cannot give it
/// (nor `RefCell<IndexMap>`) a blanket `Trace` impl (orphan rule). Wrapping it in
/// this local newtype lets `Cc<MapCell>` satisfy `T: Trace` while the cycle
/// collector still reaches the contained `Value`s. `Deref`s to the inner
/// `RefCell` so every `m.borrow()`/`m.borrow_mut()` access site is unchanged.
pub struct MapCell {
    pub map: RefCell<IndexMap<MapKey, Value>>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. See [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
}

impl MapCell {
    /// Wrap an `IndexMap` into a shared, `Cc`-managed `MapCell` (unfrozen).
    pub fn new(map: IndexMap<MapKey, Value>) -> Cc<MapCell> {
        Cc::new(MapCell {
            map: RefCell::new(map),
            frozen: Cell::new(false),
        })
    }

    /// Whether this map has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this map frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

impl std::ops::Deref for MapCell {
    type Target = RefCell<IndexMap<MapKey, Value>>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

/// The heap payload behind `Value::Set`. See [`MapCell`] — same story, a local
/// newtype over `RefCell<IndexSet<…>>` so it can carry a `Trace` impl (foreign
/// `IndexSet` cannot) and `Cc<SetCell>` satisfies `T: Trace`.
pub struct SetCell {
    pub set: RefCell<IndexSet<MapKey>>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. See [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
}

impl SetCell {
    /// Wrap an `IndexSet` into a shared, `Cc`-managed `SetCell` (unfrozen).
    pub fn new(set: IndexSet<MapKey>) -> Cc<SetCell> {
        Cc::new(SetCell {
            set: RefCell::new(set),
            frozen: Cell::new(false),
        })
    }

    /// Whether this set has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this set frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

impl std::ops::Deref for SetCell {
    type Target = RefCell<IndexSet<MapKey>>;
    fn deref(&self) -> &Self::Target {
        &self.set
    }
}

/// A hashable map key. Maps key on `nil`/`bool`/`number`/`decimal`/`string`
/// (spec §11.2 + decimal extension). Number and Decimal are distinct key kinds.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Nil,
    Bool(bool),
    /// Exact integer key (NUM §3.3). An integral, finite, in-i64-range `Float`
    /// FOLDS into this variant so `Int(1)` and `Float(1.0)` are the SAME key.
    Int(i64),
    Num(u64), // canonicalized f64 bits (−0.0→+0.0, all NaNs→one canonical NaN)
    Str(Rc<str>),
    /// Exact decimal key. Distinct from `Num`/`Int` — `Decimal("0.1")` ≠ `Num(0.1f64)`,
    /// `Decimal("1")` ≠ `Int(1)` (Decimal is exact and opt-in; never folded).
    Decimal(Decimal),
}

impl MapKey {
    /// Convert a value to a key, or `None` if its kind is not hashable.
    pub fn from_value(v: &Value) -> Option<MapKey> {
        match v {
            Value::Nil => Some(MapKey::Nil),
            Value::Bool(b) => Some(MapKey::Bool(*b)),
            Value::Int(i) => Some(MapKey::Int(*i)),
            Value::Float(n) => {
                // NUM §3.3: an integral, finite, in-i64-range float folds to the same
                // key as the equal `int` (so `map[1]` and `map[1.0]` collide). Every
                // other float (fractional, ±inf, NaN) keeps its canonical-bits key —
                // NaN stays a single canonical bit pattern (storable, but never equal
                // to a non-NaN under the evaluator's `==`).
                if n.fract() == 0.0
                    && n.is_finite()
                    && *n >= i64::MIN as f64
                    && *n <= i64::MAX as f64
                {
                    Some(MapKey::Int(*n as i64))
                } else {
                    // Only fractional or non-finite floats reach here (±0.0 folded to
                    // `Int(0)` above). NaN canonicalizes to one bit pattern.
                    let canon = if n.is_nan() {
                        f64::NAN.to_bits()
                    } else {
                        n.to_bits()
                    };
                    Some(MapKey::Num(canon))
                }
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
            MapKey::Int(i) => Value::Int(*i),
            MapKey::Num(bits) => Value::Float(f64::from_bits(*bits)),
            MapKey::Str(s) => Value::Str(s.clone()),
            MapKey::Decimal(d) => Value::Decimal(*d),
        }
    }
}

/// `object.freeze` (SP2 §4): if `v` is a FROZEN mutable container, return the
/// kind name for the panic message (`"array"|"object"|"map"|"set"|"instance"`);
/// otherwise `None`. Non-frozen containers and all non-container values are
/// `None` (mutation of an unfrozen container is allowed; non-containers are never
/// frozen). Used by `check_not_frozen` at every mutation site on both engines.
pub fn frozen_kind(v: &Value) -> Option<&'static str> {
    match v {
        Value::Array(a) if a.is_frozen() => Some("array"),
        Value::Object(o) if o.is_frozen() => Some("object"),
        Value::Map(m) if m.is_frozen() => Some("map"),
        Value::Set(s) if s.is_frozen() => Some("set"),
        Value::Instance(i) if i.borrow().frozen.get() => Some("instance"),
        _ => None,
    }
}

/// `object.freeze` (SP2 §4): shallow-freeze a mutable container in place. A no-op
/// for any non-container value (JS `Object.freeze` ergonomics). Idempotent /
/// one-way (no unfreeze). The caller returns `v` unchanged for chaining.
pub fn freeze_value(v: &Value) {
    match v {
        Value::Array(a) => a.freeze(),
        Value::Object(o) => o.freeze(),
        Value::Map(m) => m.freeze(),
        Value::Set(s) => s.freeze(),
        Value::Instance(i) => i.borrow().frozen.set(true),
        _ => {}
    }
}

/// `object.isFrozen` (SP2 §4): whether `v` is a frozen container. `false` for any
/// non-container value.
pub fn is_frozen_value(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.is_frozen(),
        Value::Object(o) => o.is_frozen(),
        Value::Map(m) => m.is_frozen(),
        Value::Set(s) => s.is_frozen(),
        Value::Instance(i) => i.borrow().frozen.get(),
        _ => false,
    }
}

pub struct EnumDef {
    pub name: String,
    pub variants: IndexMap<String, Value>, // each is a Value::EnumVariant
    /// ADT: per-variant payload schema (field names + declared types). A unit /
    /// scalar-backed variant has an EMPTY `VariantSchema.fields`; a payload variant
    /// (positional or named) carries its declared field list. The full ordered
    /// variant list is `variants.keys()` (== `variant_schemas.keys()`).
    pub variant_schemas: IndexMap<String, VariantSchema>,
}

/// ADT §5.1: the declared payload schema of one enum variant. An empty `fields`
/// vector means a unit / scalar-backed variant (no payload). A field's `name` is
/// `Some` for a named-field variant (`Circle(radius: float)`), `None` for a
/// positional one (`Pair(int, int)`). Field types use the NUM model.
#[derive(Clone)]
pub struct VariantSchema {
    pub fields: Vec<(Option<Rc<str>>, crate::ast::Type)>,
}

impl VariantSchema {
    /// A payload (non-unit) variant has at least one declared field.
    pub fn has_payload(&self) -> bool {
        !self.fields.is_empty()
    }

    /// `true` iff the fields are named (`Circle(radius: float)`). An empty schema
    /// (unit) is considered positional/none. Uniformity is guaranteed at parse time
    /// (all-named XOR all-positional), so checking the first field suffices.
    pub fn is_named(&self) -> bool {
        self.fields.first().map(|(n, _)| n.is_some()).unwrap_or(false)
    }
}

pub struct EnumVariant {
    pub enum_name: String,
    pub name: String,
    pub value: Value, // backing scalar (unit/scalar-backed variant), or Nil
    /// ADT §5.1: `None` for a unit variant OR an unsaturated constructor; `Some`
    /// for a CONSTRUCTED payload variant. The cycle-capable part of the value lives
    /// here (a recursive enum payload can form a cycle), so `Trace` reaches it.
    pub payload: Option<Payload>,
    /// ADT §5.1: `true` iff this is an unsaturated payload-variant CONSTRUCTOR
    /// (`Shape.Circle` referenced but not yet called). Calling it validates the
    /// payload and yields a constructed variant (`payload: Some, ctor: false`).
    pub ctor: bool,
    /// ADT: a back-reference to the owning `EnumDef`, populated ONLY on a constructor
    /// value RETURNED to user code (so a first-class `let mk = Shape.Circle` can
    /// validate the payload when called). The INTERNED map entry has `def: None`, so
    /// `EnumDef → variants → (interned ctor)` never forms an `Rc` cycle. A unit /
    /// constructed variant also has `def: None`. The constructor stays cheap (one
    /// extra `Rc` clone, only on the constructor read path).
    pub def: Option<Rc<EnumDef>>,
}

/// ADT §5.1: a constructed variant's payload data. The cycle-capable containers are
/// held behind a `Cc` (the cycle collector ONLY tracks `Cc` nodes — gcmodule's
/// `Rc<T>: Trace` is acyclic/no-op, so the `Rc<EnumVariant>` wrapper can never be a
/// cycle node; the payload's `Cc<ArrayCell>`/`Cc<ObjectCell>` IS). Positional reuses
/// `ArrayCell` (so `.value` returns a stable `Value::Array` handle — ADT §3.4);
/// named reuses `ObjectCell` (field-access sugar + stable `.value` Object share one
/// representation). Both are traced by the collector exactly as a free Array/Object.
pub enum Payload {
    Positional(Cc<ArrayCell>),
    Named(Cc<ObjectCell>),
}

pub struct Method {
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub is_async: bool,
    pub is_generator: bool,
    /// `worker fn` / `static worker fn` — Spec A: dispatched to a pooled isolate,
    /// returns `future<T>`. Tree-walker reads this on the static-method call path.
    pub is_worker: bool,
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
    /// `static fn` / `static async fn` / `static fn*` members (SP1 §3). A SEPARATE
    /// namespace from instance `methods` — an instance method and a static method
    /// may share a name (`c.x()` vs `C.x()`). Called as `C.name(args)` with no
    /// receiver; inherited up the superclass chain like instance methods.
    pub static_methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
    /// Workers Spec B: this class was declared `worker class`. A `worker class` is
    /// spawned into a dedicated isolate via `ClassName.spawn(args)` (returns
    /// `future<handle>`); a bare `ClassName(args)` still builds a LOCAL instance.
    /// Set from the AST/CST `is_worker` flag on both engines.
    pub is_worker: bool,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: IndexMap<String, Value>,
    /// The instance's key-layout id (V11-T2 hidden classes). Defaults to `0`
    /// (unset); the tree-walker leaves it at `0`, the VM assigns the class's base
    /// shape (and transitions it if a field is added). `Cell` so a `&self` VM
    /// method can update it without a mutable instance borrow.
    pub shape_id: Cell<u32>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. `Cell` so a `&self`
    /// engine can set/read it without a mutable instance borrow; see
    /// [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
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

/// IFACE §4: a structural interface — an immutable, acyclic conformance descriptor
/// naming a method set. An interface name resolves to a `Value::Interface(Rc<InterfaceDef>)`.
/// It is never a receiver, has no vtable, holds no `Value`/`Cc` edges, and its GC
/// `Trace` is a no-op (like `Regex`/`Native`). Identity-equal (`Rc::ptr_eq`).
pub struct InterfaceDef {
    pub name: String,
    /// This interface's OWN requirements (the body's `fn` signatures), keyed by name.
    pub own_methods: IndexMap<String, MethodReq>,
    /// The names of the interfaces this one `extends` (composition). Stored as NAMES,
    /// resolved LAZILY (interfaces forward-reference as late-bound module-globals) —
    /// NOT pre-flattened at declaration time (IFACE §4, C4).
    pub extends: Vec<String>,
    /// MEMOIZED flattened method set (own + every transitively-extended interface's),
    /// deduplicated by name. `None` until the first `conforms`/contract check; filled
    /// on first use via the engine's `flatten()` lazy builder, then reused. Never
    /// invalidated within a run (descriptors are load-time-immortal, IFACE §5.3).
    pub flat: RefCell<Option<Rc<IndexMap<String, MethodReq>>>>,
}

/// IFACE §4: a single required method on an interface — name keys it in the map, this
/// carries the call-shape. v1 is arity-only (type-erased, runtime-permissive); TYPE
/// later adds param/ret `CheckTy` slots here for the strict static check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodReq {
    /// The number of parameters the interface signature declares.
    pub arity: usize,
    /// Whether the requirement itself declares a rest param (`...xs`) — only then must
    /// the conforming method also be variadic (IFACE §5.1).
    pub has_rest: bool,
    // TYPE later adds param/ret CheckTy signatures here.
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
    // SP5 §6 std/postgres: an async Postgres connection (feature `postgres`).
    // Methods: query/queryOne/exec/begin/commit/rollback/close.
    PostgresConnection,
    // SP5 §6 std/redis: an async Redis connection (feature `redis`).
    // Methods: command/get/set/del/incr/expire/exists/close.
    RedisConnection,
    // SP5 §7 std/lru: a bounded LRU cache (core). Methods: get/set/has/delete/
    // clear/len/keys.
    Lru,
    // SP5 §7 std/events: an event-emitter (core). Methods: on/once/off/emit/
    // listenerCount.
    Events,
    // SP12 std/telemetry: a tracing span handle. Methods: setAttribute/addEvent/
    // setStatus/end. Inert (no-op) before telemetry.init. Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetrySpan,
    // SP12 std/telemetry: a metric instrument handle (counter/histogram/gauge).
    // Methods: add (counter), record (histogram), set (gauge). Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetryInstrument,
    // SP12 std/telemetry: an INERT handle returned when telemetry is not
    // initialized — every method is a no-op. Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetryNoop,
    // SP11 std/ai: a provider handle (`ai.provider(kind, config)`). Pure config in
    // `fields` (kind/baseUrl/apiKey/apiVersion/headers) — no OS resource. Method:
    // `.model(id)` → an AiModel handle. Feature `ai`.
    #[cfg(feature = "ai")]
    AiProvider,
    // SP11 std/ai: a model handle (`provider.model(id)`). Carries the resolved
    // provider config + model name in `fields`; consumed by ai.generate/stream/embed
    // as the `model:` argument. Feature `ai`.
    #[cfg(feature = "ai")]
    AiModel,
    // SP11 std/ai: a streaming chat handle (`ai.stream(...)`). Backed by an
    // `AiStream` resource; methods `next()`/`textOnly()`/`result()`, consumable by
    // `for await`. Feature `ai`.
    #[cfg(feature = "ai")]
    AiStream,
    // SP11 std/ai: a text-only streaming adapter (`stream.textOnly()`), yielding bare
    // text strings; shares the underlying `AiStream` resource. Feature `ai`.
    #[cfg(feature = "ai")]
    AiTextStream,
    // SP11 std/ai: a tool definition (`ai.tool({description, input, execute})`).
    // Carries description/input-schema/execute fn in `fields`; consumed by
    // ai.generate's `tools:` map. Feature `ai`.
    #[cfg(feature = "ai")]
    AiTool,
    // Workers Spec B §Task 5: a `worker class` ACTOR proxy handle. The actor
    // instance lives in a dedicated isolate; this handle's method calls become FIFO
    // mailbox messages over a `Send` channel. Backed by `ResourceState::WorkerActor`
    // (the outbound sender + the `IsolateHandle`, whose `Drop` tears the isolate
    // down). Not feature-gated — `worker` is core syntax. Readable field: the
    // declared class `name`.
    WorkerActor,
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
            NativeKind::PostgresConnection => "postgresConnection",
            NativeKind::RedisConnection => "redisConnection",
            NativeKind::Lru => "lru",
            NativeKind::Events => "emitter",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetrySpan => "span",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetryInstrument => "instrument",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetryNoop => "telemetryNoop",
            #[cfg(feature = "ai")]
            NativeKind::AiProvider => "aiProvider",
            #[cfg(feature = "ai")]
            NativeKind::AiModel => "aiModel",
            #[cfg(feature = "ai")]
            NativeKind::AiStream => "aiStream",
            #[cfg(feature = "ai")]
            NativeKind::AiTextStream => "aiTextStream",
            #[cfg(feature = "ai")]
            NativeKind::AiTool => "aiTool",
            NativeKind::WorkerActor => "workerActor",
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

/// `x instanceof class` (SP2 §1): `true` iff `v` is a `Value::Instance` whose class
/// is `class` or a subclass of it. Walks the `superclass` chain by `Rc::as_ptr`
/// identity — the same identity `find_method`/`super` use. Any non-`Instance` `v`
/// (number, string, object, nil, enum, …) is `false`, never an error. Single source
/// of truth shared by the tree-walker (`apply_binop`) and the VM (`Op::InstanceOf`).
pub(crate) fn is_instance_of(v: &Value, class: &Rc<Class>) -> bool {
    let Value::Instance(inst) = v else {
        return false;
    };
    let target = Rc::as_ptr(class);
    let mut cur = Some(inst.borrow().class.clone());
    while let Some(c) = cur {
        if Rc::as_ptr(&c) == target {
            return true;
        }
        cur = c.superclass.clone();
    }
    false
}

/// Walk a class chain for a STATIC method (SP1 §3), returning it plus the class
/// that defined it. Mirrors `find_method` but over the `static_methods` namespace
/// so a subclass resolves an unknown static up its superclass chain.
pub fn find_static_method(class: &Rc<Class>, name: &str) -> Option<(Rc<Method>, Rc<Class>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(m) = c.static_methods.get(name) {
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
    /// `worker fn` — Spec A: dispatched to a pooled isolate, returns `future<T>`.
    /// The tree-walker reads this in `call_function` to route to the worker pool.
    pub is_worker: bool,
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    /// A 64-bit signed integer (NUM §3.1). The exact-arithmetic default subtype of
    /// `number`; literals without a fractional part or exponent lex to `Int`.
    Int(i64),
    /// A 64-bit IEEE-754 float (NUM §3.1). The fractional subtype of `number`;
    /// literals with a `.` or exponent lex to `Float`. (Formerly `Number(f64)`.)
    Float(f64),
    /// Exact decimal arithmetic (96-bit scaled integer via `rust_decimal`).
    /// `Copy` — no heap allocation; `Hash + Eq + Ord` via the inner type.
    /// Participates in operator overloading with `Int`/`Float` via coercion.
    Decimal(Decimal),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
    /// A user-defined function carrying its closure environment.
    Function(Rc<Function>),
    /// A bytecode-VM closure: a function prototype plus its captured upvalue
    /// cells. Behaves like `Function` to the user (same `type()`/display);
    /// identity equality. Produced by the VM (V4+); inert in the tree-walker.
    Closure(Cc<crate::vm::value_ext::Closure>),
    Array(Cc<ArrayCell>),
    Object(Cc<ObjectCell>),
    // IndexMap (not HashMap) is deliberate: insertion order is required for
    // deterministic keys/values/entries/display and to match `Object`.
    Map(Cc<MapCell>),
    /// An insertion-ordered hash set of hashable values (spec §11.2).
    /// Elements use the same `MapKey` type as Map keys.
    /// Identity equality (like Array/Map/Bytes).
    Set(Cc<SetCell>),
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
    /// IFACE §4: a structural interface — an immutable, acyclic conformance descriptor
    /// (`Rc<InterfaceDef>`) naming a method set. Identity-equal like `Class`; the RHS
    /// of `instanceof Reader`, the resolved target of a `Reader` annotation. No vtable,
    /// no GC edges (no-op `Trace`).
    Interface(Rc<InterfaceDef>),
    Instance(Cc<RefCell<Instance>>),
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
    /// A class associated function bound to its class: either the built-in typed
    /// parser `User.from` or a USER static method `User.create` (SP1 §3). The name
    /// is an `Rc<str>` (not `&'static`) so it can carry an arbitrary user static
    /// name; `call_value` resolves it against `static_methods` (chain-walked),
    /// then the built-in `from`.
    ClassMethod(Rc<Class>, Rc<str>),
}

impl Value {
    /// NUM §3.3 (BREAKING): the resolved falsy set is `nil`, `false`, `Int(0)`,
    /// a `Float` that is `0.0`/`-0.0`/`NaN`, a `Decimal` equal to zero, and the
    /// empty string `""`. EVERYTHING else is truthy — including non-empty strings
    /// and ALL collections/objects/instances even when empty.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            // `0.0 == -0.0` is `true`, so the `!= 0.0` test covers both signed zeros;
            // NaN is excluded explicitly (`!is_nan()`) → `0.0`/`-0.0`/`NaN` all falsy.
            Value::Float(f) => *f != 0.0 && !f.is_nan(),
            Value::Decimal(d) => *d != Decimal::ZERO,
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// NUM: central numeric extraction. Returns the `f64` value of any number kind
    /// (`Int` is widened via `i as f64`, `Float` returned as-is). `None` for every
    /// non-number. This is the single helper every "accepts a number" site should
    /// route through so `Int` is first-class everywhere a number was accepted.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// `true` for any number kind (`Int` or `Float`).
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }

    /// `true` only for `Value::Int`. Used by range lowering to decide whether a
    /// range yields an `Int` sequence (both bounds + step `Int`) or a `Float` one.
    pub fn is_int_value(&self) -> bool {
        matches!(self, Value::Int(_))
    }

    /// NUM: exact integer extraction for integral contexts (indexing, range bounds,
    /// counts, repeat). `Int(i)` yields `i` directly. A `Float` yields `Some` ONLY
    /// when it is finite and integral and within `i64` range; a non-integral or
    /// out-of-range `Float` yields `None` (callers turn that into a Tier-2 panic
    /// such as `array index must be an int, got float`). Non-numbers yield `None`.
    pub fn as_int_exact(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => {
                if f.is_finite()
                    && f.fract() == 0.0
                    && *f >= i64::MIN as f64
                    && *f <= i64::MAX as f64
                {
                    Some(*f as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Exact `int`-vs-`float` equality (NUM §3.3): `true` iff `i` and `f` denote the
/// same mathematical value. Avoids the lossy `i as f64` round-trip — a non-finite
/// or non-integral `f`, or one outside i64 range, is never equal to any `int`; an
/// integral in-range `f` equals `i` iff `f as i64 == i`.
fn int_eq_float(i: i64, f: f64) -> bool {
    f.is_finite()
        && f.fract() == 0.0
        && f >= i64::MIN as f64
        && f <= i64::MAX as f64
        && f as i64 == i
}

/// Exact `int`-vs-`float` ordering (NUM §3.3): returns `Some(Ordering)` for the
/// mathematical comparison of `i` and `f`, or `None` iff `f` is `NaN` (which is
/// unordered, exactly like IEEE-754). The comparison is **exact** — it never casts
/// `i as f64` (which would lose precision past 2^53). Strategy: if `f` is integral
/// and within i64 range, compare as integers; otherwise compare `i as f64` vs `f`
/// — but bias by the fractional part / out-of-range magnitude so no precision is
/// lost at the boundary.
pub(crate) fn int_cmp_float(i: i64, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    if f == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if f == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // `f` is finite. If it is below the i64 range, every i64 is greater; above the
    // range, every i64 is smaller. The bounds `i64::MIN as f64` (= -2^63, exact)
    // and `i64::MAX as f64` (= 2^63, since 2^63-1 rounds up) frame the range:
    // `f < -2^63` ⇒ i > f; `f >= 2^63` ⇒ i < f (no i64 reaches 2^63).
    if f < i64::MIN as f64 {
        return Some(Ordering::Greater);
    }
    if f >= -(i64::MIN as f64) {
        // -(i64::MIN as f64) == 2^63; no i64 is >= 2^63.
        return Some(Ordering::Less);
    }
    // Now `-2^63 <= f < 2^63`, so `f.trunc()` fits in i64 exactly.
    let trunc = f.trunc() as i64;
    match i.cmp(&trunc) {
        // Same integer part: the fraction decides. `f.fract()` is in (-1, 1); a
        // positive fraction makes `f` larger than its truncation, a negative one
        // smaller. `i == trunc` so compare against the fraction's sign.
        Ordering::Equal => {
            let frac = f.fract();
            if frac > 0.0 {
                Some(Ordering::Less) // i == trunc < f
            } else if frac < 0.0 {
                Some(Ordering::Greater) // i == trunc > f
            } else {
                Some(Ordering::Equal)
            }
        }
        other => Some(other),
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // Cross-subtype numeric equality is EXACT (NUM §3.3): an `int` equals a
            // `float` iff they are mathematically equal — no lossy `i as f64` cast
            // (which would make `2**53+1 == float(2**53)`). Symmetric.
            (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => {
                int_eq_float(*i, *f)
            }
            // Decimal: same-type value equality by the Decimal's own PartialEq.
            // Cross-type Number↔Decimal equality is handled in the evaluator's
            // Eq/Ne path, not here.
            (Value::Decimal(a), Value::Decimal(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            // Functions compare by identity.
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Closure(a), Value::Closure(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),
            #[cfg(feature = "data")]
            (Value::Regex(a), Value::Regex(b)) => Rc::ptr_eq(a, b),
            // Native handles and bound native methods compare by identity.
            (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
            (Value::NativeMethod(a), Value::NativeMethod(b)) => Rc::ptr_eq(a, b),
            // Enums and their (interned) variants compare by identity.
            (Value::Enum(a), Value::Enum(b)) => Rc::ptr_eq(a, b),
            // ADT §5.2: unit / constructor variants compare by interned IDENTITY
            // (byte-identical to pre-ADT). A CONSTRUCTED payload variant compares
            // STRUCTURALLY: same enum, same variant name, payloads equal element-wise
            // (positional) or key-wise (named, via the existing Object `PartialEq`).
            (Value::EnumVariant(a), Value::EnumVariant(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                match (&a.payload, &b.payload) {
                    // At least one is a payload variant → structural compare. (A
                    // payload variant is never `==` a unit/constructor of the same
                    // name: a unit's `payload` is `None`, so the arms below short out.)
                    (Some(pa), Some(pb)) => {
                        a.enum_name == b.enum_name
                            && a.name == b.name
                            && match (pa, pb) {
                                (Payload::Positional(xa), Payload::Positional(xb)) => {
                                    *xa.borrow() == *xb.borrow()
                                }
                                (Payload::Named(oa), Payload::Named(ob)) => {
                                    *oa.borrow() == *ob.borrow()
                                }
                                _ => false,
                            }
                    }
                    // Both unit/constructor but distinct `Rc`s → not equal (interned,
                    // so identity is the only equality; a re-interning failure across
                    // a worker boundary is handled by §6 re-interning, not here).
                    _ => false,
                }
            }
            // Classes/instances/bound-methods/super compare by identity.
            (Value::Class(a), Value::Class(b)) => Rc::ptr_eq(a, b),
            // Interfaces compare by identity (immutable descriptors, IFACE §4).
            (Value::Interface(a), Value::Interface(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => crate::gc::cc_ptr_eq(a, b),
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
            Value::Int(i) => write!(f, "Int({})", i),
            Value::Float(n) => write!(f, "Float({})", n),
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
            Value::EnumVariant(v) => match &v.payload {
                None => write!(f, "EnumVariant({}.{})", v.enum_name, v.name),
                Some(_) => write!(f, "EnumVariant({}.{}(..))", v.enum_name, v.name),
            },
            Value::Class(c) => write!(f, "Class({})", c.name),
            Value::Interface(i) => write!(f, "Interface({})", i.name),
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

/// NUM §4: render a `float` (`f64`) the way AScript prints/`str()`s it. Unlike
/// Rust's `f64` Display (which prints `7.0` as `"7"`), a `float` ALWAYS shows at
/// least one fractional digit so it is visually distinguishable from an `int`
/// (the Python/Swift convention): `5.0`, `1500.0`, `-0.0`. `inf`/`-inf`/`nan`
/// pass through Rust's Display unchanged. This is the single shared spelling so
/// the tree-walker and the VM (and every str()/print/template path that routes
/// through `Value::Float` Display) agree byte-for-byte.
pub fn format_float(n: f64) -> String {
    if n.is_finite() {
        if n.fract() == 0.0 {
            // Integral finite float: append `.0`. `{}` on `-0.0` yields `-0`, so
            // the `.0` suffix gives `-0.0` / `0.0` / `7.0` uniformly.
            format!("{n}.0")
        } else {
            format!("{n}")
        }
    } else {
        // inf / -inf / NaN: unchanged ("inf", "-inf", "NaN").
        format!("{n}")
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
            Value::Int(i) => write!(f, "{}", i),
            // NUM §4: a `float` always shows a decimal (`5.0`, not `5`) so it is
            // distinguishable from an `int`. See `format_float`.
            Value::Float(n) => write!(f, "{}", format_float(*n)),
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
                let ptr = crate::gc::cc_addr(a);
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
                let ptr = crate::gc::cc_addr(o);
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
                let ptr = crate::gc::cc_addr(m);
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
                let ptr = crate::gc::cc_addr(s);
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
            Value::EnumVariant(v) => match &v.payload {
                // Unit / scalar-backed / constructor: byte-identical to pre-ADT.
                None => write!(f, "{}.{}", v.enum_name, v.name),
                // ADT: a constructed payload variant renders as `Enum.Variant(a, b)`
                // (positional) or `Enum.Variant(name: v, ...)` (named). Cycle-guarded
                // via the shared `seen` set (a recursive payload can self-reference).
                Some(Payload::Positional(a)) => {
                    let ptr = crate::gc::cc_addr(a);
                    if seen.contains(&ptr) {
                        return write!(f, "{}.{}(...)", v.enum_name, v.name);
                    }
                    seen.push(ptr);
                    write!(f, "{}.{}(", v.enum_name, v.name)?;
                    for (i, it) in a.borrow().iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        it.write_element(f, seen)?;
                    }
                    write!(f, ")")?;
                    seen.pop();
                    Ok(())
                }
                Some(Payload::Named(o)) => {
                    let ptr = crate::gc::cc_addr(o);
                    if seen.contains(&ptr) {
                        return write!(f, "{}.{}(...)", v.enum_name, v.name);
                    }
                    seen.push(ptr);
                    write!(f, "{}.{}(", v.enum_name, v.name)?;
                    for (i, (k, val)) in o.borrow().iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}: ", k)?;
                        val.write_element(f, seen)?;
                    }
                    write!(f, ")")?;
                    seen.pop();
                    Ok(())
                }
            },
            Value::Class(c) => write!(f, "<class {}>", c.name),
            Value::Interface(i) => write!(f, "<interface {}>", i.name),
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

    // ADT Task 1 helpers — construct variant values directly at the value layer.
    fn unit_variant(en: &str, name: &str, backing: Value) -> Value {
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: backing,
            payload: None,
            ctor: false,
        def: None,
        }))
    }
    fn pos_variant(en: &str, name: &str, items: Vec<Value>) -> Value {
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(Payload::Positional(ArrayCell::new(items))),
            ctor: false,
        def: None,
        }))
    }
    fn named_variant(en: &str, name: &str, fields: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in fields {
            m.insert(k.to_string(), v);
        }
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(Payload::Named(ObjectCell::new(m))),
            ctor: false,
        def: None,
        }))
    }

    #[test]
    fn adt_unit_variant_is_byte_identical_to_pre_adt() {
        // A `payload: None, ctor: false` unit variant: `.value` is the backing scalar
        // (or Nil), it is truthy, and two DISTINCT `Rc`s of the same name are NOT
        // equal (identity equality, as pre-ADT — interning makes real uses equal).
        let red = unit_variant("Color", "Red", Value::Nil);
        let green = unit_variant("Color", "Green", Value::Int(2));
        assert!(red.is_truthy());
        assert!(green.is_truthy());
        // Distinct allocations of the same unit variant are NOT `==` (identity).
        let red2 = unit_variant("Color", "Red", Value::Nil);
        assert_ne!(red, red2);
        // But cloning the SAME `Rc` is equal (the interned-use case).
        assert_eq!(red.clone(), red);
    }

    #[test]
    fn adt_constructed_variants_compare_structurally() {
        // Positional: `Pair(3, 4) == Pair(3, 4)`, `!= Pair(3, 5)`.
        let p1 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        let p2 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        let p3 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(5)]);
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        // Named: `Circle(radius: 2.0) == Circle(radius: 2.0)`, `!= Circle(radius: 3.0)`.
        let c1 = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        let c2 = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        let c3 = named_variant("Shape", "Circle", vec![("radius", Value::Float(3.0))]);
        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
        // A payload variant is never equal to a unit variant of the same name.
        let unit_circle = unit_variant("Shape", "Circle", Value::Nil);
        assert_ne!(c1, unit_circle);
        // Different variant names with equal payload are not equal.
        let other = pos_variant("Shape", "Other", vec![Value::Int(3), Value::Int(4)]);
        assert_ne!(p1, other);
        // Constructed payload variants are truthy.
        assert!(p1.is_truthy());
        assert!(c1.is_truthy());
    }

    #[test]
    fn adt_constructed_variant_display() {
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        assert_eq!(pair.to_string(), "Shape.Pair(3, 4)");
        let circle = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        assert_eq!(circle.to_string(), "Shape.Circle(radius: 2.0)");
        // Unit variant display is unchanged: `Enum.Variant`.
        let red = unit_variant("Color", "Red", Value::Nil);
        assert_eq!(red.to_string(), "Color.Red");
        // Nested string payload quotes the inner string (write_element).
        let str_v = pos_variant("Json", "Str", vec![Value::Str("hi".into())]);
        assert_eq!(str_v.to_string(), "Json.Str(\"hi\")");
    }

    #[test]
    fn adt_payload_variant_is_not_a_map_key() {
        // Payload variants are identity-style containers (like Array/Map): NOT
        // hashable as a `MapKey`. Unit variants were never hashable either (today's
        // behavior is preserved — both return `None`).
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        assert!(MapKey::from_value(&pair).is_none());
        let red = unit_variant("Color", "Red", Value::Nil);
        assert!(MapKey::from_value(&red).is_none());
    }

    #[test]
    fn adt_type_name_unchanged_for_payload_variant() {
        // The runtime `type_name` for any EnumVariant stays "enum variant" (the
        // wildcard arm). Asserted at the interp layer; here we assert the value-layer
        // Debug differentiates payload vs unit (used in panics/tests only).
        let red = unit_variant("Color", "Red", Value::Nil);
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(format!("{:?}", red), "EnumVariant(Color.Red)");
        assert_eq!(format!("{:?}", pair), "EnumVariant(Shape.Pair(..))");
    }

    #[test]
    fn displays_values_like_a_script_language() {
        // NUM §4: a `float` always renders with at least one fractional digit so it
        // is visually distinguishable from an `int` (Python/Swift convention).
        assert_eq!(Value::Float(7.0).to_string(), "7.0");
        assert_eq!(Value::Float(2.5).to_string(), "2.5");
        assert_eq!(Value::Float(1500.0).to_string(), "1500.0");
        assert_eq!(Value::Float(-0.0).to_string(), "-0.0");
        assert_eq!(Value::Float(0.0).to_string(), "0.0");
        assert_eq!(Value::Float(f64::INFINITY).to_string(), "inf");
        assert_eq!(Value::Float(f64::NEG_INFINITY).to_string(), "-inf");
        assert_eq!(Value::Float(f64::NAN).to_string(), "NaN");
        // `int` keeps NO decimal.
        assert_eq!(Value::Int(5).to_string(), "5");
        assert_eq!(Value::Int(-7).to_string(), "-7");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Nil.to_string(), "nil");
        assert_eq!(Value::Str("hi".into()).to_string(), "hi");
    }

    #[test]
    fn float_in_collections_keeps_decimal() {
        let arr = Value::Array(crate::value::ArrayCell::new(vec![
            Value::Float(1.0),
            Value::Float(2.0),
        ]));
        assert_eq!(arr.to_string(), "[1.0, 2.0]");
    }

    #[test]
    fn truthiness_follows_spec() {
        // NUM: falsy = nil, false, 0 (int), 0.0/-0.0/NaN (float), 0 decimal, "" (string).
        // Everything else — incl. non-empty strings and all collections even when empty — is truthy.
        assert!(Value::Bool(true).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Float(-0.0).is_truthy());
        assert!(!Value::Float(f64::NAN).is_truthy());
        assert!(Value::Float(0.5).is_truthy());
        assert!(!Value::Str("".into()).is_truthy());
        assert!(Value::Str("x".into()).is_truthy());
    }

    #[test]
    fn equality_is_structural_and_cross_kind_is_false() {
        assert_eq!(Value::Float(1.0), Value::Float(1.0));
        assert_eq!(Value::Str("a".into()), Value::Str("a".into()));
        assert_ne!(Value::Float(1.0), Value::Str("1".into()));
        assert_ne!(Value::Bool(true), Value::Float(1.0));
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
        

        let a = Value::Array(crate::value::ArrayCell::new(vec![
            Value::Float(1.0),
            Value::Str("two".into()),
        ]));
        assert_eq!(a.to_string(), "[1.0, \"two\"]");
        // identity: a clone of the SAME Rc is equal; a fresh array is not
        assert_eq!(a.clone(), a);
        let b = Value::Array(crate::value::ArrayCell::new(vec![Value::Float(1.0)]));
        assert_ne!(a, b);
        assert!(a.is_truthy());
    }

    #[test]
    fn maps_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert(MapKey::Str("a".into()), Value::Float(1.0));
        m.insert(MapKey::Num(0.0f64.to_bits()), Value::Str("zero".into()));
        let map = Value::Map(crate::value::MapCell::new(m));
        assert_eq!(map.to_string(), "map {\"a\": 1.0, 0.0: \"zero\"}");
        assert_eq!(map.clone(), map);
        assert!(map.is_truthy());
        assert!(MapKey::from_value(&Value::Float(0.0)).is_some());
        assert!(
            MapKey::from_value(&Value::Array(crate::value::ArrayCell::new(vec![]))).is_none()
        );
    }

    #[test]
    fn mapkey_number_and_decimal_are_distinct() {
        use rust_decimal::Decimal;
        // Number 1 and Decimal 1 must produce DIFFERENT map keys, so they index
        // distinct slots in a Map/Set. This pins the MapKey::Decimal claim directly.
        // (MapKey intentionally has no Debug derive, so compare via bool to avoid
        // requiring it in assert_eq!/assert_ne!.)
        let num_key = MapKey::from_value(&Value::Float(1.0)).expect("number is hashable");
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

    // ---- IFACE Task 1: Value::Interface descriptor ----

    fn iface(name: &str) -> Rc<InterfaceDef> {
        Rc::new(InterfaceDef {
            name: name.to_string(),
            own_methods: IndexMap::new(),
            extends: Vec::new(),
            flat: RefCell::new(None),
        })
    }

    #[test]
    fn iface_value_basics() {
        let r = iface("Reader");
        let v = Value::Interface(r.clone());
        // type_name → "interface"
        assert_eq!(crate::interp::type_name(&v), "interface");
        // truthy (a descriptor is truthy)
        assert!(v.is_truthy());
        // Display → "<interface Reader>" (mirrors "<class Foo>")
        assert_eq!(format!("{}", v), "<interface Reader>");
        // same Rc → equal (identity)
        assert_eq!(v.clone(), v);
        assert_eq!(Value::Interface(r.clone()), Value::Interface(r));
        // two distinct Rcs of the same name → NOT equal (identity, not structural)
        assert_ne!(Value::Interface(iface("Reader")), Value::Interface(iface("Reader")));
    }

    // ---- NUM Task 1: int subtype, truthiness, MapKey fold, cross-subtype eq ----

    #[test]
    fn num_type_names_distinguish_int_and_float() {
        assert_eq!(crate::interp::type_name(&Value::Int(5)), "int");
        assert_eq!(crate::interp::type_name(&Value::Float(5.0)), "float");
        // Decimal is its own subtype, unchanged.
        assert_eq!(
            crate::interp::type_name(&Value::Decimal(Decimal::from(1))),
            "decimal"
        );
    }

    #[test]
    fn num_int_cmp_float_is_exact_at_boundaries() {
        use std::cmp::Ordering;
        // Trivial integral cases.
        assert_eq!(int_cmp_float(2, 2.5), Some(Ordering::Less));
        assert_eq!(int_cmp_float(3, 2.5), Some(Ordering::Greater));
        assert_eq!(int_cmp_float(2, 2.0), Some(Ordering::Equal));
        // NaN is unordered.
        assert_eq!(int_cmp_float(1, f64::NAN), None);
        // Infinities.
        assert_eq!(int_cmp_float(i64::MAX, f64::INFINITY), Some(Ordering::Less));
        assert_eq!(
            int_cmp_float(i64::MIN, f64::NEG_INFINITY),
            Some(Ordering::Greater)
        );
        // 2^53 boundary: 2^53+1 (exact i64) vs 2^53.0 — the int is strictly
        // greater, despite (2^53+1) as f64 rounding back to 2^53.
        let two53_plus1 = (1i64 << 53) + 1;
        let two53_f = (1u64 << 53) as f64;
        assert_eq!(int_cmp_float(two53_plus1, two53_f), Some(Ordering::Greater));
        assert!(int_eq_float(1i64 << 53, two53_f)); // exactly equal at 2^53
        assert!(!int_eq_float(two53_plus1, two53_f)); // 2^53+1 != 2^53.0
        // Far out-of-range floats: every i64 is below 1e300 and above -1e300.
        assert_eq!(int_cmp_float(i64::MAX, 1e300), Some(Ordering::Less));
        assert_eq!(int_cmp_float(i64::MIN, -1e300), Some(Ordering::Greater));
        // Negative fractional near an int.
        assert_eq!(int_cmp_float(-3, -3.5), Some(Ordering::Greater));
        assert_eq!(int_cmp_float(-4, -3.5), Some(Ordering::Less));
    }

    #[test]
    fn num_int_displays_without_a_decimal_point() {
        assert_eq!(Value::Int(5).to_string(), "5");
        assert_eq!(Value::Int(-42).to_string(), "-42");
        assert_eq!(Value::Int(0).to_string(), "0");
        // Debug carries the subtype tag.
        assert_eq!(format!("{:?}", Value::Int(7)), "Int(7)");
        assert_eq!(format!("{:?}", Value::Float(7.0)), "Float(7)");
    }

    #[test]
    fn num_truthiness_resolved_falsy_set() {
        // Falsy: nil, false, Int(0), 0.0/-0.0/NaN, 0m, "".
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Float(-0.0).is_truthy());
        assert!(!Value::Float(f64::NAN).is_truthy());
        assert!(!Value::Decimal(Decimal::ZERO).is_truthy());
        assert!(!Value::Str("".into()).is_truthy());
        // Truthy: any non-zero number, non-empty string, EVERY collection even empty.
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(Value::Int(-1).is_truthy());
        assert!(Value::Float(0.5).is_truthy());
        assert!(Value::Float(f64::INFINITY).is_truthy());
        assert!(Value::Decimal(Decimal::from(1)).is_truthy());
        assert!(Value::Str("x".into()).is_truthy());
        assert!(Value::Array(crate::value::ArrayCell::new(vec![])).is_truthy());
        {
            use indexmap::IndexMap;
            assert!(Value::Map(crate::value::MapCell::new(IndexMap::new())).is_truthy());
            assert!(Value::Object(crate::value::ObjectCell::new(IndexMap::new())).is_truthy());
        }
    }

    #[test]
    fn num_mapkey_folds_integral_float_to_int() {
        // §3.3: an integral, in-range float is the SAME map key as the equal int.
        let from_int = MapKey::from_value(&Value::Int(1)).expect("int is hashable");
        let from_float = MapKey::from_value(&Value::Float(1.0)).expect("float is hashable");
        assert!(from_int == from_float, "Int(1) and Float(1.0) must share a key");
        // -0.0 folds to Int(0) and equals Int(0)/0.0.
        let neg_zero = MapKey::from_value(&Value::Float(-0.0)).expect("float is hashable");
        let pos_zero = MapKey::from_value(&Value::Float(0.0)).expect("float is hashable");
        let int_zero = MapKey::from_value(&Value::Int(0)).expect("int is hashable");
        assert!(neg_zero == pos_zero && pos_zero == int_zero);
        // A fractional float is a distinct (non-Int) key.
        let frac = MapKey::from_value(&Value::Float(1.5)).expect("float is hashable");
        assert!(frac != from_int);
        // Round-trips: Int key -> Value::Int.
        assert_eq!(from_int.to_value(), Value::Int(1));
    }

    #[test]
    fn num_mapkey_nan_carveout() {
        // §3.3: NaN is excluded from the "a==b ⟺ same key" claim. NaN keys
        // canonicalize to ONE storable key, but never equal a non-NaN key, and a
        // NaN float is NOT folded to any Int.
        let nan1 = MapKey::from_value(&Value::Float(f64::NAN)).expect("nan is hashable");
        let nan2 = MapKey::from_value(&Value::Float(f64::NAN)).expect("nan is hashable");
        // Two NaN keys canonicalize identically (storable/retrievable as one key).
        assert!(nan1 == nan2);
        // A NaN key never collides with any integer key (incl. 0).
        let zero = MapKey::from_value(&Value::Int(0)).expect("int is hashable");
        assert!(nan1 != zero);
        // The canonical NaN key is a `Num` (float) key, not an `Int` fold.
        assert!(matches!(nan1, MapKey::Num(_)));
    }

    #[test]
    fn num_cross_subtype_equality_is_exact() {
        // Int(1) == Float(1.0), symmetric.
        assert_eq!(Value::Int(1), Value::Float(1.0));
        assert_eq!(Value::Float(1.0), Value::Int(1));
        assert_eq!(Value::Int(0), Value::Float(-0.0));
        // Non-integral float is never equal to an int.
        assert_ne!(Value::Int(2), Value::Float(2.5));
        assert_ne!(Value::Float(2.5), Value::Int(2));
        // Exact (not lossy): 2^53+1 as int does NOT equal float(2^53) which rounds.
        let big = (1i64 << 53) + 1;
        assert_ne!(Value::Int(big), Value::Float(big as f64));
        // NaN/inf floats never equal any int.
        assert_ne!(Value::Int(0), Value::Float(f64::NAN));
        assert_ne!(Value::Int(0), Value::Float(f64::INFINITY));
        // Same-subtype equality still holds.
        assert_eq!(Value::Int(7), Value::Int(7));
        assert_ne!(Value::Int(7), Value::Int(8));
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
            is_worker: false,
            owning_class: None,
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
            is_worker: false,
            owning_class: None,
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
            is_worker: false,
        })
    }

    #[test]
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Float(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(ObjectCell::new(m));
        assert_eq!(o.to_string(), "{a: 1.0, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }
}
