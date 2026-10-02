//! Per-plugin database — the `db` capability of the host surface.
//!
//! Each plugin that declares `capabilities = ["db"]` gets ONE SQLite file of
//! its own (`<data>/plugins/<name>.db`), opened by the core on its behalf. The
//! plugin never sees a path or a handle: it sends SQL through three host calls
//! (`db_query` read-only, `db_exec` one statement, `db_batch` several
//! statements atomically) and gets JSON back. Cf. `Doc/plugin-host.md`.
//!
//! The plugin owns its schema: it ships ordered migrations (native
//! `Plugin::db_migrations`, WASM export `db_migrations`), applied by the core
//! at start-up before `on_load`, each in a transaction, recorded with its full
//! SQL in `_stationd_migrations` inside the plugin's file. A migration already
//! applied and since modified — or one the plugin no longer ships — refuses the
//! start (loud), same rule as the core's own migrations.
//!
//! Confinement (the plugin's SQL is untrusted):
//! - `SQLITE_LIMIT_ATTACHED = 0` and an authorizer: no `ATTACH`/`DETACH`, no
//!   `PRAGMA` outside a read-only allowlist, no `load_extension`, no
//!   `BEGIN`/`COMMIT`/`SAVEPOINT` (transactions are the core's: `db_batch`),
//!   no write to the reserved `_stationd*` tables, no `VACUUM` (`VACUUM INTO`
//!   writes a file anywhere);
//! - defensive mode (no schema corruption through `writable_schema`);
//! - a wall-clock bound per call (`query_timeout_ms`, progress handler), a
//!   cap on returned rows (`max_rows`, exceeding it is an error, never a silent
//!   truncation) and on the file size (`max_size_mb`, `max_page_count`).
//!
//! Synchronous (rusqlite): plugins are called synchronously from their actor
//! task, a host call cannot await sqlx there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rusqlite::config::DbConfig;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Prefix of the core's own tables inside a plugin file (read-only to it).
const RESERVED_PREFIX: &str = "_stationd";

/// Read-only pragmas a plugin may use (schema introspection).
const PLUGIN_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
];

/// Wall-clock bound of an admin read (`stationctl plugin db query`), which is
/// not on the air path.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Limits (`[plugin.db]`)
// ---------------------------------------------------------------------------

/// `[plugin.db]` — bounds of a plugin's database. All optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbLimits {
    /// Maximum size of the file, MiB. A write that would grow past it fails
    /// (`database or disk is full`) — the plugin gets the error.
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u32,
    /// Wall-clock bound of one host call (a `db_batch` as a whole), ms. Past
    /// it the statement is interrupted and the call fails (a batch rolls back).
    #[serde(default = "default_query_timeout_ms")]
    pub query_timeout_ms: u32,
    /// Maximum rows a `db_query` may return. More is an error, never a silent
    /// truncation: the plugin adds a `LIMIT`.
    #[serde(default = "default_max_rows")]
    pub max_rows: u32,
}

fn default_max_size_mb() -> u32 {
    64
}
fn default_query_timeout_ms() -> u32 {
    200
}
fn default_max_rows() -> u32 {
    10_000
}

impl Default for DbLimits {
    fn default() -> Self {
        Self {
            max_size_mb: default_max_size_mb(),
            query_timeout_ms: default_query_timeout_ms(),
            max_rows: default_max_rows(),
        }
    }
}

impl DbLimits {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_size_mb == 0 {
            return Err("`max_size_mb` must be ≥ 1".into());
        }
        if self.query_timeout_ms == 0 {
            return Err("`query_timeout_ms` must be ≥ 1".into());
        }
        if self.max_rows == 0 {
            return Err("`max_rows` must be ≥ 1".into());
        }
        Ok(())
    }

    fn max_size_bytes(&self) -> u64 {
        u64::from(self.max_size_mb) * 1024 * 1024
    }
}

/// A plugin name usable as a file name: ASCII letters, digits, `-`, `_`.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "plugin `{name}`: a plugin with capability `db` needs a name made of ASCII letters, digits, `-` and `_` (it names its database file)"
        ));
    }
    Ok(())
}

/// Where a plugin's database lives in `dir`.
pub fn db_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.db"))
}

// ---------------------------------------------------------------------------
// Errors and wire types (JSON at the WASM boundary)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbError {
    #[error("cannot open the plugin database {path}: {reason}")]
    Open { path: String, reason: String },
    #[error("not allowed for a plugin: {0}")]
    Denied(String),
    #[error("query exceeded {0} ms and was interrupted")]
    Timeout(u32),
    #[error("query returned more than {0} rows (add a LIMIT)")]
    TooManyRows(u32),
    #[error("db_query only runs read-only statements (use db_exec / db_batch to write)")]
    NotReadOnly,
    #[error("bad parameters: {0}")]
    Params(String),
    #[error("migration {version}: {reason}")]
    Migration { version: usize, reason: String },
    #[error("statement {index}: {reason}")]
    Batch { index: usize, reason: String },
    #[error("{0}")]
    Sql(String),
    /// A write while the core runs the plugin for a simulation.
    #[error("the database is read-only during a simulation (nothing may change)")]
    ReadOnly,
}

/// Statement parameters: positional (`?`, `?1`) as an array, or named
/// (`:name`, `@name`, `$name`) as an object (the prefix may be omitted: `:`
/// is assumed). Every parameter of the statement must be bound — an unbound
/// one is an error, never a silent NULL.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Params {
    Positional(Vec<Json>),
    Named(serde_json::Map<String, Json>),
}

impl Default for Params {
    fn default() -> Self {
        Params::Positional(Vec::new())
    }
}

/// One statement of a `db_exec` / `db_batch`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub sql: String,
    #[serde(default)]
    pub params: Params,
}

/// Result of a `db_query`. Values: `null`, numbers, strings; a BLOB is
/// `{"blob": "<base64>"}` (accepted in the same form as a parameter).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Json>>,
}

/// Result of one written statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecOutcome {
    pub changes: u64,
    pub last_insert_rowid: i64,
}

/// What `migrate` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrateOutcome {
    pub applied: usize,
    pub version: usize,
}

fn json_to_sql(v: &Json) -> Result<SqlValue, String> {
    Ok(match v {
        Json::Null => SqlValue::Null,
        Json::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Json::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => match n.as_f64() {
                Some(f) => SqlValue::Real(f),
                None => return Err(format!("number out of range: {n}")),
            },
        },
        Json::String(s) => SqlValue::Text(s.clone()),
        Json::Object(m) if m.len() == 1 && m.contains_key("blob") => {
            let b64 = m["blob"]
                .as_str()
                .ok_or_else(|| "`blob` must be a base64 string".to_string())?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("bad base64 blob: {e}"))?;
            SqlValue::Blob(bytes)
        }
        Json::Array(_) | Json::Object(_) => {
            return Err("a parameter is null, a boolean, a number, a string or {\"blob\": \"<base64>\"}".into())
        }
    })
}

fn sql_to_json(v: ValueRef<'_>) -> Json {
    match v {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => Json::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number),
        ValueRef::Text(t) => Json::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            serde_json::json!({ "blob": base64::engine::general_purpose::STANDARD.encode(b) })
        }
    }
}

// ---------------------------------------------------------------------------
// Guard: authorizer + deadline shared with the connection's hooks
// ---------------------------------------------------------------------------

/// State shared between the connection and its hooks. `plugin_sql` is set
/// while the plugin's own SQL is prepared and stepped (the core's
/// `BEGIN`/`COMMIT` and bookkeeping run with it cleared).
#[derive(Default)]
struct Guard {
    plugin_sql: AtomicBool,
    deadline: Mutex<Option<Instant>>,
    denied: Mutex<Option<String>>,
}

fn is_reserved(table: &str) -> bool {
    table.len() >= RESERVED_PREFIX.len()
        && table[..RESERVED_PREFIX.len()].eq_ignore_ascii_case(RESERVED_PREFIX)
}

/// The authorizer's verdict on the plugin's SQL: `Some(reason)` = refused.
fn refusal(action: &AuthAction<'_>) -> Option<String> {
    let reserved = |t: &str| is_reserved(t).then(|| format!("table `{t}` is reserved to the core"));
    match *action {
        AuthAction::Attach { .. } => Some("ATTACH".into()),
        AuthAction::Detach { .. } => Some("DETACH".into()),
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => {
            Some("transaction control (use db_batch for atomic writes)".into())
        }
        AuthAction::Pragma { pragma_name, .. } => {
            let p = pragma_name.to_ascii_lowercase();
            (!PLUGIN_PRAGMAS.contains(&p.as_str())).then(|| format!("PRAGMA {p}"))
        }
        AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("load_extension") => {
            Some("load_extension".into())
        }
        AuthAction::Insert { table_name }
        | AuthAction::Delete { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::CreateTable { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::CreateIndex { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
        | AuthAction::CreateTrigger { table_name, .. }
        | AuthAction::DropTrigger { table_name, .. } => reserved(table_name),
        AuthAction::CreateView { view_name } | AuthAction::DropView { view_name } => reserved(view_name),
        _ => None,
    }
}

/// `VACUUM` has no authorizer code; `VACUUM INTO 'file'` would write outside
/// the plugin's file. Refused on the statement text.
fn is_vacuum(sql: &str) -> bool {
    let mut s = sql.trim_start();
    // Skip leading comments.
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    s.len() >= 6 && s[..6].eq_ignore_ascii_case("vacuum")
}

fn contains_vacuum(sql: &str) -> bool {
    // A migration / batch may hold several statements: check each one.
    sql.split(';').any(is_vacuum)
}

fn configure(conn: &Connection, guard: &Arc<Guard>) -> Result<(), String> {
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(|e| e.to_string())?;
    let g = Arc::clone(guard);
    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        if !g.plugin_sql.load(Ordering::SeqCst) {
            return Authorization::Allow;
        }
        match refusal(&ctx.action) {
            None => Authorization::Allow,
            Some(reason) => {
                if let Ok(mut d) = g.denied.lock() {
                    *d = Some(reason);
                }
                Authorization::Deny
            }
        }
    }));
    let g = Arc::clone(guard);
    conn.progress_handler(
        1_000,
        Some(move || match g.deadline.lock() {
            Ok(d) => d.is_some_and(|at| Instant::now() >= at),
            Err(_) => true,
        }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The live database of a loaded plugin
// ---------------------------------------------------------------------------

/// A plugin's database, opened by the core for it. Only reached through the
/// plugin's `Host` (capability `db`).
pub struct PluginDb {
    conn: Mutex<Connection>,
    guard: Arc<Guard>,
    path: PathBuf,
    limits: DbLimits,
    /// Set by the core around a simulated hook: `exec` / `batch` refused.
    read_only: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for PluginDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDb").field("path", &self.path).finish()
    }
}

impl PluginDb {
    /// Open (or create) `<dir>/<name>.db` and set its confinement up.
    pub fn open(dir: &Path, name: &str, limits: DbLimits) -> Result<Self, DbError> {
        validate_name(name).map_err(|reason| DbError::Open {
            path: dir.display().to_string(),
            reason,
        })?;
        let path = db_path(dir, name);
        let open_err = |reason: String| DbError::Open {
            path: path.display().to_string(),
            reason,
        };
        std::fs::create_dir_all(dir).map_err(|e| open_err(e.to_string()))?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| open_err(e.to_string()))?;
        let guard = Arc::new(Guard::default());
        configure(&conn, &guard).map_err(open_err)?;
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(|e| open_err(e.to_string()))?;
        let max_pages = (limits.max_size_bytes() / page_size.max(512) as u64).max(1);
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = ON;
             PRAGMA max_page_count = {max_pages};
             CREATE TABLE IF NOT EXISTS {RESERVED_PREFIX}_migrations (
                 version    INTEGER PRIMARY KEY,
                 sql        TEXT    NOT NULL,
                 applied_at INTEGER NOT NULL
             );"
        ))
        .map_err(|e| open_err(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
            guard,
            path,
            limits,
            read_only: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn limits(&self) -> DbLimits {
        self.limits
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DbError> {
        self.conn
            .lock()
            .map_err(|_| DbError::Sql("plugin database poisoned".into()))
    }

    /// Run `f` as the plugin: its SQL is authorised as untrusted and bounded
    /// in time. The whole call (a batch included) shares one deadline.
    fn as_plugin<R>(&self, f: impl FnOnce() -> Result<R, DbError>) -> Result<R, DbError> {
        *self.guard.denied.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.guard.deadline.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Instant::now() + Duration::from_millis(u64::from(self.limits.query_timeout_ms)));
        self.guard.plugin_sql.store(true, Ordering::SeqCst);
        let out = f();
        self.guard.plugin_sql.store(false, Ordering::SeqCst);
        *self.guard.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
        out
    }

    /// Map a rusqlite error raised by the plugin's SQL: a denial names what
    /// was refused, an interruption is the timeout.
    fn map_err(&self, e: rusqlite::Error) -> DbError {
        map_sql_error(&self.guard, self.limits.query_timeout_ms, e)
    }

    /// Read-only query (`db_query`).
    pub fn query(&self, sql: &str, params: &Params) -> Result<Rows, DbError> {
        let conn = self.lock()?;
        self.as_plugin(|| {
            run_query(&conn, sql, params, self.limits.max_rows).map_err(|e| self.lift(e))
        })
    }

    /// Refuse writes (`exec` / `batch`) while `on` — the core, around a hook
    /// it runs for a simulation. Reads stay allowed.
    pub fn set_read_only(&self, on: bool) {
        self.read_only.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    fn check_writable(&self) -> Result<(), DbError> {
        if self.read_only.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DbError::ReadOnly);
        }
        Ok(())
    }

    /// One statement (`db_exec`), in its own implicit transaction.
    pub fn exec(&self, stmt: &Statement) -> Result<ExecOutcome, DbError> {
        self.check_writable()?;
        let conn = self.lock()?;
        self.as_plugin(|| run_exec(&conn, stmt).map_err(|e| self.lift(e)))
    }

    /// Several statements, all or nothing (`db_batch`). The first failure
    /// rolls the whole batch back and names the statement.
    pub fn batch(&self, stmts: &[Statement]) -> Result<Vec<ExecOutcome>, DbError> {
        self.check_writable()?;
        let conn = self.lock()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| DbError::Sql(e.to_string()))?;
        let res = self.as_plugin(|| {
            let mut out = Vec::with_capacity(stmts.len());
            for (i, s) in stmts.iter().enumerate() {
                match run_exec(&conn, s) {
                    Ok(o) => out.push(o),
                    Err(e) => {
                        return Err(DbError::Batch {
                            index: i,
                            reason: self.lift(e).to_string(),
                        })
                    }
                }
            }
            Ok(out)
        });
        let end = if res.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(e) = conn.execute_batch(end) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(DbError::Sql(format!("{end} failed: {e}")));
        }
        res
    }

    /// Apply the plugin's migrations (`migrations[i]` = version `i + 1`).
    /// Applied ones must be unchanged; pending ones run in order, each in its
    /// own transaction with its record.
    pub fn migrate(&self, migrations: &[String]) -> Result<MigrateOutcome, DbError> {
        let conn = self.lock()?;
        let applied: Vec<(usize, String)> = {
            let mut st = conn
                .prepare(&format!("SELECT version, sql FROM {RESERVED_PREFIX}_migrations ORDER BY version"))
                .map_err(|e| DbError::Sql(e.to_string()))?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, String>(1)?)))
                .map_err(|e| DbError::Sql(e.to_string()))?;
            rows.collect::<Result<_, _>>()
                .map_err(|e| DbError::Sql(e.to_string()))?
        };
        for (version, sql) in &applied {
            match migrations.get(version - 1) {
                None => {
                    return Err(DbError::Migration {
                        version: *version,
                        reason: format!(
                            "applied to the database but no longer shipped by the plugin (it ships {})",
                            migrations.len()
                        ),
                    })
                }
                Some(m) if m.trim() != sql.trim() => {
                    return Err(DbError::Migration {
                        version: *version,
                        reason: "already applied, and modified since — ship a new migration instead".into(),
                    })
                }
                Some(_) => {}
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let mut count = 0;
        for (i, sql) in migrations.iter().enumerate().skip(applied.len()) {
            let version = i + 1;
            let fail = |reason: String| DbError::Migration { version, reason };
            if sql.trim().is_empty() {
                return Err(fail("empty".into()));
            }
            if contains_vacuum(sql) {
                return Err(fail("not allowed for a plugin: VACUUM".into()));
            }
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| fail(e.to_string()))?;
            let res = self
                .as_plugin(|| conn.execute_batch(sql).map_err(|e| self.map_err(e)))
                .and_then(|()| {
                    conn.execute(
                        &format!("INSERT INTO {RESERVED_PREFIX}_migrations (version, sql, applied_at) VALUES (?1, ?2, ?3)"),
                        rusqlite::params![version as i64, sql.trim(), now],
                    )
                    .map(|_| ())
                    .map_err(|e| DbError::Sql(e.to_string()))
                });
            match res {
                Ok(()) => conn
                    .execute_batch("COMMIT")
                    .map_err(|e| fail(e.to_string()))?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(fail(e.to_string()));
                }
            }
            count += 1;
        }
        Ok(MigrateOutcome {
            applied: count,
            version: migrations.len(),
        })
    }

    fn lift(&self, e: StmtError) -> DbError {
        match e {
            StmtError::Sql(e) => self.map_err(e),
            StmtError::Db(e) => e,
        }
    }
}

/// Error from running one statement: already mapped, or raw rusqlite.
enum StmtError {
    Sql(rusqlite::Error),
    Db(DbError),
}

impl From<rusqlite::Error> for StmtError {
    fn from(e: rusqlite::Error) -> Self {
        StmtError::Sql(e)
    }
}

fn map_sql_error(guard: &Guard, timeout_ms: u32, e: rusqlite::Error) -> DbError {
    if let rusqlite::Error::SqliteFailure(err, _) = &e {
        if err.code == rusqlite::ErrorCode::OperationInterrupted {
            return DbError::Timeout(timeout_ms);
        }
        if err.code == rusqlite::ErrorCode::AuthorizationForStatementDenied {
            let what = guard
                .denied
                .lock()
                .ok()
                .and_then(|mut d| d.take())
                .unwrap_or_else(|| "statement".into());
            return DbError::Denied(what);
        }
    }
    if matches!(e, rusqlite::Error::MultipleStatement) {
        return DbError::Sql("one statement per call (use db_batch for several)".into());
    }
    DbError::Sql(e.to_string())
}

fn bind(stmt: &mut rusqlite::Statement<'_>, params: &Params) -> Result<(), StmtError> {
    let bad = |m: String| StmtError::Db(DbError::Params(m));
    let expected = stmt.parameter_count();
    match params {
        Params::Positional(values) => {
            if values.len() != expected {
                return Err(bad(format!(
                    "the statement takes {expected} parameter(s), {} given",
                    values.len()
                )));
            }
            for (i, v) in values.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, json_to_sql(v).map_err(bad)?)?;
            }
        }
        Params::Named(map) => {
            if map.len() != expected {
                return Err(bad(format!(
                    "the statement takes {expected} parameter(s), {} given",
                    map.len()
                )));
            }
            for (k, v) in map {
                let key = if k.starts_with([':', '@', '$']) {
                    k.clone()
                } else {
                    format!(":{k}")
                };
                let idx = stmt
                    .parameter_index(&key)?
                    .ok_or_else(|| bad(format!("the statement has no parameter `{key}`")))?;
                stmt.raw_bind_parameter(idx, json_to_sql(v).map_err(bad)?)?;
            }
        }
    }
    Ok(())
}

/// Prepare exactly one statement: an empty text or trailing statements are
/// an error (rusqlite's `prepare` would silently ignore the tail).
fn prepare_one<'c>(conn: &'c Connection, sql: &str) -> Result<rusqlite::Statement<'c>, StmtError> {
    let one = || StmtError::Db(DbError::Sql("one statement per call (use db_batch for several)".into()));
    let mut batch = rusqlite::Batch::new(conn, sql);
    let first = batch
        .next()?
        .ok_or_else(|| StmtError::Db(DbError::Sql("empty statement".into())))?;
    match batch.next() {
        Ok(None) => Ok(first),
        Ok(Some(_)) | Err(_) => Err(one()),
    }
}

fn run_query(conn: &Connection, sql: &str, params: &Params, max_rows: u32) -> Result<Rows, StmtError> {
    let mut stmt = prepare_one(conn, sql)?;
    if !stmt.readonly() {
        return Err(StmtError::Db(DbError::NotReadOnly));
    }
    bind(&mut stmt, params)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let n = columns.len();
    let mut rows = Vec::new();
    let mut it = stmt.raw_query();
    while let Some(row) = it.next()? {
        if rows.len() as u32 >= max_rows {
            return Err(StmtError::Db(DbError::TooManyRows(max_rows)));
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(sql_to_json(row.get_ref(i)?));
        }
        rows.push(out);
    }
    Ok(Rows { columns, rows })
}

fn run_exec(conn: &Connection, s: &Statement) -> Result<ExecOutcome, StmtError> {
    if is_vacuum(&s.sql) {
        return Err(StmtError::Db(DbError::Denied("VACUUM".into())));
    }
    let mut stmt = prepare_one(conn, &s.sql)?;
    bind(&mut stmt, &s.params)?;
    let changes = stmt.raw_execute()? as u64;
    Ok(ExecOutcome {
        changes,
        last_insert_rowid: conn.last_insert_rowid(),
    })
}

// ---------------------------------------------------------------------------
// Admin side (stationctl plugin db …): a separate read-only connection
// ---------------------------------------------------------------------------

/// `stationctl plugin db <name> info`.
#[derive(Debug, Clone, PartialEq)]
pub struct DbInspect {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub schema_version: u64,
    /// Plugin tables (the core's `_stationd*` excluded) and their row counts.
    pub tables: Vec<(String, u64)>,
}

fn open_read_only(path: &Path) -> Result<(Connection, Arc<Guard>), DbError> {
    let open_err = |reason: String| DbError::Open {
        path: path.display().to_string(),
        reason,
    };
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| open_err(e.to_string()))?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|e| open_err(e.to_string()))?;
    let guard = Arc::new(Guard::default());
    configure(&conn, &guard).map_err(open_err)?;
    Ok((conn, guard))
}

/// Describe a plugin's database; `None` when it has never been created.
pub fn inspect(path: &Path) -> Result<Option<DbInspect>, DbError> {
    if !path.exists() {
        return Ok(None);
    }
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let (conn, _guard) = open_read_only(path)?;
    let sql = |e: rusqlite::Error| DbError::Sql(e.to_string());
    let has_meta: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [format!("{RESERVED_PREFIX}_migrations")],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql)?
        > 0;
    let schema_version = if has_meta {
        conn.query_row(
            &format!("SELECT coalesce(max(version), 0) FROM {RESERVED_PREFIX}_migrations"),
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql)? as u64
    } else {
        0
    };
    let names: Vec<String> = {
        let mut st = conn
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            )
            .map_err(sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };
    let mut tables = Vec::new();
    for name in names.into_iter().filter(|n| !is_reserved(n)) {
        let count: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM \"{}\"", name.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )
            .map_err(sql)?;
        tables.push((name, count as u64));
    }
    Ok(Some(DbInspect {
        path: path.to_path_buf(),
        size_bytes,
        schema_version,
        tables,
    }))
}

/// Read-only query on a plugin's database from the CLI (`stationctl plugin db
/// <name> query`), on a separate read-only connection, under the plugin's
/// confinement and row cap (admin time bound: 10 s).
pub fn query_file(path: &Path, sql: &str, max_rows: u32) -> Result<Rows, DbError> {
    query_file_bounded(path, sql, max_rows, ADMIN_TIMEOUT.as_millis() as u32)
}

/// UI reads use their own deadline while keeping the CLI's existing 10 s limit.
pub fn query_file_bounded(path: &Path, sql: &str, max_rows: u32, timeout_ms: u32) -> Result<Rows, DbError> {
    if !path.exists() {
        return Err(DbError::Open {
            path: path.display().to_string(),
            reason: "the database does not exist (plugin never started, or reset)".into(),
        });
    }
    let (conn, guard) = open_read_only(path)?;
    let timeout = Duration::from_millis(timeout_ms.into());
    conn.busy_timeout(timeout.min(Duration::from_secs(2)))
        .map_err(|e| DbError::Sql(e.to_string()))?;
    *guard.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + timeout);
    guard.plugin_sql.store(true, Ordering::SeqCst);
    run_query(&conn, sql, &Params::default(), max_rows).map_err(|e| match e {
        StmtError::Sql(e) => map_sql_error(&guard, timeout_ms, e),
        StmtError::Db(e) => e,
    })
}

/// Delete a plugin's database (and its rollback journal). `Ok(false)` when
/// there was nothing to delete. The caller guarantees the plugin is stopped.
pub fn remove(path: &Path) -> Result<bool, DbError> {
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    let _ = std::fs::remove_file(PathBuf::from(journal));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(DbError::Sql(format!("cannot delete {}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn open(dir: &Path, name: &str) -> PluginDb {
        PluginDb::open(dir, name, DbLimits::default()).expect("open")
    }

    fn st(sql: &str, params: Json) -> Statement {
        serde_json::from_value(json!({ "sql": sql, "params": params })).unwrap()
    }

    fn schema(db: &PluginDb) {
        db.migrate(&["CREATE TABLE play (media TEXT PRIMARY KEY, n INTEGER NOT NULL)".into()])
            .expect("migrate");
    }

    #[test]
    fn read_only_refuses_writes_but_not_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "ro");
        schema(&db);
        db.set_read_only(true);
        let ins = st("INSERT INTO play (media, n) VALUES (?1, 1)", json!(["a"]));
        assert!(matches!(db.exec(&ins), Err(DbError::ReadOnly)));
        assert!(matches!(db.batch(std::slice::from_ref(&ins)), Err(DbError::ReadOnly)));
        assert!(db.query("SELECT count(*) FROM play", &Params::default()).is_ok());
        db.set_read_only(false);
        assert!(db.exec(&ins).is_ok());
    }

    #[test]
    fn exec_then_query_roundtrip_with_positional_and_named_params() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "stats");
        schema(&db);
        let o = db
            .exec(&st("INSERT INTO play VALUES (?1, ?2)", json!(["a.mp3", 1])))
            .unwrap();
        assert_eq!(o.changes, 1);
        db.exec(&st(
            "INSERT INTO play VALUES (:media, :n) ON CONFLICT(media) DO UPDATE SET n = n + excluded.n",
            json!({ "media": "a.mp3", "n": 2 }),
        ))
        .unwrap();
        let r = db
            .query("SELECT media, n FROM play WHERE n > ?", &Params::Positional(vec![json!(0)]))
            .unwrap();
        assert_eq!(r.columns, vec!["media", "n"]);
        assert_eq!(r.rows, vec![vec![json!("a.mp3"), json!(3)]]);
    }

    #[test]
    fn two_plugins_have_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = open(dir.path(), "a");
        let b = open(dir.path(), "b");
        schema(&a);
        a.exec(&st("INSERT INTO play VALUES ('x', 1)", json!([]))).unwrap();
        let err = b.query("SELECT * FROM play", &Params::default()).unwrap_err();
        assert!(err.to_string().contains("no such table"), "{err}");
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn attach_detach_pragma_and_vacuum_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        let other = dir.path().join("other.db");
        let attach = format!("ATTACH DATABASE '{}' AS x", other.display());
        assert!(matches!(db.exec(&st(&attach, json!([]))), Err(DbError::Denied(_))));
        assert!(!other.exists(), "ATTACH must not create a file");
        let e = db.exec(&st("PRAGMA max_page_count = 999999999", json!([]))).unwrap_err();
        assert_eq!(e, DbError::Denied("PRAGMA max_page_count".into()));
        assert!(matches!(
            db.exec(&st("PRAGMA writable_schema = 1", json!([]))),
            Err(DbError::Denied(_))
        ));
        let into = dir.path().join("copy.db");
        let vac = format!("  /* x */ VACUUM INTO '{}'", into.display());
        assert!(matches!(db.exec(&st(&vac, json!([]))), Err(DbError::Denied(_))));
        assert!(!into.exists(), "VACUUM INTO must not write a file");
        assert!(matches!(
            db.migrate(&[format!("CREATE TABLE t(x); VACUUM INTO '{}'", into.display())]),
            Err(DbError::Migration { .. })
        ));
        // Introspection pragmas stay available.
        schema(&db);
        let r = db.query("PRAGMA table_info(play)", &Params::default()).unwrap();
        assert_eq!(r.rows.len(), 2);
    }

    #[test]
    fn transactions_and_reserved_tables_are_the_cores() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        assert!(matches!(db.exec(&st("BEGIN", json!([]))), Err(DbError::Denied(_))));
        assert!(matches!(db.exec(&st("SAVEPOINT s", json!([]))), Err(DbError::Denied(_))));
        assert!(matches!(
            db.exec(&st("DELETE FROM _stationd_migrations", json!([]))),
            Err(DbError::Denied(_))
        ));
        assert!(matches!(
            db.exec(&st("CREATE TABLE _stationd_mine (x)", json!([]))),
            Err(DbError::Denied(_))
        ));
        // Reading the core's bookkeeping is fine.
        db.query("SELECT count(*) FROM _stationd_migrations", &Params::default())
            .unwrap();
    }

    #[test]
    fn query_is_read_only_and_one_statement() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        schema(&db);
        assert_eq!(
            db.query("INSERT INTO play VALUES ('x', 1)", &Params::default()),
            Err(DbError::NotReadOnly)
        );
        let e = db
            .exec(&st("INSERT INTO play VALUES ('x', 1); DELETE FROM play", json!([])))
            .unwrap_err();
        assert!(e.to_string().contains("one statement"), "{e}");
    }

    #[test]
    fn params_must_all_be_bound() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        schema(&db);
        assert!(matches!(
            db.exec(&st("INSERT INTO play VALUES (?1, ?2)", json!(["x"]))),
            Err(DbError::Params(_))
        ));
        assert!(matches!(
            db.exec(&st("INSERT INTO play VALUES (:m, :n)", json!({ "m": "x", "k": 1 }))),
            Err(DbError::Params(_))
        ));
        assert!(matches!(
            db.exec(&st("INSERT INTO play VALUES (?1, ?2)", json!(["x", [1]]))),
            Err(DbError::Params(_))
        ));
    }

    #[test]
    fn batch_is_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        schema(&db);
        let ok = db
            .batch(&[
                st("INSERT INTO play VALUES ('a', 1)", json!([])),
                st("INSERT INTO play VALUES ('b', 1)", json!([])),
            ])
            .unwrap();
        assert_eq!(ok.len(), 2);
        let err = db
            .batch(&[
                st("INSERT INTO play VALUES ('c', 1)", json!([])),
                st("INSERT INTO play VALUES ('a', 1)", json!([])), // PK conflict
            ])
            .unwrap_err();
        assert!(matches!(err, DbError::Batch { index: 1, .. }), "{err}");
        let r = db.query("SELECT count(*) FROM play", &Params::default()).unwrap();
        assert_eq!(r.rows, vec![vec![json!(2)]], "the failed batch left nothing");
        // The connection is usable after a rollback.
        db.exec(&st("INSERT INTO play VALUES ('c', 1)", json!([]))).unwrap();
    }

    #[test]
    fn migrations_apply_once_and_detect_edits() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = vec!["CREATE TABLE a (x)".to_string()];
        let v2 = vec![v1[0].clone(), "CREATE TABLE b (y)".to_string()];
        {
            let db = open(dir.path(), "p");
            assert_eq!(db.migrate(&v1).unwrap(), MigrateOutcome { applied: 1, version: 1 });
            assert_eq!(db.migrate(&v1).unwrap(), MigrateOutcome { applied: 0, version: 1 });
        }
        let db = open(dir.path(), "p");
        assert_eq!(db.migrate(&v2).unwrap(), MigrateOutcome { applied: 1, version: 2 });
        let edited = vec!["CREATE TABLE a (x, z)".to_string(), v2[1].clone()];
        assert!(matches!(db.migrate(&edited), Err(DbError::Migration { version: 1, .. })));
        assert!(matches!(db.migrate(&v1), Err(DbError::Migration { version: 2, .. })));
        // A failing migration is rolled back with its record.
        let bad = vec![v2[0].clone(), v2[1].clone(), "CREATE TABLE c (z); CREATE TABLE a (x)".into()];
        assert!(matches!(db.migrate(&bad), Err(DbError::Migration { version: 3, .. })));
        let r = db
            .query("SELECT count(*) FROM sqlite_schema WHERE name = 'c'", &Params::default())
            .unwrap();
        assert_eq!(r.rows, vec![vec![json!(0)]]);
        let info = inspect(db.path()).unwrap().unwrap();
        assert_eq!(info.schema_version, 2);
        assert_eq!(info.tables, vec![("a".into(), 0), ("b".into(), 0)]);
    }

    #[test]
    fn a_long_query_is_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let db = PluginDb::open(
            dir.path(),
            "p",
            DbLimits { query_timeout_ms: 50, ..DbLimits::default() },
        )
        .unwrap();
        let t = Instant::now();
        let e = db
            .query(
                "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c) SELECT count(*) FROM c",
                &Params::default(),
            )
            .unwrap_err();
        assert_eq!(e, DbError::Timeout(50));
        assert!(t.elapsed() < Duration::from_secs(5));
        // Next call gets a fresh deadline.
        db.query("SELECT 1", &Params::default()).unwrap();
    }

    #[test]
    fn too_many_rows_is_an_error_not_a_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let db = PluginDb::open(dir.path(), "p", DbLimits { max_rows: 3, ..DbLimits::default() })
            .unwrap();
        let q = "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c LIMIT 4) SELECT i FROM c";
        assert_eq!(db.query(q, &Params::default()), Err(DbError::TooManyRows(3)));
        let q3 = "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c LIMIT 3) SELECT i FROM c";
        assert_eq!(db.query(q3, &Params::default()).unwrap().rows.len(), 3);
    }

    #[test]
    fn the_file_size_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let db = PluginDb::open(dir.path(), "p", DbLimits { max_size_mb: 1, ..DbLimits::default() })
            .unwrap();
        db.migrate(&["CREATE TABLE big (b BLOB)".into()]).unwrap();
        let err = db
            .exec(&st(
                "INSERT INTO big SELECT zeroblob(2 * 1024 * 1024)",
                json!([]),
            ))
            .unwrap_err();
        assert!(err.to_string().contains("full"), "{err}");
        assert!(std::fs::metadata(db.path()).unwrap().len() <= 1024 * 1024);
    }

    #[test]
    fn blobs_roundtrip_as_base64() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        db.migrate(&["CREATE TABLE k (b BLOB)".into()]).unwrap();
        db.exec(&st("INSERT INTO k VALUES (?)", json!([{ "blob": "AAEC" }]))).unwrap();
        let r = db.query("SELECT b FROM k", &Params::default()).unwrap();
        assert_eq!(r.rows, vec![vec![json!({ "blob": "AAEC" })]]);
    }

    #[test]
    fn admin_read_is_read_only_and_confined() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path(), "p");
        schema(&db);
        db.exec(&st("INSERT INTO play VALUES ('x', 1)", json!([]))).unwrap();
        let path = db.path().to_path_buf();
        let r = query_file(&path, "SELECT media FROM play", 10).unwrap();
        assert_eq!(r.rows, vec![vec![json!("x")]]);
        assert_eq!(
            query_file(&path, "DELETE FROM play", 10),
            Err(DbError::NotReadOnly)
        );
        assert!(matches!(
            query_file(&path, "PRAGMA journal_mode", 10),
            Err(DbError::Denied(_))
        ));
        drop(db);
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
        assert_eq!(inspect(&path).unwrap(), None);
    }

    #[test]
    fn names_must_be_file_safe() {
        assert!(validate_name("stop-when-idle_2").is_ok());
        for bad in ["", "../x", "a/b", "a.b", "é"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(PluginDb::open(dir.path(), "../evil", DbLimits::default()).is_err());
    }
}
