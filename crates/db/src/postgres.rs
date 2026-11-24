//! PostgreSQL through the `postgres` client, over TLS with rustls when the server offers it.
//! Native only (the crate leaves it out on wasm32), so reading the clock is fine here.
#![allow(clippy::disallowed_methods, reason = "native only: Instant::now doesn't trap here")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use postgres::error::SqlState;
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::ToSql;
use postgres::{Client, Config};

use crate::{ColumnInfo, DbError, DbValue, Limits, ObjectInfo, ObjectKind, PgConfig, RowChange, RowKey, SslMode, Step, Stop, TableData, WriteTarget};

/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How often (in rows) a read reports its progress and asks whether to stop.
const REPORT_EVERY: usize = 1000;

fn db_err(e: postgres::Error, limits: Option<&Limits>) -> DbError {
    match (e.code(), limits) {
        (Some(code), Some(l)) if *code == SqlState::QUERY_CANCELED => DbError::TimedOut(l.timeout.as_secs()),
        _ => {
            // The server's own message (with its detail) where there is one, else the client's.
            match e.as_db_error() {
                Some(d) => DbError::Database(d.message().to_string()),
                None => DbError::Database(e.to_string()),
            }
        }
    }
}

/// rustls with ring, trusting the system's certificate authorities.
fn tls() -> Result<tokio_postgres_rustls::MakeRustlsConnect, DbError> {
    let mut roots = rustls::RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    for cert in found.certs {
        // A certificate the system has but rustls can't use is left out.
        let _ = roots.add(cert);
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| DbError::Database(format!("TLS: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Connects, read-only: every transaction is read-only, statements stop at the time limit, and
/// dates come as ISO text.
fn connect(cfg: &PgConfig, timeout: Duration) -> Result<Client, DbError> {
    connect_as(cfg, timeout, false)
}

/// Connects as [`connect`] does, but a session that writes when `writable`.
fn connect_as(cfg: &PgConfig, timeout: Duration, writable: bool) -> Result<Client, DbError> {
    let mut c = Config::new();
    c.host(&cfg.host).port(cfg.port).dbname(&cfg.database).user(&cfg.user).connect_timeout(CONNECT_TIMEOUT).application_name("GridKu");
    if !cfg.password.is_empty() {
        c.password(&cfg.password);
    }
    c.ssl_mode(match cfg.ssl {
        SslMode::Disable => postgres::config::SslMode::Disable,
        SslMode::Prefer => postgres::config::SslMode::Prefer,
        SslMode::Require => postgres::config::SslMode::Require,
    });
    let mut client = c.connect(tls()?).map_err(|e| db_err(e, None))?;
    let ms = timeout.as_millis().clamp(1, i32::MAX as u128);
    client
        .batch_execute(&format!(
            "SET default_transaction_read_only = {}; SET statement_timeout = {ms}; SET DateStyle = ISO, YMD; SET IntervalStyle = iso_8601",
            if writable { "off" } else { "on" }
        ))
        .map_err(|e| db_err(e, None))?;
    Ok(client)
}

/// An identifier quoted for SQL: in double quotes, inner ones doubled.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", quote(schema), quote(table))
}

pub(crate) fn objects(cfg: &PgConfig) -> Result<Vec<ObjectInfo>, DbError> {
    let mut client = connect(cfg, Limits::default().timeout)?;
    let rows = client
        .query(
            "SELECT table_schema::text, table_name::text, table_type::text FROM information_schema.tables \
             WHERE table_schema NOT IN ('pg_catalog', 'information_schema') AND table_schema NOT LIKE 'pg\\_toast%' \
             ORDER BY table_schema, (table_type = 'VIEW'), table_name",
            &[],
        )
        .map_err(|e| db_err(e, None))?;
    rows.iter()
        .map(|r| {
            let kind = if r.try_get::<_, String>(2).map_err(|e| db_err(e, None))? == "VIEW" { ObjectKind::View } else { ObjectKind::Table };
            Ok(ObjectInfo { schema: Some(r.try_get(0).map_err(|e| db_err(e, None))?), name: r.try_get(1).map_err(|e| db_err(e, None))?, kind })
        })
        .collect()
}

/// A column with how its values are read and turned into [`DbValue`]s.
struct Col {
    info: ColumnInfo,
    /// `information_schema.columns.data_type` (`integer`, `character varying`, `bytea`…).
    data_type: String,
}

fn columns_of(client: &mut Client, schema: &str, table: &str) -> Result<Vec<Col>, DbError> {
    let rows = client
        .query(
            "SELECT column_name::text, data_type::text, udt_name::text, character_maximum_length::int8, numeric_precision::int8, numeric_scale::int8, (is_nullable = 'YES') \
             FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position",
            &[&schema, &table],
        )
        .map_err(|e| db_err(e, None))?;
    if rows.is_empty() {
        return Err(DbError::NoSuchTable(format!("{schema}.{table}")));
    }
    // The primary key's columns, in key order.
    let key = client
        .query(
            "SELECT a.attname::text, k.ord::int8 FROM pg_index i CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE i.indisprimary AND i.indrelid = (quote_ident($1) || '.' || quote_ident($2))::regclass",
            &[&schema, &table],
        )
        .map_err(|e| db_err(e, None))?;
    let key: Vec<(String, i64)> = key.iter().filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?))).collect();
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let get = |i: usize| -> Result<Option<String>, DbError> { r.try_get(i).map_err(|e| db_err(e, None)) };
        let num = |i: usize| -> Option<i64> { r.try_get::<_, Option<i64>>(i).ok().flatten() };
        let name = get(0)?.unwrap_or_default();
        let data_type = get(1)?.unwrap_or_default();
        let udt = get(2)?.unwrap_or_default();
        let length = match (num(3), num(4), num(5)) {
            (Some(n), ..) => Some(n.to_string()),
            (None, Some(p), Some(s)) if data_type == "numeric" => Some(format!("{p},{s}")),
            (None, Some(p), None) if data_type == "numeric" => Some(p.to_string()),
            _ => None,
        };
        // A user-defined type or an array is named by its type (`mood`, `_int4` → `int4[]`).
        let shown = match data_type.as_str() {
            "USER-DEFINED" => udt.clone(),
            "ARRAY" => format!("{}[]", udt.trim_start_matches('_')),
            _ => data_type.clone(),
        };
        let primary_key = key.iter().find(|(k, _)| *k == name).and_then(|(_, o)| u32::try_from(*o).ok()).unwrap_or(0);
        let nullable = r.try_get::<_, bool>(6).unwrap_or(true);
        out.push(Col { info: ColumnInfo { name, data_type: shown, length, nullable, primary_key }, data_type });
    }
    Ok(out)
}

pub(crate) fn columns(cfg: &PgConfig, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    let mut client = connect(cfg, Limits::default().timeout)?;
    Ok(columns_of(&mut client, schema, table)?.into_iter().map(|c| c.info).collect())
}

/// What `SELECT` reads of a column: its text, the size of a `bytea`, and a time with a time zone
/// in the session's zone without the zone.
fn select_expr(c: &Col) -> String {
    let q = quote(&c.info.name);
    match c.data_type.as_str() {
        "bytea" => format!("octet_length({q})::text"),
        "timestamp with time zone" => format!("to_char({q}, 'YYYY-MM-DD HH24:MI:SS.US')"),
        _ => format!("{q}::text"),
    }
}

/// Significant digits a number holds exactly.
const EXACT_DIGITS: usize = 15;

/// A value as its column's type reads it.
fn value(c: &Col, text: Option<String>, max_text: usize, cut_texts: &mut usize) -> DbValue {
    let Some(t) = text else { return DbValue::Null };
    match c.data_type.as_str() {
        "smallint" | "integer" | "bigint" => t.parse::<i64>().map_or(DbValue::Text(t), DbValue::Integer),
        "real" | "double precision" => t.parse::<f64>().ok().filter(|x| x.is_finite()).map_or(DbValue::Text(t), DbValue::Real),
        "numeric" => {
            let digits = t.trim_start_matches(['-', '+']).trim_start_matches(['0', '.']).chars().filter(char::is_ascii_digit).count();
            match t.parse::<f64>() {
                Ok(x) if x.is_finite() && digits <= EXACT_DIGITS => DbValue::Real(x),
                _ => DbValue::Text(t),
            }
        }
        "boolean" => match t.as_str() {
            "t" | "true" => DbValue::Integer(1),
            "f" | "false" => DbValue::Integer(0),
            _ => DbValue::Text(t),
        },
        "bytea" => t.parse::<usize>().map_or(DbValue::Text(t), DbValue::Blob),
        "timestamp with time zone" => DbValue::Text(t.trim_end_matches(".000000").to_string()),
        _ => match t.char_indices().nth(max_text) {
            Some((at, _)) => {
                *cut_texts += 1;
                DbValue::Text(t.get(..at).unwrap_or(&t).to_string())
            }
            None => DbValue::Text(t),
        },
    }
}

pub(crate) fn read(
    cfg: &PgConfig,
    schema: &str,
    table: &str,
    limits: Limits,
    stop: Option<Stop>,
    progress: &mut dyn FnMut(Step),
) -> Result<TableData, DbError> {
    let stopped = |s: &Option<Stop>| s.as_ref().is_some_and(|f| f());
    let deadline = Instant::now() + limits.timeout;
    progress(Step::Connecting);
    let mut client = connect(cfg, limits.timeout)?;
    if stopped(&stop) {
        return Err(DbError::Cancelled);
    }
    let mut cols = columns_of(&mut client, schema, table)?;
    let mut truncated = false;
    if cols.len() > limits.max_columns {
        cols.truncate(limits.max_columns);
        truncated = true;
    }
    let mut pk: Vec<(u32, usize)> = cols.iter().enumerate().filter(|(_, c)| c.info.primary_key > 0).map(|(i, c)| (c.info.primary_key, i)).collect();
    pk.sort_unstable();
    let key = if pk.is_empty() { RowKey::None } else { RowKey::Columns(pk.into_iter().map(|(_, i)| i).collect()) };
    let width = cols.len().max(1);
    let max_rows = limits.max_rows.min(limits.max_cells / width);
    let from = qualified(schema, table);
    progress(Step::Counting);
    let count: i64 =
        client.query_one(&format!("SELECT count(*) FROM {from}"), &[]).and_then(|r| r.try_get(0)).map_err(|e| db_err(e, Some(&limits)))?;
    let total = usize::try_from(count).unwrap_or(0).min(max_rows);
    progress(Step::Rows { done: 0, total });
    if stopped(&stop) {
        return Err(DbError::Cancelled);
    }
    let query = format!("SELECT {} FROM {from}", cols.iter().map(select_expr).collect::<Vec<_>>().join(", "));
    let mut rows = client.query_raw(&query, std::iter::empty::<&dyn ToSql>()).map_err(|e| db_err(e, Some(&limits)))?;
    let (mut out, mut cut_texts) = (Vec::new(), 0usize);
    while let Some(row) = rows.next().map_err(|e| db_err(e, Some(&limits)))? {
        if out.len() >= max_rows {
            truncated = true;
            break;
        }
        if out.len().is_multiple_of(REPORT_EVERY) && !out.is_empty() {
            if stopped(&stop) {
                return Err(DbError::Cancelled);
            }
            if Instant::now() > deadline {
                return Err(DbError::TimedOut(limits.timeout.as_secs()));
            }
            progress(Step::Rows { done: out.len(), total: total.max(out.len()) });
        }
        let mut values = Vec::with_capacity(cols.len());
        for (i, c) in cols.iter().enumerate() {
            let text: Option<String> = row.try_get(i).map_err(|e| db_err(e, Some(&limits)))?;
            values.push(value(c, text, limits.max_text, &mut cut_texts));
        }
        out.push(values);
    }
    progress(Step::Rows { done: out.len(), total: out.len() });
    Ok(TableData { columns: cols.into_iter().map(|c| c.info).collect(), rows: out, key, rowids: Vec::new(), truncated, cut_texts })
}

/// A value as text for a statement's `$n` (NULL as none). A BLOB, known only by its size, can't
/// be written.
fn param_text(v: &DbValue) -> Result<Option<String>, DbError> {
    Ok(match v {
        DbValue::Null => None,
        DbValue::Integer(n) => Some(n.to_string()),
        DbValue::Real(x) => Some(x.to_string()),
        DbValue::Text(t) => Some(t.clone()),
        DbValue::Blob(_) => return Err(DbError::Database("binary data (bytea) can't be written from a sheet".into())),
    })
}

fn sql_params(v: &[Option<String>]) -> Vec<&(dyn ToSql + Sync)> {
    v.iter().map(|x| x as &(dyn ToSql + Sync)).collect()
}

/// How often (in rows) a write reports its progress and asks whether to stop.
const WRITE_REPORT_EVERY: usize = 200;

pub(crate) fn write(
    cfg: &PgConfig,
    target: WriteTarget<'_>,
    changes: &[RowChange],
    limits: Limits,
    stop: Option<Stop>,
    progress: &mut dyn FnMut(Step),
) -> Result<Vec<Vec<DbValue>>, DbError> {
    use postgres::types::Type;
    let (schema, table) = (target.schema.unwrap_or("public"), target.table);
    let stopped = |s: &Option<Stop>| s.as_ref().is_some_and(|f| f());
    let deadline = Instant::now() + limits.timeout;
    let late = || Instant::now() > deadline;
    progress(Step::Connecting);
    let mut client = connect_as(cfg, limits.timeout, true)?;
    if stopped(&stop) {
        return Err(DbError::Cancelled);
    }
    let all = columns_of(&mut client, schema, table)?;
    let cols = target
        .columns
        .iter()
        .map(|n| {
            all.iter().find(|c| c.info.name == *n).ok_or_else(|| DbError::Database(format!("the column \"{n}\" is no longer in {schema}.{table}")))
        })
        .collect::<Result<Vec<&Col>, _>>()?;
    if cols.is_empty() || target.key.is_empty() {
        return Err(DbError::Database("there is no primary key to find the rows by".into()));
    }
    // Each column's type without its modifier: a value is cast to it, and storing it in the
    // column then checks the length and precision (and fails rather than cutting a text).
    let typed = client
        .query(
            "SELECT attname::text, format_type(atttypid, NULL) FROM pg_attribute \
             WHERE attrelid = (quote_ident($1) || '.' || quote_ident($2))::regclass AND attnum > 0 AND NOT attisdropped",
            &[&schema, &table],
        )
        .map_err(|e| db_err(e, Some(&limits)))?;
    let types: Vec<(String, String)> = typed.iter().filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?))).collect();
    let type_of = |name: &str| types.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone()).unwrap_or_else(|| "text".into());
    let key_cols = target
        .key
        .iter()
        .map(|&k| cols.get(k).copied())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| DbError::Database("the primary key is not among the columns".into()))?;
    let by_key = |from: usize| {
        key_cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{} = CAST(${} AS {})", quote(&c.info.name), from + i + 1, type_of(&c.info.name)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let from = qualified(schema, table);
    let select = format!("SELECT {} FROM {from} WHERE {} FOR UPDATE", cols.iter().map(|c| select_expr(c)).collect::<Vec<_>>().join(", "), by_key(0));
    let mut tx = client.transaction().map_err(|e| db_err(e, Some(&limits)))?;
    let key_types = vec![Type::TEXT; key_cols.len()];
    let st = tx.prepare_typed(&select, &key_types).map_err(|e| db_err(e, Some(&limits)))?;
    let key_params = |ch: &RowChange| -> Result<Vec<Option<String>>, DbError> {
        if ch.key.len() != key_cols.len() || ch.key.contains(&DbValue::Null) {
            return Err(DbError::Database("a row's primary key is missing".into()));
        }
        ch.key.iter().map(param_text).collect()
    };
    let total = changes.len();
    let mut cut_texts = 0usize;
    let mut conflicts = Vec::new();
    for (i, ch) in changes.iter().enumerate() {
        if i.is_multiple_of(WRITE_REPORT_EVERY) {
            if stopped(&stop) {
                return Err(DbError::Cancelled);
            }
            if late() {
                return Err(DbError::TimedOut(limits.timeout.as_secs()));
            }
            progress(Step::Checked { done: i, total });
        }
        let key = key_params(ch)?;
        let same = match tx.query_opt(&st, &sql_params(&key)).map_err(|e| db_err(e, Some(&limits)))? {
            None => false,
            Some(row) => {
                let mut same = true;
                for (c, old, _) in &ch.set {
                    let col = cols.get(*c).ok_or_else(|| DbError::Database("a column to set is not among the columns".into()))?;
                    let text: Option<String> = row.try_get(*c).map_err(|e| db_err(e, Some(&limits)))?;
                    same &= value(col, text, limits.max_text, &mut cut_texts) == *old;
                }
                same
            }
        };
        if !same {
            conflicts.push(i);
        }
    }
    if !conflicts.is_empty() {
        return Err(DbError::Conflict(conflicts));
    }
    progress(Step::Checked { done: total, total });
    let mut out = Vec::with_capacity(total);
    for (i, ch) in changes.iter().enumerate() {
        if i.is_multiple_of(WRITE_REPORT_EVERY) {
            if stopped(&stop) {
                return Err(DbError::Cancelled);
            }
            if late() {
                return Err(DbError::TimedOut(limits.timeout.as_secs()));
            }
            progress(Step::Written { done: i, total });
        }
        let mut sets = Vec::with_capacity(ch.set.len());
        let mut params: Vec<Option<String>> = Vec::with_capacity(ch.set.len() + ch.key.len());
        for (c, _, new) in &ch.set {
            let col = cols.get(*c).ok_or_else(|| DbError::Database("a column to set is not among the columns".into()))?;
            params.push(param_text(new)?);
            sets.push(format!("{} = CAST(${} AS {})", quote(&col.info.name), params.len(), type_of(&col.info.name)));
        }
        if !sets.is_empty() {
            let at = params.len();
            params.extend(key_params(ch)?);
            let sql = format!("UPDATE {from} SET {} WHERE {}", sets.join(", "), by_key(at));
            let types = vec![Type::TEXT; params.len()];
            let upd = tx.prepare_typed(&sql, &types).map_err(|e| db_err(e, Some(&limits)))?;
            tx.execute(&upd, &sql_params(&params)).map_err(|e| db_err(e, Some(&limits)))?;
        }
        let row = tx.query_one(&st, &sql_params(&key_params(ch)?)).map_err(|e| db_err(e, Some(&limits)))?;
        let mut values = Vec::with_capacity(cols.len());
        for (c, col) in cols.iter().enumerate() {
            let text: Option<String> = row.try_get(c).map_err(|e| db_err(e, Some(&limits)))?;
            values.push(value(col, text, limits.max_text, &mut cut_texts));
        }
        out.push(values);
    }
    if stopped(&stop) {
        return Err(DbError::Cancelled);
    }
    tx.commit().map_err(|e| db_err(e, Some(&limits)))?;
    progress(Step::Written { done: total, total });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(data_type: &str) -> Col {
        Col { info: ColumnInfo::default(), data_type: data_type.into() }
    }

    #[test]
    fn values_are_read_by_their_columns_type() {
        let mut cut = 0;
        let mut v = |ty: &str, t: Option<&str>| value(&col(ty), t.map(str::to_string), 5, &mut cut);
        assert_eq!(v("integer", Some("42")), DbValue::Integer(42));
        assert_eq!(v("bigint", Some("9007199254740993")), DbValue::Integer(9007199254740993));
        assert_eq!(v("double precision", Some("1.5")), DbValue::Real(1.5));
        assert_eq!(v("double precision", Some("NaN")), DbValue::Text("NaN".into()));
        assert_eq!(v("numeric", Some("12.50")), DbValue::Real(12.5));
        assert_eq!(v("numeric", Some("12345678901234567.89")), DbValue::Text("12345678901234567.89".into()), "beyond 15 digits stays exact");
        assert_eq!(v("numeric", Some("0.000123")), DbValue::Real(0.000123));
        assert_eq!(v("boolean", Some("t")), DbValue::Integer(1));
        assert_eq!(v("boolean", Some("f")), DbValue::Integer(0));
        assert_eq!(v("bytea", Some("1024")), DbValue::Blob(1024));
        assert_eq!(v("timestamp with time zone", Some("2026-01-31 10:20:00.000000")), DbValue::Text("2026-01-31 10:20:00".into()));
        assert_eq!(v("text", Some("abcdefgh")), DbValue::Text("abcde".into()));
        assert_eq!(v("text", None), DbValue::Null);
        assert_eq!(cut, 1);
    }

    #[test]
    fn select_reads_text_sizes_and_local_times() {
        let mut c = col("bytea");
        c.info.name = "scan \"x\"".into();
        assert_eq!(select_expr(&c), "octet_length(\"scan \"\"x\"\"\")::text");
        c.data_type = "timestamp with time zone".into();
        assert!(select_expr(&c).starts_with("to_char("));
        c.data_type = "integer".into();
        assert_eq!(select_expr(&c), "\"scan \"\"x\"\"\"::text");
        assert_eq!(qualified("my schema", "t"), "\"my schema\".\"t\"");
    }

    #[test]
    fn a_server_that_isnt_there_is_an_error() {
        // Port 1 on this machine: nothing listens there.
        let cfg =
            PgConfig { host: "127.0.0.1".into(), port: 1, database: "x".into(), user: "x".into(), password: String::new(), ssl: SslMode::Disable };
        assert!(matches!(objects(&cfg), Err(DbError::Database(_))));
    }
}

/// Against a real server, when `GRIDKU_TEST_PG` names one (`host:port:user:password`) with a
/// database `toko` made by `docs/databases.md`'s test script; skipped otherwise.
#[cfg(test)]
mod server_tests {
    use super::*;

    fn server() -> Option<PgConfig> {
        let v = std::env::var("GRIDKU_TEST_PG").ok()?;
        let mut it = v.splitn(4, ':');
        let (host, port, user, password) = (it.next()?, it.next()?, it.next()?, it.next()?);
        Some(PgConfig {
            host: host.into(),
            port: port.parse().ok()?,
            database: "toko".into(),
            user: user.into(),
            password: password.into(),
            ssl: SslMode::Prefer,
        })
    }

    #[test]
    fn lists_reads_and_never_writes() {
        let Some(cfg) = server() else { return };
        // Without the schemas the write test makes and drops meanwhile.
        let objs: Vec<(Option<String>, String, ObjectKind)> = objects(&cfg)
            .unwrap()
            .into_iter()
            .filter(|o| !o.schema.as_deref().is_some_and(|s| s.starts_with("uji_")))
            .map(|o| (o.schema, o.name, o.kind))
            .collect();
        assert_eq!(
            objs,
            [
                (Some("gudang".into()), "stok".into(), ObjectKind::Table),
                (Some("public".into()), "pelanggan".into(), ObjectKind::Table),
                (Some("public".into()), "aktif".into(), ObjectKind::View),
            ]
        );
        let cols = columns(&cfg, "public", "pelanggan").unwrap();
        let shown: Vec<(&str, &str, Option<&str>, bool, u32)> =
            cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.length.as_deref(), c.nullable, c.primary_key)).collect();
        assert_eq!(shown[0], ("id", "integer", None, false, 1));
        assert_eq!(shown[1], ("nama", "character varying", Some("60"), false, 0));
        assert_eq!(shown[2], ("saldo", "numeric", Some("12,2"), true, 0));
        assert_eq!(shown[9].1, "mood");
        assert_eq!(shown[10].1, "int4[]");
        let t = read(&cfg, "public", "pelanggan", Limits::default(), None, &mut |_| {}).unwrap();
        assert_eq!(t.key, RowKey::Columns(vec![0]));
        let ana = &t.rows[0];
        assert_eq!(ana[1], DbValue::Text("Ana".into()));
        assert_eq!(ana[2], DbValue::Real(1234.5));
        assert_eq!(ana[3], DbValue::Integer(1));
        assert_eq!(ana[4], DbValue::Text("2025-01-31".into()));
        assert!(matches!(&ana[5], DbValue::Text(t) if t.len() == 19), "{:?}", ana[5]);
        assert_eq!(ana[6], DbValue::Integer(9007199254740993));
        assert_eq!(ana[7], DbValue::Blob(2));
        assert_eq!(ana[8], DbValue::Text("{\"a\": 1}".into()));
        assert_eq!(ana[9], DbValue::Text("senang".into()));
        assert_eq!(t.rows[1][2], DbValue::Null);
        // A composite key in key order, and progress.
        let mut seen = Vec::new();
        let t = read(&cfg, "gudang", "stok", Limits::default(), None, &mut |step| {
            if let Step::Rows { done, total } = step {
                seen.push((done, total))
            }
        })
        .unwrap();
        assert_eq!((t.key, t.rows.len()), (RowKey::Columns(vec![1, 0]), 2500));
        assert_eq!(seen.first(), Some(&(0, 2500)));
        assert_eq!(seen.last(), Some(&(2500, 2500)));
        // Read-only.
        let mut client = connect(&cfg, Duration::from_secs(5)).unwrap();
        assert!(client.batch_execute("DELETE FROM gudang.stok").is_err());
        // A wrong password says so.
        let wrong = PgConfig { password: "salah".into(), ..cfg };
        let e = objects(&wrong).unwrap_err().to_string();
        assert!(e.contains("password"), "{e}");
    }

    #[test]
    fn writes_by_the_primary_key_checks_types_and_conflicts() {
        let Some(cfg) = server() else { return };
        // A table of its own, made and dropped here (the listing test sees only the script's).
        let schema = format!("uji_{}", std::process::id());
        let mut admin = connect_as(&cfg, Duration::from_secs(10), true).unwrap();
        admin
            .batch_execute(&format!(
                "CREATE SCHEMA {schema}; CREATE TABLE {schema}.t (id integer PRIMARY KEY, nama varchar(5) NOT NULL, saldo numeric(6,2), lahir date, aktif boolean, at timestamptz); \
                 INSERT INTO {schema}.t VALUES (1, 'Ana', 10.5, '2025-01-31', true, '2026-01-02 03:04:05'), (2, 'Budi', NULL, NULL, false, NULL);"
            ))
            .unwrap();
        let cols: Vec<String> = ["id", "nama", "saldo", "lahir", "aktif", "at"].map(String::from).to_vec();
        let target = WriteTarget { schema: Some(&schema), table: "t", columns: &cols, key: &[0] };
        let original = read(&cfg, &schema, "t", Limits::default(), None, &mut |_| {}).unwrap();
        let ana = original.rows[0].clone();
        let run = |changes: &[RowChange]| write(&cfg, target, changes, Limits::default(), None, &mut |_| {});
        // Values are cast to the column's type, and come back as the database stores them.
        let ok = [RowChange {
            key: vec![DbValue::Integer(1)],
            set: vec![
                (1, ana[1].clone(), DbValue::Text("Ani".into())),
                (2, ana[2].clone(), DbValue::Real(12.345)),
                (3, ana[3].clone(), DbValue::Text("2025-02-28".into())),
                (4, ana[4].clone(), DbValue::Integer(0)),
                (5, ana[5].clone(), DbValue::Text("2026-03-04 05:06:07".into())),
            ],
        }];
        let back = run(&ok).unwrap();
        assert_eq!(back[0][1], DbValue::Text("Ani".into()));
        assert_eq!(back[0][2], DbValue::Real(12.35), "rounded to the column's scale");
        assert_eq!(back[0][3], DbValue::Text("2025-02-28".into()));
        assert_eq!(back[0][4], DbValue::Integer(0));
        assert_eq!(back[0][5], DbValue::Text("2026-03-04 05:06:07".into()));
        // The same change again: the row isn't as it was read any more.
        assert!(matches!(run(&ok), Err(DbError::Conflict(rows)) if rows == [0]));
        // Too long for varchar(5), and NOT NULL: the database says so and nothing is written.
        let bad = |v: DbValue| [RowChange { key: vec![DbValue::Integer(2)], set: vec![(1, DbValue::Text("Budi".into()), v)] }];
        let e = run(&bad(DbValue::Text("Bambang".into()))).unwrap_err().to_string();
        assert!(e.contains("too long"), "{e}");
        assert!(run(&bad(DbValue::Null)).is_err());
        let now = read(&cfg, &schema, "t", Limits::default(), None, &mut |_| {}).unwrap();
        assert_eq!(now.rows.iter().find(|r| r[0] == DbValue::Integer(2)).map(|r| r[1].clone()), Some(DbValue::Text("Budi".into())));
        admin.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).unwrap();
    }
}
