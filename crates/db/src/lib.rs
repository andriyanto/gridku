//! GridKu's database sources (Data › DB Connect): list the tables and views of a database, the
//! columns of one, and read one, for the engine to load into a sheet.
//!
//! SQLite and PostgreSQL. Reading changes nothing: no statement is run but the catalogue
//! queries, a `count(*)` and one `SELECT` of the table asked for. A SQLite file is opened
//! read-only, never created, and is hostile input: the schema it holds is not trusted
//! (`trusted_schema=OFF`) and no extension can be loaded. A PostgreSQL session is read-only
//! (`default_transaction_read_only`), over TLS when the server offers it (rustls, the system's
//! certificate authorities), and its password is only ever held for the call. Every read is
//! bounded in rows, cells, text length and time.
//!
//! Writing ([`sqlite_write`], [`pg_write`]) only ever runs `UPDATE`s of rows found by their
//! primary key, in one transaction: each row is first read again (and locked) and compared with
//! the values it had when it was loaded, and if any changed in the database since, or is gone,
//! nothing is written ([`DbError::Conflict`]).
//!
//! Each column keeps its declared type, split into a type and a length (`VARCHAR(50)` is
//! `VARCHAR` and `50`), whether it may be NULL and its place in the primary key; each table read
//! keeps how its rows are told apart (its primary key, else SQLite's `rowid`, with each row's),
//! so a later stage can write changes back.
//!
//! Not on the web: SQLite's C library does not build for wasm32 here, so there every function
//! returns [`DbError::Unavailable`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

/// Where a read has got, for a progress bar: connecting, counting the rows, then the rows read
/// of how many there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Connecting,
    Counting,
    Rows {
        done: usize,
        total: usize,
    },
    /// Writing: the rows checked against the database so far, of how many are to be written.
    Checked {
        done: usize,
        total: usize,
    },
    /// Writing: the rows written so far.
    Written {
        done: usize,
        total: usize,
    },
}

/// Asked now and then while a read runs: `true` stops it ([`DbError::Cancelled`]).
pub type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

#[cfg(not(target_arch = "wasm32"))]
mod postgres;
#[cfg(not(target_arch = "wasm32"))]
mod sqlite;

/// Why a database could not be read.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Database(String),
    #[error("there is no table or view called \"{0}\" in this database")]
    NoSuchTable(String),
    #[error("reading the table took longer than {0} seconds and was stopped")]
    TimedOut(u64),
    #[error("reading the table was cancelled")]
    Cancelled,
    #[error("databases can't be read in the web version")]
    Unavailable,
    /// Writing found rows that changed in the database since they were read, or are gone: their
    /// indexes among the changes. Nothing was written.
    #[error("{} rows were changed in the database since they were loaded; nothing was saved", .0.len())]
    Conflict(Vec<usize>),
}

/// A table or a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
}

impl ObjectKind {
    pub fn name(self) -> &'static str {
        match self {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
        }
    }
}

/// One table or view of a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The schema it is in (`None` for SQLite's main database).
    pub schema: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
}

/// One column of a table or view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// The declared type without its length (`VARCHAR`), as the database spells it; empty when
    /// none was declared.
    pub data_type: String,
    /// The length or precision the type declares (`50`, `10,2`).
    pub length: Option<String>,
    pub nullable: bool,
    /// Its place in the primary key, from 1; 0 when it is not part of it.
    pub primary_key: u32,
}

/// Splits a declared type into the type and its length: `VARCHAR(50)` → (`VARCHAR`, `50`),
/// `DECIMAL(10, 2)` → (`DECIMAL`, `10,2`), `TEXT` → (`TEXT`, none).
pub fn split_type(declared: &str) -> (String, Option<String>) {
    let d = declared.trim();
    match (d.find('('), d.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            let length: String = d.get(open + 1..close).unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
            let base = d.get(..open).unwrap_or(d).trim().to_string();
            (base, Some(length).filter(|l| !l.is_empty()))
        }
        _ => (d.to_string(), None),
    }
}

/// One value read from the database.
#[derive(Clone, Debug, PartialEq)]
pub enum DbValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// A BLOB, by its size in bytes (its bytes are not read into a sheet).
    Blob(usize),
}

/// How the rows of a table read are told apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    /// By the primary key: the indexes of its columns among the columns read, in key order.
    Columns(Vec<usize>),
    /// By SQLite's `rowid`: [`TableData::rowids`] holds each row's.
    Rowid,
    /// Not at all (a view, or a table with neither).
    None,
}

/// What reading a table gives.
#[derive(Clone, Debug, PartialEq)]
pub struct TableData {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<DbValue>>,
    pub key: RowKey,
    /// Each row's `rowid` when [`TableData::key`] is [`RowKey::Rowid`]; empty otherwise.
    pub rowids: Vec<i64>,
    /// The table has more rows than [`Limits::max_rows`] (or cells than [`Limits::max_cells`]):
    /// only the first were read.
    pub truncated: bool,
    /// Texts longer than [`Limits::max_text`] characters, cut to it.
    pub cut_texts: usize,
}

/// One row to write: found by its primary key, with the columns to set.
#[derive(Clone, Debug, PartialEq)]
pub struct RowChange {
    /// The primary key's values as they were read, in key order.
    pub key: Vec<DbValue>,
    /// Each column to set: its index among the columns written, its value as it was read, and
    /// the new value (never a [`DbValue::Blob`]).
    pub set: Vec<(usize, DbValue, DbValue)>,
}

/// The table to write to (PostgreSQL's in `schema`, `public` when none): its columns, in the
/// order the values of [`RowChange`] and the rows read back use, and which of them make the
/// primary key, in key order.
#[derive(Clone, Copy, Debug)]
pub struct WriteTarget<'a> {
    pub schema: Option<&'a str>,
    pub table: &'a str,
    pub columns: &'a [String],
    pub key: &'a [usize],
}

/// The bounds of one read.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_cells: usize,
    /// Characters kept of one text (a cell holds at most 32,767).
    pub max_text: usize,
    pub timeout: Duration,
}

impl Default for Limits {
    /// A sheet's rows below a header row and its columns, at most 20 million cells, a cell's
    /// 32,767 characters, two minutes.
    fn default() -> Self {
        Limits { max_rows: 1_048_575, max_columns: 16_384, max_cells: 20_000_000, max_text: 32_767, timeout: Duration::from_secs(120) }
    }
}

/// Whether a PostgreSQL connection uses TLS ([libpq's `sslmode`](https://www.postgresql.org/docs/current/libpq-ssl.html)
/// names). When it does, the server's certificate is always checked against the system's
/// certificate authorities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    /// Never.
    Disable,
    /// When the server offers it.
    #[default]
    Prefer,
    /// Always: a server without TLS is refused.
    Require,
}

impl SslMode {
    pub fn id(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
        }
    }

    pub fn parse(s: &str) -> Option<SslMode> {
        match s.to_ascii_lowercase().as_str() {
            "disable" => Some(SslMode::Disable),
            "prefer" => Some(SslMode::Prefer),
            "require" => Some(SslMode::Require),
            _ => None,
        }
    }
}

/// Where a PostgreSQL database is, and who connects. The password is held only while a call
/// needs it; [`PgConfig::url`] leaves it out.
#[derive(Clone, PartialEq, Eq)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
    pub ssl: SslMode,
}

impl std::fmt::Debug for PgConfig {
    /// Without the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url())
    }
}

/// `%`-encodes what a URL's user, host or database part can't hold as it is.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(v) = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl PgConfig {
    /// The connection as a URL without the password: `postgresql://user@host:5432/db?sslmode=prefer`.
    pub fn url(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { encode(&self.host) };
        format!("postgresql://{}@{host}:{}/{}?sslmode={}", encode(&self.user), self.port, encode(&self.database), self.ssl.id())
    }

    /// A URL [`PgConfig::url`] wrote, with no password.
    pub fn from_url(url: &str) -> Option<PgConfig> {
        let rest = url.strip_prefix("postgresql://").or_else(|| url.strip_prefix("postgres://"))?;
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (user, rest) = rest.split_once('@')?;
        let (hostport, database) = rest.split_once('/')?;
        let (host, port) = match hostport.strip_prefix('[') {
            Some(v6) => {
                let (h, p) = v6.split_once(']')?;
                (h.to_string(), p.strip_prefix(':').unwrap_or(""))
            }
            None => {
                let (h, p) = hostport.rsplit_once(':').unwrap_or((hostport, ""));
                (decode(h), p)
            }
        };
        let port = if port.is_empty() { 5432 } else { port.parse().ok()? };
        let ssl = query.split('&').find_map(|kv| kv.strip_prefix("sslmode=")).and_then(SslMode::parse).unwrap_or_default();
        Some(PgConfig { host, port, database: decode(database), user: decode(user), password: String::new(), ssl })
    }
}

/// The tables and views of the PostgreSQL database `cfg` names, schema by schema (the system's
/// own left out), tables first in each.
pub fn pg_objects(cfg: &PgConfig) -> Result<Vec<ObjectInfo>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return postgres::objects(cfg);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = cfg;
        Err(DbError::Unavailable)
    }
}

/// The columns of the table or view `schema.table` of the PostgreSQL database `cfg` names.
pub fn pg_columns(cfg: &PgConfig, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return postgres::columns(cfg, schema, table);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (cfg, schema, table);
        Err(DbError::Unavailable)
    }
}

/// Reads `schema.table` of the PostgreSQL database `cfg` names, as [`sqlite_read`] reads a SQLite
/// table. Rows are told apart by the primary key, or not at all (there is no stable row id).
pub fn pg_read(
    cfg: &PgConfig,
    schema: &str,
    table: &str,
    limits: Limits,
    stop: Option<Stop>,
    progress: &mut dyn FnMut(Step),
) -> Result<TableData, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return postgres::read(cfg, schema, table, limits, stop, progress);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (cfg, schema, table, limits, stop, progress);
        Err(DbError::Unavailable)
    }
}

/// The tables and views of the SQLite database at `path`, tables first, each by name.
pub fn sqlite_objects(path: &str) -> Result<Vec<ObjectInfo>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return sqlite::objects(path);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err(DbError::Unavailable)
    }
}

/// The columns of the table or view `table` of the SQLite database at `path`.
pub fn sqlite_columns(path: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return sqlite::columns(path, table);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, table);
        Err(DbError::Unavailable)
    }
}

/// Reads the table or view `table` of the SQLite database at `path`, within `limits`. `stop`,
/// when given, is asked now and then whether to stop; `progress` hears each [`Step`]: counting the
/// rows, then how many have been read of how many there are (at most the rows read), every so
/// many rows.
pub fn sqlite_read(path: &str, table: &str, limits: Limits, stop: Option<Stop>, progress: &mut dyn FnMut(Step)) -> Result<TableData, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return sqlite::read(path, table, limits, stop, progress);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, table, limits, stop, progress);
        Err(DbError::Unavailable)
    }
}

/// Writes `changes` to the table `target.table` of the SQLite database at `path`, in one transaction,
/// and gives each changed row as it reads back afterwards (all of `target.columns`), in the
/// order of `changes`. Within `limits.timeout`; `stop` and `progress` as for [`sqlite_read`].
pub fn sqlite_write(
    path: &str,
    target: WriteTarget<'_>,
    changes: &[RowChange],
    limits: Limits,
    stop: Option<Stop>,
    progress: &mut dyn FnMut(Step),
) -> Result<Vec<Vec<DbValue>>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return sqlite::write(path, target, changes, limits, stop, progress);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, target, changes, limits, stop, progress);
        Err(DbError::Unavailable)
    }
}

/// Writes `changes` to `target.schema.table` of the PostgreSQL database `cfg` names, as [`sqlite_write`].
pub fn pg_write(
    cfg: &PgConfig,
    target: WriteTarget<'_>,
    changes: &[RowChange],
    limits: Limits,
    stop: Option<Stop>,
    progress: &mut dyn FnMut(Step),
) -> Result<Vec<Vec<DbValue>>, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return postgres::write(cfg, target, changes, limits, stop, progress);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (cfg, target, changes, limits, stop, progress);
        Err(DbError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_urls_leave_the_password_out_and_read_back() {
        let cfg = PgConfig {
            host: "db.example.com".into(),
            port: 6543,
            database: "toko lama".into(),
            user: "andri@kantor".into(),
            password: "rahasia".into(),
            ssl: SslMode::Require,
        };
        let url = cfg.url();
        assert_eq!(url, "postgresql://andri%40kantor@db.example.com:6543/toko%20lama?sslmode=require");
        assert!(!url.contains("rahasia") && !format!("{cfg:?}").contains("rahasia"));
        let back = PgConfig::from_url(&url).unwrap();
        assert_eq!(back, PgConfig { password: String::new(), ..cfg });
        let v6 = PgConfig { host: "::1".into(), port: 5432, database: "d".into(), user: "u".into(), password: String::new(), ssl: SslMode::Prefer };
        assert_eq!(PgConfig::from_url(&v6.url()), Some(v6));
        assert_eq!(PgConfig::from_url("toko.db"), None);
    }

    #[test]
    fn declared_types_split_into_type_and_length() {
        assert_eq!(split_type("VARCHAR(50)"), ("VARCHAR".into(), Some("50".into())));
        assert_eq!(split_type("decimal( 10, 2 )"), ("decimal".into(), Some("10,2".into())));
        assert_eq!(split_type("TEXT"), ("TEXT".into(), None));
        assert_eq!(split_type(""), (String::new(), None));
        assert_eq!(split_type("CHAR()"), ("CHAR".into(), None));
        assert_eq!(split_type("odd)("), ("odd)(".into(), None));
    }
}
