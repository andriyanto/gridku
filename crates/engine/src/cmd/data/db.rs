//! Data › DB Connect: a database's tables and views, their columns, and loading one into a sheet
//! as a table, which Refresh reads again (`docs/databases.md`). A database is a SQLite file
//! (`path`) or a PostgreSQL server (`host`, `port`, `database`, `user`, `password`, `ssl`); a
//! PostgreSQL password is a parameter of each call and is never kept, here or in the journal.
//!
//! Loading reads the database table first, then, as one Undo step, puts it in a new sheet named
//! after it (made a valid, unused sheet name) as a table with a header row. Loading the same
//! database table again, or Refresh, reads it again into the same sheet table, which grows or
//! shrinks with it; the cells beside and below must be empty for it to grow into. The workbook
//! keeps where each table came from ([`gridcraft_model::DbSource`]), with each column's declared
//! type and how the rows are told apart, never a password. Saved, the table is an ordinary table
//! that opens without the database. Its columns are fitted to their contents when it is loaded.
//!
//! Values: NULL is an empty cell; integers and reals are numbers, but an integer beyond 2^53
//! (which a number can't hold exactly) is text; a column declared as a boolean takes 0 and 1 as
//! FALSE and TRUE; text in a column declared as a date or time is a date when it reads as one
//! (`2026-01-31`, `2026-01-31 10:20:00`, `2026-01-31T10:20:00Z`), with the date format; a BLOB
//! shows its size.
//!
//! A long load says how far it has got ([`crate::Session::progress`]): the rows read, then the
//! rows written; the interface shows it with a progress bar while the command runs in the
//! background. Cancel stops it in either phase, and nothing changes.

use std::sync::Arc;

use gridcraft_core::{CellRef, RangeRef, Value};
use gridcraft_db::{ColumnInfo, DbError, DbValue, Limits, PgConfig, RowKey, SslMode, Step, TableData};
use gridcraft_model::{DbColumn, DbEngine, DbRowKey, DbSource};
use serde_json::{Value as Json, json};

use crate::cmd::{CommandSpec, bad, cmd, edit, has_doc, str_param};
use crate::progress::Phase;
use crate::{EngineError, Result, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "data.dbTables",
            "DB Connect",
            ["Data", "Get & Transform Data"],
            None,
            "{engine?: \"sqlite\" | \"postgresql\", path (SQLite) | host, port?: 5432, database, user, password?, ssl?: \"disable\" | \"prefer\" | \"require\" (PostgreSQL)} → {tables: [{schema?, name, kind: \"table\" | \"view\"}]}: the tables, then the views, of the database (read-only; PostgreSQL schema by schema). The password is used for the call and never kept",
            has_doc,
            tables
        ),
        cmd!(
            query "data.dbColumns",
            "Table Information",
            ["Data", "Get & Transform Data"],
            None,
            "{engine?: \"sqlite\" | \"postgresql\", path (SQLite) | host, port?: 5432, database, user, password?, ssl?: \"disable\" | \"prefer\" | \"require\" (PostgreSQL), schema? (PostgreSQL, default public), table} → {columns: [{name, type, length, nullable, primaryKey}]}: a table's or view's columns as the database declares them (primaryKey: place in the key, 0 when not in it)",
            has_doc,
            columns
        ),
        cmd!(
            "data.dbLoad",
            "Load",
            ["Data", "Get & Transform Data"],
            None,
            "{engine?: \"sqlite\" | \"postgresql\", path (SQLite) | host, port?: 5432, database, user, password?, ssl?: \"disable\" | \"prefer\" | \"require\" (PostgreSQL), schema?, table} → {sheet, table, range, rows, columns, truncated, cutTexts, refreshed}: the database table in a new sheet named after it, as a table with a header row; a table loaded before is read again into its sheet table instead (`refreshed`)",
            has_doc,
            load
        ),
        cmd!(
            "data.dbRefresh",
            "Refresh",
            ["Data", "Get & Transform Data"],
            None,
            "{table?: a sheet table's name, else the one at the active cell, password? (PostgreSQL: asked again, never kept)} → as data.dbLoad: reads its database table again",
            has_doc,
            refresh
        ),
    ]
}

fn engine_param(p: &Json, id: &str) -> Result<DbEngine> {
    match str_param(p, "engine") {
        None => Ok(DbEngine::Sqlite),
        Some(e) => DbEngine::parse(e).ok_or_else(|| bad(id, format!("unknown database engine `{e}` (known: sqlite, postgresql)"))),
    }
}

/// The database file `p.path`, absolute, where files may be read.
fn path_param(p: &Json, id: &str) -> Result<String> {
    let path = str_param(p, "path").map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| bad(id, "missing `path`: the database file"))?;
    let abs = crate::io::source_path(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
    crate::io::check_file_access(&abs)?;
    Ok(abs)
}

/// A PostgreSQL connection from the parameters: `host`, `port`, `database`, `user`, `password`,
/// `ssl`. A macro step or a script may not reach a server any more than a file.
fn pg_param(p: &Json, id: &str) -> Result<PgConfig> {
    let need =
        |k: &str| str_param(p, k).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| bad(id, format!("missing `{k}`")));
    let port = match p.get("port") {
        None | Some(Json::Null) => 5432,
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| bad(id, "`port` must be 1 to 65535"))?,
    };
    let ssl = match str_param(p, "ssl") {
        None => SslMode::default(),
        Some(m) => SslMode::parse(m).ok_or_else(|| bad(id, format!("`ssl` must be disable, prefer or require, not `{m}`")))?,
    };
    let cfg = PgConfig {
        host: need("host")?,
        port,
        database: need("database")?,
        user: need("user")?,
        password: str_param(p, "password").unwrap_or("").to_string(),
        ssl,
    };
    crate::io::check_file_access(&cfg.url())?;
    Ok(cfg)
}

/// A database to read: a SQLite file or a PostgreSQL connection.
enum Conn {
    Sqlite(String),
    Pg(PgConfig),
}

impl Conn {
    fn from_params(p: &Json, id: &str) -> Result<Conn> {
        Ok(match engine_param(p, id)? {
            DbEngine::Sqlite => Conn::Sqlite(path_param(p, id)?),
            DbEngine::Postgres => Conn::Pg(pg_param(p, id)?),
        })
    }

    /// The database a loaded table came from; a PostgreSQL one takes `p.password`.
    fn of_source(src: &DbSource, p: &Json, id: &str) -> Result<Conn> {
        Ok(match src.engine {
            DbEngine::Sqlite => Conn::Sqlite(src.location.clone()),
            DbEngine::Postgres => {
                let mut cfg = PgConfig::from_url(&src.location).ok_or_else(|| bad(id, "the table's database connection can't be read"))?;
                cfg.password = str_param(p, "password").unwrap_or("").to_string();
                Conn::Pg(cfg)
            }
        })
    }

    fn engine(&self) -> DbEngine {
        match self {
            Conn::Sqlite(_) => DbEngine::Sqlite,
            Conn::Pg(_) => DbEngine::Postgres,
        }
    }

    /// Where the database is, as a source keeps it (never a password).
    fn location(&self) -> String {
        match self {
            Conn::Sqlite(path) => path.clone(),
            Conn::Pg(cfg) => cfg.url(),
        }
    }

    /// The schema a table is in: PostgreSQL's (`public` unless named), none for SQLite.
    fn schema(&self, p: &Json) -> Option<String> {
        match self {
            Conn::Sqlite(_) => None,
            Conn::Pg(_) => Some(str_param(p, "schema").map(str::trim).filter(|s| !s.is_empty()).unwrap_or("public").to_string()),
        }
    }

    fn objects(&self) -> std::result::Result<Vec<gridcraft_db::ObjectInfo>, DbError> {
        match self {
            Conn::Sqlite(path) => gridcraft_db::sqlite_objects(path),
            Conn::Pg(cfg) => gridcraft_db::pg_objects(cfg),
        }
    }

    fn columns(&self, schema: Option<&str>, table: &str) -> std::result::Result<Vec<ColumnInfo>, DbError> {
        match self {
            Conn::Sqlite(path) => gridcraft_db::sqlite_columns(path, table),
            Conn::Pg(cfg) => gridcraft_db::pg_columns(cfg, schema.unwrap_or("public"), table),
        }
    }
}

fn db_err(e: DbError) -> EngineError {
    EngineError::Other(e.to_string())
}

fn tables(_: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.dbTables";
    let conn = Conn::from_params(p, ID)?;
    let found = conn.objects().map_err(db_err)?;
    let list = found.iter().map(|o| {
        let mut t = json!({"name": o.name, "kind": o.kind.name()});
        if let Some(schema) = &o.schema {
            t["schema"] = json!(schema);
        }
        t
    });
    Ok(json!({"tables": list.collect::<Vec<_>>()}))
}

fn column_json(c: &ColumnInfo) -> Json {
    json!({"name": c.name, "type": c.data_type, "length": c.length, "nullable": c.nullable, "primaryKey": c.primary_key})
}

fn columns(_: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.dbColumns";
    let conn = Conn::from_params(p, ID)?;
    let table = str_param(p, "table").ok_or_else(|| bad(ID, "missing `table`"))?;
    let cols = conn.columns(conn.schema(p).as_deref(), table).map_err(db_err)?;
    Ok(json!({"columns": cols.iter().map(column_json).collect::<Vec<_>>()}))
}

/// A sheet name for database table `table`: the characters a sheet name can't hold become `_`,
/// at most 31 characters, and unused (`orders (2)`).
fn sheet_name(wb: &gridcraft_model::Workbook, table: &str) -> String {
    let clean: String =
        table.chars().map(|c| if matches!(c, ':' | '\\' | '/' | '?' | '*' | '[' | ']') || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim().trim_matches('\'').to_string();
    let base = if clean.is_empty() || clean.eq_ignore_ascii_case("History") { "Table".to_string() } else { clean };
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let first = cut(&base, 31);
    if wb.check_sheet_name(&first, None).is_ok() {
        return first;
    }
    (2..10_000)
        .map(|n| {
            let suffix = format!(" ({n})");
            format!("{}{suffix}", cut(&base, 31 - suffix.chars().count()))
        })
        .find(|name| wb.check_sheet_name(name, None).is_ok())
        .unwrap_or_else(|| wb.next_sheet_name())
}

/// The cell values of a table read: the header row, then each row, with the number format
/// each column's dates take.
struct Grid {
    rows: Vec<Vec<Value>>,
    formats: Vec<Option<&'static str>>,
}

/// Largest integer a number holds exactly.
const MAX_EXACT: i64 = 1 << 53;

fn grid(data: &TableData, sys: gridcraft_core::DateSystem) -> Grid {
    let upper: Vec<String> = data.columns.iter().map(|c| c.data_type.to_ascii_uppercase()).collect();
    let mut formats: Vec<Option<&'static str>> = vec![None; data.columns.len()];
    let mut rows = Vec::with_capacity(data.rows.len() + 1);
    rows.push(data.columns.iter().map(|c| Value::text(c.name.as_str())).collect());
    for row in &data.rows {
        let mut out = Vec::with_capacity(row.len());
        for (i, v) in row.iter().enumerate() {
            let ty = upper.get(i).map(String::as_str).unwrap_or("");
            out.push(match v {
                DbValue::Null => Value::Empty,
                DbValue::Integer(n) if ty.contains("BOOL") => Value::Bool(*n != 0),
                DbValue::Integer(n) if n.unsigned_abs() <= MAX_EXACT.unsigned_abs() => Value::number(*n as f64),
                DbValue::Integer(n) => Value::text(n.to_string()),
                DbValue::Real(x) if x.is_finite() => Value::number(*x),
                DbValue::Real(x) => Value::text(x.to_string()),
                DbValue::Text(t) if ty.contains("DATE") || ty.contains("TIME") => {
                    let typed = t.trim().trim_end_matches('Z').replacen('T', " ", 1);
                    let parsed = gridcraft_core::parse::parse_input(&typed, sys);
                    match (parsed.value, parsed.format) {
                        (Value::Number(n), Some(f)) => {
                            if let Some(slot) = formats.get_mut(i) {
                                slot.get_or_insert(f);
                            }
                            Value::number(n)
                        }
                        _ => Value::text(t.as_str()),
                    }
                }
                DbValue::Text(t) => Value::text(t.as_str()),
                DbValue::Blob(n) => Value::text(format!("[BLOB {n} bytes]")),
            });
        }
        rows.push(out);
    }
    Grid { rows, formats }
}

fn source(conn: &Conn, schema: Option<String>, table: &str, table_id: u32, data: &TableData) -> DbSource {
    let columns = data
        .columns
        .iter()
        .map(|c| DbColumn {
            name: c.name.clone(),
            data_type: c.data_type.clone(),
            length: c.length.clone(),
            nullable: c.nullable,
            primary_key: c.primary_key,
        })
        .collect();
    let key = match &data.key {
        RowKey::Columns(cols) => DbRowKey::Columns(cols.clone()),
        RowKey::Rowid => DbRowKey::Rowid(data.rowids.clone()),
        RowKey::None => DbRowKey::None,
    };
    DbSource { engine: conn.engine(), location: conn.location(), schema, table: table.to_string(), table_id, columns, key }
}

/// The sheet table with id `id`: its sheet, name and range.
fn table_by_id(wb: &gridcraft_model::Workbook, id: u32) -> Option<(usize, String, RangeRef)> {
    wb.sheets.iter().enumerate().find_map(|(si, sh)| sh.tables.iter().find(|t| t.id == id).map(|t| (si, t.name.clone(), t.range)))
}

/// The range a table of `cols` columns and `rows` data rows takes from `start`: a header row and
/// at least one data row, as every sheet table has.
fn range_for(start: CellRef, rows: usize, cols: usize, id: &str) -> Result<RangeRef> {
    let end_row = u64::from(start.row) + rows.max(1) as u64;
    let end_col = u64::from(start.col) + cols.max(1) as u64 - 1;
    if end_row >= u64::from(gridcraft_core::MAX_ROWS) || end_col >= u64::from(gridcraft_core::MAX_COLS) {
        return Err(bad(id, "the table doesn't fit on the sheet from where it starts"));
    }
    Ok(RangeRef::new(start, CellRef::new(end_row as u32, end_col as u32)))
}

/// Writes `g` from `start`, emptying first the cells of `clear` (the table's old range), and gives
/// each date column its format.
fn write(s: &mut Session, si: usize, start: CellRef, g: &Grid, clear: Option<RangeRef>) -> Result<()> {
    let interrupt = s.interrupt();
    let progress = s.progress();
    let total = g.rows.len() as u64;
    let written = edit(s, |cx| {
        let sh = cx.sheet_mut(si)?;
        if let Some(old) = clear {
            let filled: Vec<CellRef> = sh.cells.iter_range(old).map(|(c, _)| c).collect();
            for c in filled {
                sh.set_value(c, Value::Empty);
            }
        }
        for (r, row) in g.rows.iter().enumerate() {
            if r.is_multiple_of(WRITE_REPORT_EVERY) {
                if interrupt.is_requested() {
                    return Err(EngineError::Cancelled);
                }
                progress.report(Phase::WritingRows, r as u64, total);
            }
            for (c, v) in row.iter().enumerate() {
                if !v.is_empty() {
                    sh.set_value(CellRef::new(start.row + r as u32, start.col + c as u32), v.clone());
                }
            }
        }
        cx.structural = true;
        Ok(())
    });
    progress.clear();
    written?;
    let data_rows = g.rows.len().saturating_sub(1);
    for (c, f) in g.formats.iter().enumerate() {
        if let (Some(f), true) = (f, data_rows > 0) {
            let col = start.col + c as u32;
            let r = RangeRef::new(CellRef::new(start.row + 1, col), CellRef::new(start.row + data_rows as u32, col));
            s.execute("home.numberFormat", json!({"sheet": si, "range": r.a1(), "format": f}))?;
        }
    }
    Ok(())
}

fn read(s: &Session, conn: &Conn, schema: Option<&str>, table: &str) -> Result<TableData> {
    // Cancel (the busy box, Esc) stops the read; the rows read show as they come.
    let interrupt = s.interrupt();
    let stop: gridcraft_db::Stop = Arc::new(move || interrupt.is_requested());
    let progress = s.progress();
    let mut report = |step: Step| match step {
        Step::Connecting => progress.report(Phase::Connecting, 0, 0),
        Step::Counting => progress.report(Phase::CountingRows, 0, 0),
        Step::Rows { done, total } => progress.report(Phase::ReadingRows, done as u64, total as u64),
    };
    let data = match conn {
        Conn::Sqlite(path) => gridcraft_db::sqlite_read(path, table, Limits::default(), Some(stop), &mut report),
        Conn::Pg(cfg) => gridcraft_db::pg_read(cfg, schema.unwrap_or("public"), table, Limits::default(), Some(stop), &mut report),
    };
    s.progress().clear();
    data.map_err(|e| match e {
        DbError::Cancelled => EngineError::Cancelled,
        e => db_err(e),
    })
}

/// How often (in rows) writing reports its progress and asks whether to stop.
const WRITE_REPORT_EVERY: usize = 5000;

fn result(data: &TableData, sheet: &str, table: &str, range: RangeRef, refreshed: bool) -> Json {
    json!({
        "sheet": sheet,
        "table": table,
        "range": range.a1(),
        "rows": data.rows.len(),
        "columns": data.columns.len(),
        "truncated": data.truncated,
        "cutTexts": data.cut_texts,
        "refreshed": refreshed,
    })
}

fn load(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.dbLoad";
    let conn = Conn::from_params(p, ID)?;
    let schema = conn.schema(p);
    let table = str_param(p, "table").ok_or_else(|| bad(ID, "missing `table`"))?.to_string();
    // Loaded before, and its sheet table is still there: read it again into that table.
    let (engine, location) = (conn.engine(), conn.location());
    let wb = &s.doc()?.wb;
    let before = wb.db_sources.iter().find(|src| {
        src.engine == engine && src.location == location && src.schema == schema && src.table == table && table_by_id(wb, src.table_id).is_some()
    });
    if let Some(id) = before.map(|src| src.table_id) {
        return refresh_table(s, id, &conn, ID);
    }
    let data = read(s, &conn, schema.as_deref(), &table)?;
    if data.columns.is_empty() {
        return Err(bad(ID, format!("\"{table}\" has no columns to load")));
    }
    let sys = s.doc()?.wb.date_system;
    let g = grid(&data, sys);
    s.grouped("Load from Database", |s| {
        // A table of another schema than `public` is named with it (`gudang.stok`).
        let shown = match schema.as_deref() {
            Some(sch) if sch != "public" => format!("{sch}.{table}"),
            _ => table.clone(),
        };
        let name = sheet_name(&s.doc()?.wb, &shown);
        let count = s.doc()?.wb.sheets.len();
        s.execute("home.insertSheet", json!({"name": name, "before": count}))?;
        let si = s.doc()?.wb.sheet_index(&name).ok_or_else(|| bad(ID, "the new sheet is gone"))?;
        let range = range_for(CellRef::new(0, 0), data.rows.len(), data.columns.len(), ID)?;
        write(s, si, range.start, &g, None)?;
        let made = s.execute("insert.table", json!({"sheet": si, "range": range.a1(), "header": true}))?;
        // Columns as wide as their contents, so dates and long texts show (a refresh keeps the
        // widths the person set).
        let cols = format!("{}:{}", gridcraft_core::col_to_letters(range.start.col), gridcraft_core::col_to_letters(range.end.col));
        s.execute("home.autofitColumnWidth", json!({"sheet": si, "cols": cols}))?;
        let tname = made.get("table").and_then(Json::as_str).unwrap_or_default().to_string();
        let wb = &s.doc()?.wb;
        let id = wb.sheet(si).and_then(|sh| sh.tables.iter().find(|t| t.name == tname)).map(|t| t.id).ok_or_else(|| bad(ID, "the table is gone"))?;
        let src = Arc::new(source(&conn, schema.clone(), &table, id, &data));
        edit(s, |cx| {
            cx.wb.db_sources.retain(|o| o.table_id != id);
            cx.wb.db_sources.push(src);
            Ok(())
        })?;
        s.execute("selection.set", json!({"sheet": si, "range": "A1"}))?;
        Ok(result(&data, &name, &tname, range, false))
    })
}

fn refresh(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.dbRefresh";
    let d = s.doc()?;
    let wb = &d.wb;
    let si = wb.active_sheet();
    let sh = wb.sheet(si).ok_or(EngineError::NoDocument)?;
    let t = match str_param(p, "table") {
        Some(n) => wb.table(n).and_then(|(si, ti)| wb.sheet(si).and_then(|sh| sh.tables.get(ti))),
        None => sh.table_at(d.selection.active).or_else(|| {
            // The only database table on the sheet.
            let mine: Vec<_> = sh.tables.iter().filter(|t| wb.db_sources.iter().any(|src| src.table_id == t.id)).collect();
            if mine.len() == 1 { mine.first().copied() } else { None }
        }),
    };
    let id = t.map(|t| t.id).ok_or_else(|| bad(ID, "select a cell in a table loaded from a database"))?;
    let src = wb.db_sources.iter().find(|src| src.table_id == id).cloned().ok_or_else(|| bad(ID, "this table wasn't loaded from a database"))?;
    let conn = Conn::of_source(&src, p, ID)?;
    refresh_table(s, id, &conn, ID)
}

/// Reads the database table of sheet table `id` again into it, from `conn`.
fn refresh_table(s: &mut Session, id: u32, conn: &Conn, cmd: &str) -> Result<Json> {
    let wb = &s.doc()?.wb;
    let src = wb.db_sources.iter().find(|src| src.table_id == id).cloned().ok_or_else(|| bad(cmd, "this table wasn't loaded from a database"))?;
    let (si, tname, old) = table_by_id(wb, id).ok_or_else(|| bad(cmd, "the table loaded from the database is gone"))?;
    let data = read(s, conn, src.schema.as_deref(), &src.table)?;
    if data.columns.is_empty() {
        return Err(bad(cmd, format!("\"{}\" has no columns to load", src.table)));
    }
    let new = range_for(old.start, data.rows.len(), data.columns.len(), cmd)?;
    // Growing: the cells it grows into must be empty and in no other table.
    let sh = wb.sheet(si).ok_or(EngineError::NoDocument)?;
    let taken = sh.cells.iter_range(new).any(|(c, cell)| !old.contains(c) && cell.has_content());
    if taken || sh.tables.iter().any(|t| t.id != id && t.range.intersects(&new)) {
        return Err(EngineError::Other(format!(
            "There isn't room for \"{}\" to grow to {}: move what is beside or below the table first.",
            src.table,
            new.a1()
        )));
    }
    let sheet = sh.name.clone();
    let g = grid(&data, wb.date_system);
    s.grouped("Refresh", |s| {
        if s.doc()?.wb.active_sheet() != si {
            s.execute("sheet.activate", json!({"sheet": si}))?;
        }
        write(s, si, new.start, &g, Some(old))?;
        if new != old {
            s.execute("table.resize", json!({"table": tname, "range": new.a1()}))?;
        }
        let fresh = Arc::new(source(conn, src.schema.clone(), &src.table, id, &data));
        edit(s, |cx| {
            for o in cx.wb.db_sources.iter_mut().filter(|o| o.table_id == id) {
                *o = fresh.clone();
            }
            Ok(())
        })?;
        Ok(result(&data, &sheet, &tname, new, true))
    })
}
