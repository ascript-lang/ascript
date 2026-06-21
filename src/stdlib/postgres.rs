//! `std/postgres` — async PostgreSQL client (feature `postgres`, backed by
//! `tokio-postgres`).
//!
//! Follows the native-resource pattern (like `std/sqlite`) but over a NETWORK
//! connection on the `!Send` current-thread runtime:
//!
//! - `await postgres.connect(url) -> [conn, err]` — `tokio_postgres::connect` then
//!   `spawn_local` the driver `Connection` future (it drives the wire protocol);
//!   the `Client` + the driver task's `AbortHandle` are stored in
//!   `ResourceState::PostgresConnection`. A bad URL / unreachable server → Tier-1 err.
//! - Connection methods (all async, all Tier-1):
//!   `query(sql, params?) -> [array<rowObject>, err]`,
//!   `queryOne(sql, params?) -> [rowObject | nil, err]`,
//!   `exec(sql, params?) -> [affectedRows, err]`,
//!   `begin()/commit()/rollback() -> [nil, err]`,
//!   `close() -> nil` (aborts the driver task, drops the client).
//!   `query(sql, params, Class) -> [array<Instance>, err]` — typed rows.
//!
//! ## `!Send` / borrow discipline
//! The driver future is spawned with `spawn_local` (NOT `tokio::spawn`). Methods
//! use the take-out-across-await pattern: `take_resource` → await on the owned
//! `Client` → `return_resource`. No `resources`/`RefCell` borrow is ever held
//! across an `.await` (enforced by clippy `await_holding_refcell_ref`).
//!
//! ## Type map (Postgres → Value)
//! bool→Bool; int2/int4/int8→Number; float4/float8→Number; **numeric→Str** (to
//! avoid f64 precision loss); text/varchar/name/char→Str; bytea→Bytes; json/jsonb
//! →decoded Value; uuid→Str; timestamp(tz)/date/time→Str (ISO-ish text); null→Nil.
//! An unmapped type falls back to its text representation when available, else Nil.
//!
//! Bind params (Value → Postgres) cover Nil/Bool/Number/Str/Bytes; other kinds are
//! a Tier-2 arg-type misuse.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use std::rc::Rc;
use tokio_postgres::types::{ToSql, Type};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("connect", bi("postgres.connect"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

/// The outcome of a Postgres op-run helper: a successful payload, a Tier-1 DB error
/// message, or a RESIL §5.4 deadline-budget exhaustion (carried distinctly so it
/// surfaces the canonical `deadline-exceeded` pair, not a plain `{message}` err).
enum PgOutcome<T> {
    Ok(T),
    Err(String),
    Deadline,
}

impl<T> PgOutcome<T> {
    fn from_result(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => PgOutcome::Ok(v),
            Err(msg) => PgOutcome::Err(msg),
        }
    }
}

impl Interp {
    /// `std/postgres` module dispatch (only `connect`; methods go through
    /// `call_postgres_method`).
    pub(crate) async fn call_postgres(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.postgres_connect(args, span).await,
            _ => Err(AsError::at(format!("std/postgres has no function '{}'", func), span).into()),
        }
    }

    async fn postgres_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let url = want_string(&arg(args, 0), span, "postgres.connect")?;
        let (client, connection) =
            match tokio_postgres::connect(&url, tokio_postgres::NoTls).await {
                Ok(pair) => pair,
                Err(e) => return Ok(err_pair(format!("postgres.connect failed: {}", e))),
            };
        // Drive the protocol on its own local task; abort it on close/drop.
        let join = crate::exec::spawn_local(async move {
            // If the connection errors (server closed, etc.), the future resolves;
            // the error is observed by the next client call as a Tier-1 error.
            let _ = connection.await;
        });
        let conn_task = join.abort_handle();
        let handle = self.register_resource(
            NativeKind::PostgresConnection,
            indexmap::IndexMap::new(),
            ResourceState::PostgresConnection { client, conn_task },
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// Dispatch a method on a Postgres connection handle.
    pub(crate) async fn call_postgres_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "close" => {
                // Aborting the driver task + dropping the Client closes the socket.
                if let Some(ResourceState::PostgresConnection { conn_task, .. }) =
                    self.take_resource(id)
                {
                    conn_task.abort();
                }
                Ok(Value::nil())
            }
            "query" => {
                // query(sql, params?, Class?) -> [array<row|instance>, err]
                let sql = want_string(&arg(&args, 0), span, "connection.query")?;
                let params = bind_params(args.get(1), span, "connection.query")?;
                let type_arg = args.get(2).cloned();
                let rows = match self.pg_run_query(id, &sql, &params, span).await? {
                    PgOutcome::Ok(rows) => rows,
                    PgOutcome::Err(msg) => return Ok(err_pair(msg)),
                    PgOutcome::Deadline => return Ok(crate::interp::deadline_exceeded_pair()),
                };
                let row_vals = rows.iter().map(rows_to_value).collect::<Vec<_>>();
                // Optional typed decode per row (Class or schema).
                if let Some(t) = type_arg {
                    let is_class = matches!(t.kind(), ValueKind::Class(_));
                    let is_schema = crate::stdlib::schema::schema_kind(&t).is_some();
                    if is_class || is_schema {
                        let parsed = make_pair(
                            Value::array_cell(crate::value::ArrayCell::new(row_vals)),
                            Value::nil(),
                        );
                        return self.typed_decode_rows(parsed, &t, span).await;
                    }
                }
                Ok(make_pair(
                    Value::array_cell(crate::value::ArrayCell::new(row_vals)),
                    Value::nil(),
                ))
            }
            "queryOne" => {
                let sql = want_string(&arg(&args, 0), span, "connection.queryOne")?;
                let params = bind_params(args.get(1), span, "connection.queryOne")?;
                let rows = match self.pg_run_query(id, &sql, &params, span).await? {
                    PgOutcome::Ok(rows) => rows,
                    PgOutcome::Err(msg) => return Ok(err_pair(msg)),
                    PgOutcome::Deadline => return Ok(crate::interp::deadline_exceeded_pair()),
                };
                match rows.first() {
                    Some(r) => Ok(make_pair(rows_to_value(r), Value::nil())),
                    None => Ok(make_pair(Value::nil(), Value::nil())),
                }
            }
            "exec" => {
                let sql = want_string(&arg(&args, 0), span, "connection.exec")?;
                let params = bind_params(args.get(1), span, "connection.exec")?;
                match self.pg_run_execute(id, &sql, &params, span).await? {
                    PgOutcome::Ok(n) => Ok(make_pair(Value::int(n as i64), Value::nil())),
                    PgOutcome::Err(msg) => Ok(err_pair(msg)),
                    PgOutcome::Deadline => Ok(crate::interp::deadline_exceeded_pair()),
                }
            }
            "begin" => self.pg_simple(id, "BEGIN", "connection.begin", span).await,
            "commit" => self.pg_simple(id, "COMMIT", "connection.commit", span).await,
            "rollback" => self.pg_simple(id, "ROLLBACK", "connection.rollback", span).await,
            other => {
                Err(AsError::at(format!("postgres connection has no method '{}'", other), span).into())
            }
        }
    }

    /// Run a query via the take-out-across-await pattern. Returns
    /// `Ok(PgOutcome::Ok(rows))` on success, `Ok(PgOutcome::Err(msg))` on a Tier-1
    /// DB error, `Ok(PgOutcome::Deadline)` when a `resilience.deadline` budget was
    /// exhausted (§5.4), or `Err(Control)` only for a closed-handle programmer error
    /// (never here: closed → Tier-1 err for ergonomics).
    async fn pg_run_query(
        &self,
        id: u64,
        sql: &str,
        params: &[BoundParam],
        _span: Span,
    ) -> Result<PgOutcome<Vec<tokio_postgres::Row>>, Control> {
        // RESIL §5.4 pre-check: an exhausted deadline budget → refuse before issuing
        // the query (no resource taken, no wire bytes). NO deadline → `None` fast path.
        if matches!(self.deadline_remaining_ms(), Some(r) if r <= 0.0) {
            return Ok(PgOutcome::Deadline);
        }
        let state = match self.take_resource(id) {
            Some(ResourceState::PostgresConnection { client, conn_task }) => (client, conn_task),
            other => {
                if let Some(o) = other {
                    self.return_resource(id, o);
                }
                return Ok(PgOutcome::Err("connection is closed".to_string()));
            }
        };
        let (client, conn_task) = state;
        let param_refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p.as_to_sql()).collect();
        // RESIL §5.4 budget-wrap: race the query against the remaining budget. On the
        // deadline branch the query future is DROPPED. tokio-postgres does NOT cancel
        // the server-side query on a dropped future (no CancelToken is sent): the
        // command continues to run on the server and its protocol replies remain
        // un-consumed on the shared connection, so the pooled connection is left in an
        // indeterminate state — callers should DISCARD a deadline-abandoned connection
        // rather than reuse it. NO deadline → the `None` branch awaits unchanged
        // (byte-identical). The resource is ALWAYS returned (so `close()` still works).
        let result = match self.deadline_remaining_ms() {
            Some(r) => {
                tokio::select! {
                    res = client.query(sql, &param_refs) => Some(res),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(r as u64)) => None,
                }
            }
            None => Some(client.query(sql, &param_refs).await),
        };
        self.return_resource(
            id,
            ResourceState::PostgresConnection { client, conn_task },
        );
        Ok(match result {
            Some(r) => PgOutcome::from_result(r.map_err(|e| format!("connection.query failed: {}", e))),
            None => PgOutcome::Deadline,
        })
    }

    async fn pg_run_execute(
        &self,
        id: u64,
        sql: &str,
        params: &[BoundParam],
        _span: Span,
    ) -> Result<PgOutcome<u64>, Control> {
        // RESIL §5.4 pre-check (see `pg_run_query`).
        if matches!(self.deadline_remaining_ms(), Some(r) if r <= 0.0) {
            return Ok(PgOutcome::Deadline);
        }
        let state = match self.take_resource(id) {
            Some(ResourceState::PostgresConnection { client, conn_task }) => (client, conn_task),
            other => {
                if let Some(o) = other {
                    self.return_resource(id, o);
                }
                return Ok(PgOutcome::Err("connection is closed".to_string()));
            }
        };
        let (client, conn_task) = state;
        let param_refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p.as_to_sql()).collect();
        // RESIL §5.4 budget-wrap (see `pg_run_query` for the connection-fate note).
        let result = match self.deadline_remaining_ms() {
            Some(r) => {
                tokio::select! {
                    res = client.execute(sql, &param_refs) => Some(res),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(r as u64)) => None,
                }
            }
            None => Some(client.execute(sql, &param_refs).await),
        };
        self.return_resource(
            id,
            ResourceState::PostgresConnection { client, conn_task },
        );
        Ok(match result {
            Some(r) => PgOutcome::from_result(r.map_err(|e| format!("connection.exec failed: {}", e))),
            None => PgOutcome::Deadline,
        })
    }

    async fn pg_simple(
        &self,
        id: u64,
        sql: &str,
        ctx: &str,
        span: Span,
    ) -> Result<Value, Control> {
        match self.pg_run_execute(id, sql, &[], span).await? {
            PgOutcome::Ok(_) => Ok(make_pair(Value::nil(), Value::nil())),
            PgOutcome::Err(msg) => Ok(err_pair(format!("{}: {}", ctx, msg))),
            PgOutcome::Deadline => Ok(crate::interp::deadline_exceeded_pair()),
        }
    }
}

/// A bound parameter, owning its Rust value so it lives across the await.
enum BoundParam {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl BoundParam {
    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            BoundParam::Null => &Option::<i64>::None,
            BoundParam::Bool(b) => b,
            BoundParam::Int(i) => i,
            BoundParam::Float(f) => f,
            BoundParam::Text(s) => s,
            BoundParam::Bytes(b) => b,
        }
    }
}

/// Parse the optional params array into bound params. Missing/nil → empty.
fn bind_params(v: Option<&Value>, span: Span, ctx: &str) -> Result<Vec<BoundParam>, Control> {
    match v.map(|x| x.kind()) {
        None | Some(ValueKind::Nil) => Ok(Vec::new()),
        // A Class/schema 3rd-arg-style value passed as 2nd arg is not params; but
        // params is positional-only (an array). Anything non-array is Tier-2.
        Some(ValueKind::Array(a)) => {
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(value_to_param(item, span, ctx)?);
            }
            Ok(out)
        }
        Some(_) => Err(AsError::at(
            format!(
                "{} params must be an array, got {}",
                ctx,
                crate::interp::type_name(v.unwrap())
            ),
            span,
        )
        .into()),
    }
}

fn value_to_param(v: &Value, span: Span, ctx: &str) -> Result<BoundParam, Control> {
    Ok(match v.kind() {
        ValueKind::Nil => BoundParam::Null,
        ValueKind::Bool(b) => BoundParam::Bool(b),
        // NUM §4: an `Int` binds directly as a SQL integer.
        ValueKind::Int(i) => BoundParam::Int(i),
        ValueKind::Float(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9.2e18 {
                BoundParam::Int(n as i64)
            } else {
                BoundParam::Float(n)
            }
        }
        ValueKind::Str(s) => BoundParam::Text(s.to_string()),
        ValueKind::Bytes(b) => BoundParam::Bytes(b.borrow().clone()),
        _ => {
            return Err(AsError::at(
                format!(
                    "{}: cannot bind a {} as a SQL parameter",
                    ctx,
                    crate::interp::type_name(v)
                ),
                span,
            )
            .into())
        }
    })
}

/// Convert a `tokio_postgres::Row` into an Object keyed by column name.
fn rows_to_value(row: &tokio_postgres::Row) -> Value {
    let mut map = indexmap::IndexMap::new();
    for (i, col) in row.columns().iter().enumerate() {
        map.insert(col.name().to_string(), column_to_value(row, i, col.type_()));
    }
    Value::object_cell(crate::value::ObjectCell::new(map))
}

/// Map a single Postgres column value to an AScript value, per the type map. A
/// decode failure for a mapped type falls back to Nil (defensive; the column was
/// well-typed at the wire level).
fn column_to_value(row: &tokio_postgres::Row, i: usize, ty: &Type) -> Value {
    use std::cell::RefCell;
    match *ty {
        Type::BOOL => row
            .try_get::<_, Option<bool>>(i)
            .ok()
            .flatten()
            .map(Value::bool_)
            .unwrap_or(Value::nil()),
        Type::INT2 => opt_int(row.try_get::<_, Option<i16>>(i).ok().flatten().map(|n| n as i64)),
        Type::INT4 => opt_int(row.try_get::<_, Option<i32>>(i).ok().flatten().map(|n| n as i64)),
        Type::INT8 => opt_int(row.try_get::<_, Option<i64>>(i).ok().flatten()),
        Type::OID => opt_int(row.try_get::<_, Option<u32>>(i).ok().flatten().map(|n| n as i64)),
        Type::FLOAT4 => opt_num(row.try_get::<_, Option<f32>>(i).ok().flatten().map(|n| n as f64)),
        Type::FLOAT8 => opt_num(row.try_get::<_, Option<f64>>(i).ok().flatten()),
        // numeric/decimal → text to avoid f64 precision loss.
        Type::NUMERIC => opt_str(decimal_as_string(row, i)),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR | Type::UNKNOWN => {
            opt_str(row.try_get::<_, Option<String>>(i).ok().flatten())
        }
        Type::CHAR => opt_int(row.try_get::<_, Option<i8>>(i).ok().flatten().map(|n| n as i64)),
        Type::BYTEA => row
            .try_get::<_, Option<Vec<u8>>>(i)
            .ok()
            .flatten()
            .map(|b| Value::bytes_rc(Rc::new(RefCell::new(b))))
            .unwrap_or(Value::nil()),
        Type::UUID => opt_str(
            row.try_get::<_, Option<String>>(i)
                .ok()
                .flatten()
                .or_else(|| uuid_as_string(row, i)),
        ),
        Type::JSON | Type::JSONB => match row.try_get::<_, Option<serde_json::Value>>(i) {
            Ok(Some(jv)) => crate::stdlib::json::to_ascript(&jv),
            _ => Value::nil(),
        },
        Type::TIMESTAMP | Type::TIMESTAMPTZ | Type::DATE | Type::TIME => {
            opt_str(row.try_get::<_, Option<String>>(i).ok().flatten())
        }
        // Fallback: try a string representation, else Nil.
        _ => opt_str(row.try_get::<_, Option<String>>(i).ok().flatten()),
    }
}

fn opt_num(n: Option<f64>) -> Value {
    n.map(Value::float).unwrap_or(Value::nil())
}
/// NUM §4: an integer SQL column decodes to `Int`.
fn opt_int(n: Option<i64>) -> Value {
    n.map(Value::int).unwrap_or(Value::nil())
}
fn opt_str(s: Option<String>) -> Value {
    s.map(Value::str).unwrap_or(Value::nil())
}

/// numeric columns: tokio-postgres has no built-in Decimal without a feature, so
/// read the column as text via Postgres' text output is not directly available;
/// we attempt a String get (works when the value arrives as text) else None.
fn decimal_as_string(row: &tokio_postgres::Row, i: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(i).ok().flatten()
}

/// uuid columns: attempt a String get (works when delivered as text).
fn uuid_as_string(row: &tokio_postgres::Row, i: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(i).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    // value_to_param maps each supported kind; unsupported is Tier-2.
    #[test]
    fn value_to_param_type_map() {
        assert!(matches!(value_to_param(&Value::nil(), sp(), "x").unwrap(), BoundParam::Null));
        assert!(matches!(value_to_param(&Value::bool_(true), sp(), "x").unwrap(), BoundParam::Bool(true)));
        assert!(matches!(value_to_param(&Value::float(3.0), sp(), "x").unwrap(), BoundParam::Int(3)));
        assert!(matches!(value_to_param(&Value::float(3.5), sp(), "x").unwrap(), BoundParam::Float(_)));
        assert!(matches!(value_to_param(&Value::str("hi"), sp(), "x").unwrap(), BoundParam::Text(_)));
        // A function value cannot be bound.
        assert!(value_to_param(&Value::builtin("math.abs"), sp(), "x").is_err());
    }

    #[test]
    fn bind_params_array_and_nil() {
        assert_eq!(bind_params(None, sp(), "x").unwrap().len(), 0);
        assert_eq!(bind_params(Some(&Value::nil()), sp(), "x").unwrap().len(), 0);
        let arr = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::float(1.0),
            Value::str("a"),
        ]));
        assert_eq!(bind_params(Some(&arr), sp(), "x").unwrap().len(), 2);
        // A non-array params arg is a Tier-2 error.
        assert!(bind_params(Some(&Value::float(1.0)), sp(), "x").is_err());
    }

    // Dead-port connect → clean Tier-1 err (NOT a panic). Runs under a LocalSet
    // because connect spawn_locals the driver task.
    #[tokio::test(flavor = "current_thread")]
    async fn dead_port_connect_is_tier1_err() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let pair = interp
                    .call_postgres(
                        "connect",
                        &[Value::str("postgres://127.0.0.1:1/none")],
                        sp(),
                    )
                    .await
                    .expect("connect must not panic on a dead port");
                // [nil, err]
                if let ValueKind::Array(a) = pair.kind() {
                    let b = a.borrow();
                    assert_eq!(b[0], Value::nil(), "value slot should be nil");
                    assert!(matches!(b[1].kind(), ValueKind::Object(_)), "err slot should be set");
                } else {
                    panic!("expected a [value, err] pair");
                }
            })
            .await;
    }

    // Live round-trip — skipped (passes) when ASCRIPT_TEST_POSTGRES_URL is unset.
    #[tokio::test(flavor = "current_thread")]
    async fn pg_roundtrip_live() {
        let Ok(url) = std::env::var("ASCRIPT_TEST_POSTGRES_URL") else {
            return; // no live server → no-op pass (no #[ignore])
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let suffix = format!("{}", uuid::Uuid::new_v4().simple());
                let table = format!("sp5_pg_{}", suffix);
                let pair = interp
                    .call_postgres("connect", &[Value::str(url.clone())], sp())
                    .await
                    .unwrap();
                let conn = match pair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("connect pair"),
                };
                assert!(matches!(conn.kind(), ValueKind::Native(_)), "connect should yield a handle");
                let m = |method: &str| -> Rc<NativeMethod> {
                    match conn.kind() {
                        ValueKind::Native(n) => Rc::new(NativeMethod {
                            receiver: n.clone(),
                            method: method.to_string(),
                        }),
                        _ => unreachable!(),
                    }
                };
                let exec = |sql: String| {
                    let interp = &interp;
                    let mm = m("exec");
                    async move {
                        interp
                            .call_postgres_method(&mm, vec![Value::str(sql)], sp())
                            .await
                            .unwrap()
                    }
                };
                exec(format!("CREATE TEMP TABLE {} (id int, name text)", table)).await;
                exec(format!("INSERT INTO {} VALUES (1, 'ada')", table)).await;
                let q = m("query");
                let rows = interp
                    .call_postgres_method(
                        &q,
                        vec![Value::str(format!("SELECT id, name FROM {} ORDER BY id", table))],
                        sp(),
                    )
                    .await
                    .unwrap();
                if let ValueKind::Array(a) = rows.kind() {
                    let b = a.borrow();
                    assert_eq!(b[1], Value::nil(), "query err should be nil");
                    if let ValueKind::Array(rs) = b[0].kind() {
                        let rs = rs.borrow();
                        assert_eq!(rs.len(), 1, "one row expected");
                        if let ValueKind::Object(o) = rs[0].kind() {
                            assert_eq!(o.get("id"), Some(Value::int(1)));
                            assert_eq!(o.get("name"), Some(Value::str("ada")));
                        }
                    }
                }
                // Cleanup: TEMP tables vanish with the session; close to be tidy.
                let c = m("close");
                interp.call_postgres_method(&c, vec![], sp()).await.unwrap();
            })
            .await;
    }

    // ── RESIL §5.4: deadline-aware Postgres consult site ─────────────────────

    use crate::interp::{task_locals_scope, TaskLocals};
    use crate::value::{NativeMethod, NativeObject};

    fn assert_deadline_pair(pair: &Value) {
        match pair.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                assert_eq!(b.len(), 2, "expected a [value, err] pair");
                assert_eq!(b[0], Value::nil());
                match b[1].kind() {
                    ValueKind::Object(o) => assert_eq!(
                        o.get("code"),
                        Some(Value::str("deadline-exceeded")),
                        "err code should be deadline-exceeded"
                    ),
                    other => panic!("err slot should be an object, got {:?}", other),
                }
            }
            other => panic!("expected a pair, got {:?}", other),
        }
    }

    // §5.4 pre-check (NO live server): with an ALREADY-EXPIRED deadline, the query
    // op-runner returns the deadline-exceeded pair BEFORE taking the connection
    // resource — so even a non-existent handle id never reaches the wire. This
    // isolates the pre-check from any connection. The receiver id is bogus on
    // purpose: if the pre-check did NOT fire first, the path would instead hit the
    // "connection is closed" Tier-1 err (a {message}-only pair, no `code`).
    #[tokio::test(flavor = "current_thread")]
    async fn pg_expired_deadline_pre_check_no_op() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let now = interp.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
                let locals = std::rc::Rc::new(TaskLocals {
                    deadline_at_ms: Some(now - 1000.0),
                    trace_id: None,
                });
                let recv = std::rc::Rc::new(NativeObject {
                    id: u64::MAX, // a handle that is NOT registered
                    kind: NativeKind::PostgresConnection,
                    fields: indexmap::IndexMap::new(),
                });
                let m = std::rc::Rc::new(NativeMethod {
                    receiver: recv,
                    method: "query".to_string(),
                });
                let pair = task_locals_scope(Some(locals), async {
                    interp
                        .call_postgres_method(&m, vec![Value::str("SELECT 1")], sp())
                        .await
                        .expect("must not panic")
                })
                .await;
                assert_deadline_pair(&pair);
            })
            .await;
    }

    // Live budget-wrap — gated on ASCRIPT_TEST_POSTGRES_URL (skips/pass when unset,
    // matching the other live tests). A `pg_sleep(5)` query under a 100ms deadline
    // returns `deadline-exceeded` well before the 5s server-side sleep completes.
    #[tokio::test(flavor = "current_thread")]
    async fn pg_budget_wrap_live() {
        let Ok(url) = std::env::var("ASCRIPT_TEST_POSTGRES_URL") else {
            return; // no live server → no-op pass
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                let pair = interp
                    .call_postgres("connect", &[Value::str(url.clone())], sp())
                    .await
                    .unwrap();
                let conn = match pair.kind() {
                    ValueKind::Array(a) => a.borrow()[0].clone(),
                    _ => panic!("connect pair"),
                };
                let recv = match conn.kind() {
                    ValueKind::Native(n) => n.clone(),
                    _ => panic!("handle"),
                };
                let m = std::rc::Rc::new(NativeMethod {
                    receiver: recv,
                    method: "query".to_string(),
                });
                let now = interp.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
                let locals = std::rc::Rc::new(TaskLocals {
                    deadline_at_ms: Some(now + 100.0),
                    trace_id: None,
                });
                let started = std::time::Instant::now();
                let res = task_locals_scope(Some(locals), async {
                    interp
                        .call_postgres_method(&m, vec![Value::str("SELECT pg_sleep(5)")], sp())
                        .await
                        .unwrap()
                })
                .await;
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(3),
                    "budget-wrap must return well before the 5s pg_sleep"
                );
                assert_deadline_pair(&res);
            })
            .await;
    }
}
