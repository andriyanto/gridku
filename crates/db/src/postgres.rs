//! PostgreSQL through the `postgres` client, over TLS with rustls when the server offers it.
//! Native only (the crate leaves it out on wasm32), so reading the clock is fine here.
#![allow(clippy::disallowed_methods, reason = "native only: Instant::now doesn't trap here")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use postgres::error::SqlState;
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::ToSql;
use postgres::{Client, Config};

use crate::{ColumnInfo, DbError, DbValue, Limits, ObjectInfo, ObjectKind, PgConfig, RowKey, SslMode, Stop, TableData};

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
            "SET default_transaction_read_only = on; SET statement_timeout = {ms}; SET DateStyle = ISO, YMD; SET IntervalStyle = iso_8601"
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
    progress: &mut dyn FnMut(usize, usize),
) -> Result<TableData, DbError> {
    let stopped = |s: &Option<Stop>| s.as_ref().is_some_and(|f| f());
    let deadline = Instant::now() + limits.timeout;
    let mut client = connect(cfg, limits.timeout)?;
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
    let count: i64 =
        client.query_one(&format!("SELECT count(*) FROM {from}"), &[]).and_then(|r| r.try_get(0)).map_err(|e| db_err(e, Some(&limits)))?;
    let total = usize::try_from(count).unwrap_or(0).min(max_rows);
    progress(0, total);
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
            progress(out.len(), total.max(out.len()));
        }
        let mut values = Vec::with_capacity(cols.len());
        for (i, c) in cols.iter().enumerate() {
            let text: Option<String> = row.try_get(i).map_err(|e| db_err(e, Some(&limits)))?;
            values.push(value(c, text, limits.max_text, &mut cut_texts));
        }
        out.push(values);
    }
    progress(out.len(), out.len());
    Ok(TableData { columns: cols.into_iter().map(|c| c.info).collect(), rows: out, key, rowids: Vec::new(), truncated, cut_texts })
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
        let objs: Vec<(Option<String>, String, ObjectKind)> = objects(&cfg).unwrap().into_iter().map(|o| (o.schema, o.name, o.kind)).collect();
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
        let t = read(&cfg, "public", "pelanggan", Limits::default(), None, &mut |_, _| {}).unwrap();
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
        let t = read(&cfg, "gudang", "stok", Limits::default(), None, &mut |d, n| seen.push((d, n))).unwrap();
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
}
