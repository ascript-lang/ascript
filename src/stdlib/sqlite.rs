//! `std/sqlite` — SQLite database access (feature `sql`, backed by `rusqlite`
//! with the `bundled` SQLite, so no system library is required).
//!
//! This is the first real consumer of the native resource-handle mechanism
//! (`Value::Native` + the interp `resources` table from M13 Task 1). `open` is the
//! only module-level function; everything else is a method on a handle:
//!
//! - `open(path) -> [connection, err]` (`":memory:"` for an in-memory DB) registers
//!   a `ResourceState::SqliteConnection` and returns a `Value::Native` of kind
//!   `SqliteConnection`.
//! - Connection methods: `exec(sql, params?) -> [changes, err]`,
//!   `query(sql, params?) -> [rows, err]` (rows are objects keyed by column),
//!   `prepare(sql) -> [statement, err]`, `begin/commit/rollback() -> [nil, err]`,
//!   and `close() -> nil` (removes the connection; reuse → Tier-2 "use after close").
//! - Statement methods: `run(params?) -> [changes, err]`, `all(params?) -> [rows, err]`.
//!
//! ## The statement borrow problem
//! rusqlite's `Statement<'conn>` borrows the `Connection`, so it can't be stored in
//! the resource table alongside its connection. Instead, a statement handle stores
//! only the SQL text + the owning connection's id
//! (`ResourceState::SqliteStatement { conn_id, sql }`); `run`/`all` resolve the
//! connection by id and execute via `Connection::prepare_cached(&sql)`, which
//! rusqlite caches internally (no re-parse). This sidesteps the self-referential
//! borrow cleanly while keeping prepared statements efficient.
//!
//! ## Transactions
//! Explicit `begin()/commit()/rollback()` (plain `BEGIN`/`COMMIT`/`ROLLBACK` SQL)
//! rather than a callback form — simpler given rusqlite's borrow model and our
//! re-entrant interpreter.
//!
//! Params are either a positional array (`?`/`?1`) or a named object whose keys are
//! the `:name` placeholders (the leading `:` is optional in the key). AScript values
//! map to SQLite as: Number→i64 (if integral) or f64, Str→text, Bool→int 0/1,
//! Nil→null, Bytes→blob. SQLite values map back as int/real→Number, text→Str,
//! blob→Bytes, null→Nil.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use rusqlite::types::{Value as SqlValue, ValueRef};
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("open", bi("sqlite.open"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn obj(map: indexmap::IndexMap<String, Value>) -> Value {
    Value::Object(crate::value::ObjectCell::new(map))
}

impl Interp {
    /// Module-level dispatch for `std/sqlite` (only `open`). Method calls on a
    /// connection/statement handle go through `call_sqlite_method` instead.
    pub(crate) fn call_sqlite_open(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "open" => {
                let path = want_string(&arg(args, 0), span, "sqlite.open")?;
                match rusqlite::Connection::open(&*path) {
                    Ok(conn) => {
                        let handle = self.register_resource(
                            NativeKind::SqliteConnection,
                            indexmap::IndexMap::new(),
                            ResourceState::SqliteConnection(conn),
                        );
                        Ok(make_pair(handle, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("sqlite.open failed: {}", e))),
                }
            }
            _ => Err(AsError::at(format!("std/sqlite has no function '{}'", func), span).into()),
        }
    }

    /// Dispatch a method on a sqlite connection or statement handle. Sync work, but
    /// async-signed for uniformity with `call_native_method`.
    pub(crate) async fn call_sqlite_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::SqliteConnection => self.sqlite_conn_method(id, &m.method, &args, span),
            NativeKind::SqliteStatement => self.sqlite_stmt_method(id, &m.method, &args, span),
            // call_native_method only routes the two sqlite kinds here.
            _ => {
                Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into())
            }
        }
    }

    fn sqlite_conn_method(
        &self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // `close` consumes the handle; do it before the conn-presence check so a
        // double close is itself a "use after close".
        if method == "close" {
            return match self.take_resource(id) {
                Some(_) => Ok(Value::Nil),
                None => Err(use_after_close(span)),
            };
        }

        // Every other method needs a live connection.
        if self.sqlite_conn(id).is_none() {
            return Err(use_after_close(span));
        }

        match method {
            "exec" => {
                let sql = want_string(&arg(args, 0), span, "connection.exec")?;
                let params = parse_params(args.get(1), span, "connection.exec")?;
                let conn = self.sqlite_conn(id).expect("checked present");
                match exec_sql(&conn, &sql, &params) {
                    Ok(changes) => Ok(make_pair(Value::Number(changes as f64), Value::Nil)),
                    Err(e) => Ok(err_pair(format!("connection.exec failed: {}", e))),
                }
            }
            "query" => {
                let sql = want_string(&arg(args, 0), span, "connection.query")?;
                let params = parse_params(args.get(1), span, "connection.query")?;
                let conn = self.sqlite_conn(id).expect("checked present");
                match query_sql(&conn, &sql, &params) {
                    Ok(rows) => Ok(make_pair(
                        Value::Array(Rc::new(RefCell::new(rows))),
                        Value::Nil,
                    )),
                    Err(e) => Ok(err_pair(format!("connection.query failed: {}", e))),
                }
            }
            "prepare" => {
                let sql = want_string(&arg(args, 0), span, "connection.prepare")?;
                // Validate the SQL up front so a bad statement is a Tier-1 err here
                // rather than surfacing only on the first run/all. Scope the conn
                // borrow so it drops before `register_resource` re-borrows the table.
                if let Some(e) = {
                    let conn = self.sqlite_conn(id).expect("checked present");
                    conn.prepare(&sql).err().map(|e| e.to_string())
                } {
                    return Ok(err_pair(format!("connection.prepare failed: {}", e)));
                }
                let handle = self.register_resource(
                    NativeKind::SqliteStatement,
                    indexmap::IndexMap::new(),
                    ResourceState::SqliteStatement {
                        conn_id: id,
                        sql: sql.to_string(),
                    },
                );
                Ok(make_pair(handle, Value::Nil))
            }
            "begin" => self.conn_exec_simple(id, "BEGIN", "connection.begin"),
            "commit" => self.conn_exec_simple(id, "COMMIT", "connection.commit"),
            "rollback" => self.conn_exec_simple(id, "ROLLBACK", "connection.rollback"),
            _ => Err(AsError::at(format!("connection has no method '{}'", method), span).into()),
        }
    }

    fn conn_exec_simple(&self, id: u64, sql: &str, ctx: &str) -> Result<Value, Control> {
        let conn = self.sqlite_conn(id).expect("checked present");
        match conn.execute(sql, []) {
            Ok(_) => Ok(make_pair(Value::Nil, Value::Nil)),
            Err(e) => Ok(err_pair(format!("{} failed: {}", ctx, e))),
        }
    }

    fn sqlite_stmt_method(
        &self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Pull the stored SQL + owning connection id out of the statement resource.
        let (conn_id, sql) = match self.with_resource(id, |r| match r {
            Some(ResourceState::SqliteStatement { conn_id, sql }) => Some((*conn_id, sql.clone())),
            _ => None,
        }) {
            Some(pair) => pair,
            None => return Err(use_after_close(span)),
        };
        // The statement is dead once its connection is closed.
        if self.sqlite_conn(conn_id).is_none() {
            return Err(use_after_close(span));
        }

        match method {
            "run" => {
                let params = parse_params(args.first(), span, "statement.run")?;
                let conn = self.sqlite_conn(conn_id).expect("checked present");
                match exec_cached(&conn, &sql, &params) {
                    Ok(changes) => Ok(make_pair(Value::Number(changes as f64), Value::Nil)),
                    Err(e) => Ok(err_pair(format!("statement.run failed: {}", e))),
                }
            }
            "all" => {
                let params = parse_params(args.first(), span, "statement.all")?;
                let conn = self.sqlite_conn(conn_id).expect("checked present");
                match query_cached(&conn, &sql, &params) {
                    Ok(rows) => Ok(make_pair(
                        Value::Array(Rc::new(RefCell::new(rows))),
                        Value::Nil,
                    )),
                    Err(e) => Ok(err_pair(format!("statement.all failed: {}", e))),
                }
            }
            _ => Err(AsError::at(format!("statement has no method '{}'", method), span).into()),
        }
    }
}

fn use_after_close(span: Span) -> Control {
    AsError::at("use after close: this sqlite handle is closed", span).into()
}

/// Parameter binding: either positional (`?`) or named (`:name`).
enum Params {
    None,
    Positional(Vec<SqlValue>),
    Named(Vec<(String, SqlValue)>),
}

/// Convert an AScript value used as a bind parameter into a SQLite value.
fn to_sql(v: &Value, span: Span, ctx: &str) -> Result<SqlValue, Control> {
    Ok(match v {
        Value::Nil => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9.2e18 {
                SqlValue::Integer(*n as i64)
            } else {
                SqlValue::Real(*n)
            }
        }
        Value::Str(s) => SqlValue::Text(s.to_string()),
        Value::Bytes(b) => SqlValue::Blob(b.borrow().clone()),
        other => {
            return Err(AsError::at(
                format!(
                    "{}: cannot bind a {} as a SQL parameter",
                    ctx,
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
    })
}

/// Parse the optional second argument (params) into a `Params`. A missing/nil arg
/// is `None`; an array is positional; an object is named. Anything else is Tier-2.
fn parse_params(v: Option<&Value>, span: Span, ctx: &str) -> Result<Params, Control> {
    match v {
        None | Some(Value::Nil) => Ok(Params::None),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(to_sql(item, span, ctx)?);
            }
            Ok(Params::Positional(out))
        }
        Some(Value::Object(o)) => {
            let mut out = Vec::new();
            for (k, val) in o.borrow().iter() {
                // Accept both ":name" and "name" keys; rusqlite wants the ":" form.
                let key = if k.starts_with(':') {
                    k.clone()
                } else {
                    format!(":{}", k)
                };
                out.push((key, to_sql(val, span, ctx)?));
            }
            Ok(Params::Named(out))
        }
        Some(other) => Err(AsError::at(
            format!(
                "{} params must be an array or an object, got {}",
                ctx,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Map a borrowed SQLite column value into an AScript value.
fn from_sql(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Nil,
        ValueRef::Integer(i) => Value::Number(i as f64),
        ValueRef::Real(r) => Value::Number(r),
        ValueRef::Text(t) => Value::Str(String::from_utf8_lossy(t).into_owned().into()),
        ValueRef::Blob(b) => Value::Bytes(Rc::new(RefCell::new(b.to_vec()))),
    }
}

/// Bind a `Params` onto a prepared statement, returning bound for execution. We
/// dispatch on the param kind and forward to rusqlite's `execute`/`query` with the
/// matching params form.
macro_rules! with_bound {
    ($stmt:expr, $params:expr, |$bound:ident| $body:expr) => {{
        match $params {
            Params::None => {
                let $bound = rusqlite::params![];
                $body
            }
            Params::Positional(p) => {
                let $bound = rusqlite::params_from_iter(p.iter());
                $body
            }
            Params::Named(p) => {
                let refs: Vec<(&str, &dyn rusqlite::ToSql)> = p
                    .iter()
                    .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
                    .collect();
                let $bound = refs.as_slice();
                $body
            }
        }
    }};
}

fn exec_sql(conn: &rusqlite::Connection, sql: &str, params: &Params) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(sql)?;
    with_bound!(&mut stmt, params, |bound| stmt.execute(bound))
}

fn exec_cached(conn: &rusqlite::Connection, sql: &str, params: &Params) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare_cached(sql)?;
    with_bound!(&mut stmt, params, |bound| stmt.execute(bound))
}

fn query_sql(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &Params,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    collect_rows(&mut stmt, params)
}

fn query_cached(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &Params,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(sql)?;
    collect_rows(&mut stmt, params)
}

/// Run a query and collect each row into an object keyed by column name.
fn collect_rows(
    stmt: &mut rusqlite::Statement<'_>,
    params: &Params,
) -> rusqlite::Result<Vec<Value>> {
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    with_bound!(stmt, params, |bound| {
        let mut rows = stmt.query(bound)?;
        while let Some(row) = rows.next()? {
            let mut map = indexmap::IndexMap::new();
            for (i, name) in col_names.iter().enumerate() {
                map.insert(name.clone(), from_sql(row.get_ref(i)?));
            }
            out.push(obj(map));
        }
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::value::Value;

    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    /// Run an AScript program expecting a Tier-2 panic; return the panic message.
    async fn run_err(src: &str) -> String {
        match crate::run_source(src).await {
            Ok(out) => panic!("expected a panic, but program produced output:\n{}", out),
            Err(e) => e.message,
        }
    }

    #[tokio::test]
    async fn open_memory_returns_a_connection() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, err] = open(":memory:")
print(err)
print(type(conn))
"#)
        .await;
        assert_eq!(out, "nil\nconnection\n");
    }

    #[tokio::test]
    async fn create_table_then_insert_reports_changes() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")
let [c0, e0] = conn.exec("CREATE TABLE t (id INTEGER, name TEXT)")
print(c0)
print(e0)
let [c1, e1] = conn.exec("INSERT INTO t VALUES (1, 'alice')")
print(c1)
"#)
        .await;
        // CREATE reports 0 changes; the INSERT reports 1.
        assert_eq!(out, "0\nnil\n1\n");
    }

    #[tokio::test]
    async fn query_returns_rows_as_objects_keyed_by_column() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER, name TEXT)")
conn.exec("INSERT INTO t VALUES (1, 'alice')")
conn.exec("INSERT INTO t VALUES (2, 'bob')")
let [rows, err] = conn.query("SELECT id, name FROM t ORDER BY id")
print(err)
print(len(rows))
print(rows[0].id)
print(rows[0].name)
print(rows[1].name)
"#)
        .await;
        assert_eq!(out, "nil\n2\n1\nalice\nbob\n");
    }

    #[tokio::test]
    async fn positional_params() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER, name TEXT)")
conn.exec("INSERT INTO t VALUES (?, ?)", [7, "gwen"])
let [rows, _e1] = conn.query("SELECT name FROM t WHERE id = ?", [7])
print(rows[0].name)
"#)
        .await;
        assert_eq!(out, "gwen\n");
    }

    #[tokio::test]
    async fn named_params() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER, name TEXT)")
conn.exec("INSERT INTO t VALUES (:id, :name)", { id: 3, name: "carol" })
let [rows, _e1] = conn.query("SELECT name FROM t WHERE id = :id", { id: 3 })
print(rows[0].name)
"#)
        .await;
        assert_eq!(out, "carol\n");
    }

    #[tokio::test]
    async fn prepared_statement_run_and_all() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER, name TEXT)")
let [ins, perr] = conn.prepare("INSERT INTO t VALUES (?, ?)")
print(perr)
let [r1, _e1] = ins.run([1, "a"])
let [r2, _e2] = ins.run([2, "b"])
print(r1)
print(r2)
let [sel, _e3] = conn.prepare("SELECT name FROM t ORDER BY id")
let [rows, _e4] = sel.all()
print(len(rows))
print(rows[0].name)
print(rows[1].name)
"#)
        .await;
        assert_eq!(out, "nil\n1\n1\n2\na\nb\n");
    }

    #[tokio::test]
    async fn transaction_commit_persists() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER)")
conn.begin()
conn.exec("INSERT INTO t VALUES (1)")
conn.commit()
let [rows, _e1] = conn.query("SELECT id FROM t")
print(len(rows))
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    #[tokio::test]
    async fn transaction_rollback_discards() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (id INTEGER)")
conn.begin()
conn.exec("INSERT INTO t VALUES (1)")
conn.rollback()
let [rows, _e1] = conn.query("SELECT id FROM t")
print(len(rows))
"#)
        .await;
        assert_eq!(out, "0\n");
    }

    #[tokio::test]
    async fn close_then_use_is_use_after_close_panic() {
        let msg = run_err(
            r#"
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")
conn.close()
conn.exec("CREATE TABLE t (id INTEGER)")
"#,
        )
        .await;
        assert!(msg.contains("use after close"), "got: {}", msg);
    }

    #[tokio::test]
    async fn sql_syntax_error_is_tier1_err() {
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")
let [val, err] = conn.exec("CREATE TABBLE oops")
print(val)
print(err != nil)
"#)
        .await;
        // Tier-1: value slot nil, err slot present.
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn type_mapping_int_real_text_blob_null() {
        // Store one row with each affinity and read the values back, checking that
        // each maps to the right AScript kind.
        let out = run(r#"
import { open } from "std/sqlite"
import { fromArray } from "std/bytes"
let [conn, _e0] = open(":memory:")
conn.exec("CREATE TABLE t (i INTEGER, r REAL, s TEXT, b BLOB, n INTEGER)")
let [c, e] = conn.exec("INSERT INTO t VALUES (?, ?, ?, ?, ?)", [42, 3.5, "hi", fromArray([0, 255, 16]), nil])
print(e)
let [rows, _e1] = conn.query("SELECT i, r, s, b, n FROM t")
let row = rows[0]
print(type(row.i))
print(row.i)
print(type(row.r))
print(row.r)
print(type(row.s))
print(row.s)
print(type(row.b))
print(len(row.b))
print(type(row.n))
print(row.n)
"#).await;
        assert_eq!(
            out,
            "nil\nnumber\n42\nnumber\n3.5\nstring\nhi\nbytes\n3\nnil\nnil\n"
        );
    }

    #[tokio::test]
    async fn handle_display_and_equality() {
        // A connection handle is identity-equal to itself and displays as a native.
        let out = run(r#"
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")
print(conn == conn)
print(conn)
"#)
        .await;
        assert_eq!(out, "true\n<native connection #0>\n");
    }

    // A direct Rust-level sanity check that `open` registers a SqliteConnection
    // resource reachable through the handle's id.
    #[tokio::test]
    async fn open_registers_resource_state() {
        use crate::interp::{Interp, ResourceState};
        let interp = Interp::new();
        let pair = interp
            .call_sqlite_open(
                "open",
                &[Value::Str(":memory:".into())],
                crate::span::Span::new(0, 0),
            )
            .unwrap();
        let conn_handle = match &pair {
            Value::Array(a) => a.borrow()[0].clone(),
            _ => panic!("expected pair"),
        };
        let id = match &conn_handle {
            Value::Native(n) => n.id,
            _ => panic!("expected native handle"),
        };
        assert!(interp.with_resource(id, |r| matches!(
            r,
            Some(ResourceState::SqliteConnection(_))
        )));
    }
}
