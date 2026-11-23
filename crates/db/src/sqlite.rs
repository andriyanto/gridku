//! SQLite through `rusqlite`, with SQLite built in. Native only (the crate leaves it out on
//! wasm32), so reading the clock for the time limit is fine here.
#![allow(clippy::disallowed_methods, reason = "native only: Instant::now doesn't trap here")]

use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};

use crate::{ColumnInfo, DbError, DbValue, Limits, ObjectInfo, ObjectKind, RowKey, Step, Stop, TableData, split_type};

/// SQLite's own error as ours. An interrupted statement was cancelled, or ran out of time.
fn db_err(e: rusqlite::Error, limits: Option<&Limits>) -> DbError {
    match (&e, limits) {
        (rusqlite::Error::SqliteFailure(f, _), Some(l)) if f.code == ErrorCode::OperationInterrupted => DbError::TimedOut(l.timeout.as_secs()),
        _ => DbError::Database(e.to_string()),
    }
}

/// How often (in rows) a read reports its progress and asks whether to stop.
const REPORT_EVERY: usize = 1000;

/// Opens the database read-only. It must be an existing file: nothing is created. Its schema
/// isn't trusted (functions it names run with the least rights), and no statement can write.
fn open(path: &str) -> Result<Connection, DbError> {
    if !std::path::Path::new(path).is_file() {
        return Err(DbError::Database(format!("there is no database file at {path}")));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|e| db_err(e, None))?;
    conn.pragma_update(None, "trusted_schema", false).map_err(|e| db_err(e, None))?;
    conn.pragma_update(None, "query_only", true).map_err(|e| db_err(e, None))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| db_err(e, None))?;
    // A file that isn't a database says so here rather than at the first real query.
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(())).map_err(|e| db_err(e, None))?;
    Ok(conn)
}

/// An identifier quoted for SQL: in double quotes, inner ones doubled.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn objects(path: &str) -> Result<Vec<ObjectInfo>, DbError> {
    let conn = open(path)?;
    let mut st = conn
        .prepare(
            "SELECT name, type FROM sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name",
        )
        .map_err(|e| db_err(e, None))?;
    let rows = st
        .query_map([], |r| {
            let kind = if r.get::<_, String>(1)? == "view" { ObjectKind::View } else { ObjectKind::Table };
            Ok(ObjectInfo { schema: None, name: r.get(0)?, kind })
        })
        .map_err(|e| db_err(e, None))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| db_err(e, None))
}

/// The table or view `table` as the catalogue has it: its kind and its `CREATE` statement.
fn object(conn: &Connection, table: &str) -> Result<(ObjectKind, String), DbError> {
    conn.query_row("SELECT type, coalesce(sql, '') FROM sqlite_schema WHERE type IN ('table', 'view') AND name = ?1", [table], |r| {
        Ok((if r.get::<_, String>(0)? == "view" { ObjectKind::View } else { ObjectKind::Table }, r.get::<_, String>(1)?))
    })
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NoSuchTable(table.to_string()),
        e => db_err(e, None),
    })
}

fn columns_of(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    object(conn, table)?;
    let mut st = conn.prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid").map_err(|e| db_err(e, None))?;
    let rows = st
        .query_map([table], |r| {
            let (data_type, length) = split_type(&r.get::<_, String>(1)?);
            Ok(ColumnInfo {
                name: r.get(0)?,
                data_type,
                length,
                nullable: r.get::<_, i64>(2)? == 0,
                primary_key: u32::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
            })
        })
        .map_err(|e| db_err(e, None))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| db_err(e, None))
}

pub(crate) fn columns(path: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    columns_of(&open(path)?, table)
}

/// The name `rowid` can be read by in `table`: the first of SQLite's three that no column takes.
/// `None` for a view, a virtual table or a `WITHOUT ROWID` table, which have none to read.
fn rowid_alias(kind: ObjectKind, sql: &str, columns: &[ColumnInfo]) -> Option<&'static str> {
    let upper = sql.to_ascii_uppercase();
    if kind == ObjectKind::View
        || upper.trim_start().starts_with("CREATE VIRTUAL")
        || upper.split_whitespace().collect::<Vec<_>>().windows(2).any(|w| w == ["WITHOUT", "ROWID"])
    {
        return None;
    }
    ["rowid", "_rowid_", "oid"].into_iter().find(|a| !columns.iter().any(|c| c.name.eq_ignore_ascii_case(a)))
}

fn cut(text: &str, max: usize, cut_texts: &mut usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => {
            *cut_texts += 1;
            text.get(..at).unwrap_or(text).to_string()
        }
        None => text.to_string(),
    }
}

pub(crate) fn read(path: &str, table: &str, limits: Limits, stop: Option<Stop>, progress: &mut dyn FnMut(Step)) -> Result<TableData, DbError> {
    let stopped = |s: &Option<Stop>| s.as_ref().is_some_and(|f| f());
    let cancelled = |e: DbError, s: &Option<Stop>| if stopped(s) { DbError::Cancelled } else { e };
    let mut conn = open(path)?;
    let (kind, sql) = object(&conn, table)?;
    let mut columns = columns_of(&conn, table)?;
    let mut truncated = false;
    if columns.len() > limits.max_columns {
        columns.truncate(limits.max_columns);
        truncated = true;
    }
    let mut pk: Vec<(u32, usize)> = columns.iter().enumerate().filter(|(_, c)| c.primary_key > 0).map(|(i, c)| (c.primary_key, i)).collect();
    pk.sort_unstable();
    let alias = if pk.is_empty() { rowid_alias(kind, &sql, &columns) } else { None };
    let key = match (pk.is_empty(), alias) {
        (false, _) => RowKey::Columns(pk.into_iter().map(|(_, i)| i).collect()),
        (true, Some(_)) => RowKey::Rowid,
        (true, None) => RowKey::None,
    };
    let mut select: Vec<String> = alias.map(|a| a.to_string()).into_iter().collect();
    select.extend(columns.iter().map(|c| quote(&c.name)));
    if select.is_empty() {
        return Ok(TableData { columns, rows: Vec::new(), key, rowids: Vec::new(), truncated, cut_texts: 0 });
    }
    let query = format!("SELECT {} FROM {}", select.join(", "), quote(table));
    // The time limit and the request to stop: SQLite asks every so many steps whether to go on.
    let deadline = Instant::now() + limits.timeout;
    let asked = stop.clone();
    conn.progress_handler(10_000, Some(move || Instant::now() > deadline || asked.as_ref().is_some_and(|f| f()))).map_err(|e| db_err(e, None))?;
    let width = columns.len().max(1);
    let max_rows = limits.max_rows.min(limits.max_cells / width);
    // How many rows there are, for the progress (at most the rows read).
    progress(Step::Counting);
    let count: i64 = conn
        .query_row(&format!("SELECT count(*) FROM {}", quote(table)), [], |r| r.get(0))
        .map_err(|e| cancelled(db_err(e, Some(&limits)), &stop))?;
    let total = usize::try_from(count).unwrap_or(0).min(max_rows);
    progress(Step::Rows { done: 0, total });
    let mut st = conn.prepare(&query).map_err(|e| cancelled(db_err(e, Some(&limits)), &stop))?;
    let mut rows = st.query([]).map_err(|e| cancelled(db_err(e, Some(&limits)), &stop))?;
    let offset = usize::from(alias.is_some());
    let (mut out, mut rowids, mut cut_texts) = (Vec::new(), Vec::new(), 0usize);
    while let Some(row) = rows.next().map_err(|e| cancelled(db_err(e, Some(&limits)), &stop))? {
        if out.len() >= max_rows {
            truncated = true;
            break;
        }
        if out.len().is_multiple_of(REPORT_EVERY) && !out.is_empty() {
            if stopped(&stop) {
                return Err(DbError::Cancelled);
            }
            progress(Step::Rows { done: out.len(), total: total.max(out.len()) });
        }
        if alias.is_some() {
            rowids.push(row.get::<_, i64>(0).map_err(|e| db_err(e, Some(&limits)))?);
        }
        let mut values = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            let v = match row.get_ref(i + offset).map_err(|e| db_err(e, Some(&limits)))? {
                ValueRef::Null => DbValue::Null,
                ValueRef::Integer(n) => DbValue::Integer(n),
                ValueRef::Real(x) => DbValue::Real(x),
                ValueRef::Text(t) => DbValue::Text(cut(&String::from_utf8_lossy(t), limits.max_text, &mut cut_texts)),
                ValueRef::Blob(b) => DbValue::Blob(b.len()),
            };
            values.push(v);
        }
        out.push(values);
    }
    progress(Step::Rows { done: out.len(), total: out.len() });
    Ok(TableData { columns, rows: out, key, rowids, truncated, cut_texts })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database file made for one test, removed when dropped.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str, sql: &str) -> TempDb {
            let path = std::env::temp_dir().join(format!("gridku-db-{name}-{}.sqlite", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(sql).unwrap();
            TempDb(path)
        }
        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const SHOP: &str = "
        CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR(50) NOT NULL, balance DECIMAL(10,2), note TEXT, photo BLOB);
        INSERT INTO customers VALUES (1, 'Ana', 12.5, NULL, x'00010203'), (2, 'Budi \"B\"', -3, 'hi', NULL);
        CREATE TABLE log (msg TEXT, at DATETIME);
        INSERT INTO log VALUES ('a', '2026-01-02 03:04:05'), ('b', '2026-02-03');
        CREATE TABLE pairs (a TEXT, b TEXT, PRIMARY KEY (b, a)) WITHOUT ROWID;
        INSERT INTO pairs VALUES ('x', 'y');
        CREATE TABLE \"odd \"\"name\"\"\" (rowid TEXT, oid TEXT);
        INSERT INTO \"odd \"\"name\"\"\" VALUES ('r', 'o');
        CREATE VIEW rich AS SELECT name FROM customers WHERE balance > 0;
    ";

    fn read_all(path: &str, table: &str, limits: Limits) -> Result<TableData, DbError> {
        read(path, table, limits, None, &mut |_| {})
    }

    #[test]
    fn lists_tables_then_views_without_sqlites_own() {
        let db = TempDb::new("objects", SHOP);
        let names: Vec<(String, ObjectKind)> = objects(db.path()).unwrap().into_iter().map(|o| (o.name, o.kind)).collect();
        assert_eq!(
            names,
            [
                ("customers".to_string(), ObjectKind::Table),
                ("log".into(), ObjectKind::Table),
                ("odd \"name\"".into(), ObjectKind::Table),
                ("pairs".into(), ObjectKind::Table),
                ("rich".into(), ObjectKind::View)
            ]
        );
    }

    #[test]
    fn columns_have_type_length_nullability_and_key() {
        let db = TempDb::new("columns", SHOP);
        let cols = columns(db.path(), "customers").unwrap();
        assert_eq!(cols[0], ColumnInfo { name: "id".into(), data_type: "INTEGER".into(), length: None, nullable: true, primary_key: 1 });
        assert_eq!(
            cols[1],
            ColumnInfo { name: "name".into(), data_type: "VARCHAR".into(), length: Some("50".into()), nullable: false, primary_key: 0 }
        );
        assert_eq!((cols[2].data_type.as_str(), cols[2].length.as_deref()), ("DECIMAL", Some("10,2")));
        assert!(matches!(columns(db.path(), "nope"), Err(DbError::NoSuchTable(_))));
    }

    #[test]
    fn reads_values_and_how_rows_are_told_apart() {
        let db = TempDb::new("read", SHOP);
        let t = read_all(db.path(), "customers", Limits::default()).unwrap();
        assert_eq!(t.key, RowKey::Columns(vec![0]));
        assert_eq!(t.rows[0], [DbValue::Integer(1), DbValue::Text("Ana".into()), DbValue::Real(12.5), DbValue::Null, DbValue::Blob(4)]);
        assert_eq!(t.rows[1][1], DbValue::Text("Budi \"B\"".into()));
        // No primary key: the rowid, read alongside.
        let t = read_all(db.path(), "log", Limits::default()).unwrap();
        assert_eq!((t.key, t.rowids), (RowKey::Rowid, vec![1, 2]));
        // A composite key, in key order.
        let t = read_all(db.path(), "pairs", Limits::default()).unwrap();
        assert_eq!(t.key, RowKey::Columns(vec![1, 0]));
        // Columns called rowid and oid: `_rowid_` is the one left.
        let t = read_all(db.path(), "odd \"name\"", Limits::default()).unwrap();
        assert_eq!((t.key, t.rowids.len(), t.rows[0].clone()), (RowKey::Rowid, 1, vec![DbValue::Text("r".into()), DbValue::Text("o".into())]));
        // A view has no key.
        let t = read_all(db.path(), "rich", Limits::default()).unwrap();
        assert_eq!((t.key, t.rows.len()), (RowKey::None, 1));
    }

    #[test]
    fn reads_are_bounded() {
        let db = TempDb::new("bounds", SHOP);
        let limits = Limits { max_rows: 1, max_text: 2, ..Limits::default() };
        let t = read_all(db.path(), "customers", limits).unwrap();
        assert!(t.truncated);
        assert_eq!((t.rows.len(), t.rows[0][1].clone(), t.cut_texts), (1, DbValue::Text("An".into()), 1));
        let t = read_all(db.path(), "customers", Limits { max_columns: 2, ..Limits::default() }).unwrap();
        assert_eq!((t.columns.len(), t.rows[0].len(), t.truncated), (2, 2, true));
        let t = read_all(db.path(), "customers", Limits { timeout: std::time::Duration::ZERO, ..Limits::default() });
        assert!(matches!(t, Err(DbError::TimedOut(0))) || t.is_ok(), "{t:?}");
    }

    #[test]
    fn a_read_reports_progress_and_stops_when_asked() {
        let mut sql = String::from("CREATE TABLE big (n INTEGER);");
        sql.push_str("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 2500) INSERT INTO big SELECT x FROM c;");
        let db = TempDb::new("progress", &sql);
        let mut seen = Vec::new();
        let t = read(db.path(), "big", Limits::default(), None, &mut |step| {
            if let Step::Rows { done, total } = step {
                seen.push((done, total))
            }
        })
        .unwrap();
        assert_eq!(t.rows.len(), 2500);
        assert_eq!(seen, [(0, 2500), (1000, 2500), (2000, 2500), (2500, 2500)]);
        // Asked to stop: it stops, whatever it was doing.
        let stop: Stop = std::sync::Arc::new(|| true);
        assert!(matches!(read(db.path(), "big", Limits::default(), Some(stop), &mut |_| {}), Err(DbError::Cancelled)));
    }

    #[test]
    fn a_missing_or_foreign_file_is_an_error_and_nothing_is_created() {
        let missing = std::env::temp_dir().join(format!("gridku-db-missing-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        assert!(objects(missing.to_str().unwrap()).is_err());
        assert!(!missing.exists(), "no database is created");
        let text = std::env::temp_dir().join(format!("gridku-db-text-{}.txt", std::process::id()));
        std::fs::write(&text, "not a database at all, just some text").unwrap();
        assert!(objects(text.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&text);
    }

    #[test]
    fn nothing_is_written() {
        let db = TempDb::new("readonly", SHOP);
        let conn = open(db.path()).unwrap();
        assert!(conn.execute("DELETE FROM customers", []).is_err());
    }
}
