//! Saving the changes made in a table loaded from a database back to it (Data › DB Connect ›
//! Save), and what keeps those changes fit to save (`docs/databases.md`).
//!
//! Only values change: the header row (the database's column names) and the primary key's
//! columns (which tell the rows apart) stay as they were loaded ([`check_locks`]), and each other
//! column gets a data validation rule from its declared type ([`rules`]), so a value that can't go
//! in is refused as it is typed. Saving checks every changed value again, against its column's
//! type ([`to_db`]), since a paste or a macro passes no validation, and writes nothing if one
//! doesn't fit. The database then gets every changed row in one transaction, each found by its
//! key and compared with the row as it was loaded: a row changed there since, or gone, stops the
//! whole save. The rows saved are read back into the sheet as the database keeps them.
//!
//! Rows added to the table and rows taken out of it are left alone (adding and deleting rows is
//! a later stage); the result counts them. Saving can't be undone, and clears Undo.

use std::collections::HashMap;
use std::sync::Arc;

use gridcraft_core::{CellRef, DateSystem, RangeRef, Value};
use gridcraft_db::{DbError, DbValue, RowChange, WriteTarget};
use gridcraft_model::{DbCell, DbColumn, DbEngine, DbOriginal, DbRowKey, DbSource, Workbook};
use serde_json::{Value as Json, json};

use super::db::{Conn, db_err, find_table, read, shown, table_by_id, target, unlocked, write_to_db};
use crate::cmd::{bad, bool_param, edit};
use crate::{EngineError, Result, Session};

/// What a column's declared type takes, for checking a value and for its validation rule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    /// A whole number between the two.
    Int(i64, i64),
    /// A floating-point number.
    Real,
    /// A decimal number of at most `digits` digits before the point (from `numeric(p,s)`).
    Numeric {
        digits: Option<u32>,
    },
    Bool,
    Date,
    DateTime,
    Time,
    /// A text of at most so many characters.
    Text(Option<usize>),
    /// Binary data: it can't be changed from a sheet.
    Blob,
    /// Anything else (`json`, `uuid`, an enum, an array, a column without a type): its text, which
    /// the database checks.
    Other,
}

/// The kind of column `c` of a database of `engine`.
pub(super) fn kind(c: &DbColumn, engine: DbEngine) -> Kind {
    let t = c.data_type.to_ascii_lowercase();
    let len = || c.length.as_deref().and_then(|l| l.split(',').next()).and_then(|n| n.trim().parse::<usize>().ok());
    let digits = || {
        let mut it = c.length.as_deref()?.split(',').map(|n| n.trim().parse::<u32>().ok());
        let p = it.next().flatten()?;
        let s = it.next().flatten().unwrap_or(0);
        Some(p.saturating_sub(s))
    };
    match engine {
        DbEngine::Postgres => match t.as_str() {
            "smallint" => Kind::Int(i64::from(i16::MIN), i64::from(i16::MAX)),
            "integer" => Kind::Int(i64::from(i32::MIN), i64::from(i32::MAX)),
            "bigint" => Kind::Int(i64::MIN, i64::MAX),
            "real" | "double precision" => Kind::Real,
            "numeric" => Kind::Numeric { digits: digits() },
            "boolean" => Kind::Bool,
            "date" => Kind::Date,
            "timestamp without time zone" | "timestamp with time zone" => Kind::DateTime,
            "time without time zone" => Kind::Time,
            "character varying" | "character" => Kind::Text(len()),
            "text" => Kind::Text(None),
            "bytea" => Kind::Blob,
            _ => Kind::Other,
        },
        // SQLite's affinities (https://sqlite.org/datatype3.html#determination_of_column_affinity),
        // with the names that say a boolean, a date or a time first.
        DbEngine::Sqlite => {
            if t.contains("bool") {
                Kind::Bool
            } else if t.contains("datetime") || t.contains("timestamp") {
                Kind::DateTime
            } else if t.contains("date") {
                Kind::Date
            } else if t.contains("time") {
                Kind::Time
            } else if t.contains("int") {
                Kind::Int(i64::MIN, i64::MAX)
            } else if t.contains("char") || t.contains("clob") || t.contains("text") {
                Kind::Text(len())
            } else if t.contains("blob") {
                Kind::Blob
            } else if t.contains("real") || t.contains("floa") || t.contains("doub") {
                Kind::Real
            } else if t.contains("numeric") || t.contains("decimal") {
                Kind::Numeric { digits: digits() }
            } else {
                Kind::Other
            }
        }
    }
}

/// How a column's type is named in a message (`VARCHAR(40)`, `numeric(12,2)`).
fn type_name(c: &DbColumn) -> String {
    match &c.length {
        Some(l) => format!("{}({l})", c.data_type),
        None => c.data_type.clone(),
    }
}

/// Largest integer a number holds exactly.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

/// The value a cell's value `v` is written to column `c` as, or why it can't be.
pub(super) fn to_db(v: &Value, c: &DbColumn, k: Kind, sys: DateSystem) -> std::result::Result<DbValue, String> {
    let ty = type_name(c);
    if matches!(v, Value::Empty) || matches!(v, Value::Text(t) if t.is_empty() && !matches!(k, Kind::Text(_) | Kind::Other)) {
        return if c.nullable { Ok(DbValue::Null) } else { Err(format!("{} can't be empty", c.name)) };
    }
    if let Value::Error(e) = v {
        return Err(format!("{} holds the error {}", c.name, e.as_str()));
    }
    let number = |v: &Value| match v {
        Value::Number(n) => Some(*n),
        Value::Text(t) => gridcraft_core::parse::parse_number_text_in(t.trim(), sys),
        _ => None,
    };
    match k {
        Kind::Int(lo, hi) => {
            let n = match v {
                Value::Number(n) if n.fract() == 0.0 && n.abs() <= MAX_EXACT => Some(*n as i64),
                Value::Text(t) => t.trim().parse::<i64>().ok(),
                _ => None,
            };
            match n {
                Some(n) if (lo..=hi).contains(&n) => Ok(DbValue::Integer(n)),
                Some(_) => Err(format!("{} is {ty}: {} is out of its range", c.name, v.display())),
                None => Err(format!("{} is {ty}: it takes a whole number", c.name)),
            }
        }
        Kind::Real => number(v).filter(|n| n.is_finite()).map(DbValue::Real).ok_or_else(|| format!("{} is {ty}: it takes a number", c.name)),
        Kind::Numeric { digits } => {
            // A long decimal loaded as text stays text, exact.
            if let Value::Text(t) = v {
                let t = t.trim();
                let plain = t.strip_prefix(['-', '+']).unwrap_or(t);
                if !plain.is_empty() && plain.chars().all(|ch| ch.is_ascii_digit() || ch == '.') && plain.matches('.').count() <= 1 {
                    let whole = plain.split('.').next().unwrap_or("").trim_start_matches('0').len();
                    return match digits {
                        Some(d) if whole > d as usize => Err(format!("{} is {ty}: {t} has too many digits", c.name)),
                        _ => Ok(DbValue::Text(t.to_string())),
                    };
                }
            }
            let n = number(v).filter(|n| n.is_finite()).ok_or_else(|| format!("{} is {ty}: it takes a number", c.name))?;
            if let Some(d) = digits
                && n.abs().trunc() >= 10f64.powi(d as i32)
            {
                return Err(format!("{} is {ty}: {} has too many digits", c.name, v.display()));
            }
            Ok(if n.fract() == 0.0 && n.abs() <= MAX_EXACT { DbValue::Integer(n as i64) } else { DbValue::Real(n) })
        }
        Kind::Bool => match v {
            Value::Bool(b) => Ok(DbValue::Integer(i64::from(*b))),
            Value::Number(n) if *n == 0.0 || *n == 1.0 => Ok(DbValue::Integer(*n as i64)),
            Value::Text(t) if t.eq_ignore_ascii_case("true") => Ok(DbValue::Integer(1)),
            Value::Text(t) if t.eq_ignore_ascii_case("false") => Ok(DbValue::Integer(0)),
            _ => Err(format!("{} is {ty}: it takes TRUE or FALSE", c.name)),
        },
        Kind::Date | Kind::DateTime | Kind::Time => {
            let serial = match v {
                Value::Number(n) => Some(*n),
                Value::Text(t) => match gridcraft_core::parse::parse_input(t.trim(), sys) {
                    gridcraft_core::parse::Parsed { value: Value::Number(n), format: Some(_), .. } => Some(n),
                    _ => None,
                },
                _ => None,
            };
            let what = match k {
                Kind::Date => "a date",
                Kind::Time => "a time",
                _ => "a date and time",
            };
            let dt = serial
                .and_then(|n| gridcraft_core::date::datetime_from_serial(sys, if k == Kind::Time { n.rem_euclid(1.0) } else { n }))
                .ok_or_else(|| format!("{} is {ty}: it takes {what}", c.name))?;
            let date = format!("{:04}-{:02}-{:02}", dt.year, dt.month, dt.day);
            let mut time = format!("{:02}:{:02}:{:02}", dt.hour, dt.minute, dt.second);
            if dt.milli > 0 {
                time.push_str(&format!(".{:03}", dt.milli));
            }
            Ok(DbValue::Text(match k {
                Kind::Date => date,
                Kind::Time => time,
                _ => format!("{date} {time}"),
            }))
        }
        Kind::Text(max) => {
            let t = v.to_text().map_err(|e| format!("{} holds the error {}", c.name, e.as_str()))?;
            match max {
                Some(m) if t.chars().count() > m => Err(format!("{} is {ty}: at most {m} characters", c.name)),
                _ => Ok(DbValue::Text(t)),
            }
        }
        Kind::Blob => Err(format!("{} is binary data ({ty}): it can't be changed in a sheet", c.name)),
        Kind::Other => match v {
            Value::Number(n) if c.data_type.is_empty() && n.fract() == 0.0 && n.abs() <= MAX_EXACT => Ok(DbValue::Integer(*n as i64)),
            Value::Number(n) if c.data_type.is_empty() => Ok(DbValue::Real(*n)),
            Value::Bool(b) => Ok(DbValue::Text(if *b { "true" } else { "false" }.into())),
            v => v.to_text().map(DbValue::Text).map_err(|e| format!("{} holds the error {}", c.name, e.as_str())),
        },
    }
}

/// The data validation (`data.validation` parameters, without the range) a column of kind `k`
/// gets: the values its type takes, refused as they are typed, with a message that names the
/// type. None for a column whose values the database alone checks.
pub(super) fn rule(c: &DbColumn, k: Kind) -> Option<Json> {
    let ty = type_name(c);
    let (mut p, says) = match k {
        Kind::Int(lo, hi) => {
            let (lo, hi) = (lo.max(-(MAX_EXACT as i64)), hi.min(MAX_EXACT as i64));
            (json!({"type": "whole", "operator": "between", "formula1": lo.to_string(), "formula2": hi.to_string()}), "a whole number".to_string())
        }
        Kind::Real => (json!({"type": "decimal", "operator": "between", "formula1": "-1E+307", "formula2": "1E+307"}), "a number".into()),
        Kind::Numeric { digits } => {
            let top = match digits {
                Some(d) => format!("1E+{d}"),
                None => "1E+307".into(),
            };
            (json!({"type": "decimal", "operator": "between", "formula1": format!("-{top}"), "formula2": top}), "a number".into())
        }
        Kind::Bool => (json!({"type": "list", "formula1": "TRUE,FALSE"}), "TRUE or FALSE".into()),
        Kind::Date | Kind::DateTime => (json!({"type": "date", "operator": "greaterOrEqual", "formula1": "1"}), "a date".into()),
        Kind::Time => (json!({"type": "time", "operator": "between", "formula1": "0", "formula2": "0.99999999"}), "a time".into()),
        Kind::Text(Some(m)) => {
            (json!({"type": "textLength", "operator": "lessOrEqual", "formula1": m.to_string()}), format!("at most {m} characters"))
        }
        Kind::Text(None) | Kind::Blob | Kind::Other => return None,
    };
    let empty = if c.nullable { "" } else { ", and can't be empty" };
    p["ignoreBlank"] = json!(c.nullable);
    p["errorStyle"] = json!("stop");
    p["errorTitle"] = json!(format!("{} ({ty})", c.name));
    p["errorMessage"] = json!(format!("This column of the database takes {says}{empty}."));
    Some(p)
}

/// A cell's value as a key for comparing (numbers by their bits).
fn repr(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Number(n) => format!("n{}", n.to_bits()),
        Value::Text(t) => format!("t{t}"),
        Value::Bool(b) => format!("b{b}"),
        Value::Error(e) => format!("e{}", e.as_str()),
        Value::Array(_) => "a".into(),
    }
}

/// The header names of a table, by column.
fn heads(wb: &Workbook, si: usize, range: RangeRef) -> Vec<String> {
    let Some(sh) = wb.sheet(si) else { return Vec::new() };
    (range.start.col..=range.end.col).map(|c| sh.cell(CellRef::new(range.start.row, c)).map(|x| x.value.display()).unwrap_or_default()).collect()
}

/// Where each of the source's key columns is in the table (offsets from its first column).
fn key_offsets(src: &DbSource, heads: &[String]) -> Option<Vec<u32>> {
    let DbRowKey::Columns(key) = &src.key else { return Some(Vec::new()) };
    key.iter().map(|&k| src.columns.get(k).and_then(|c| heads.iter().position(|h| *h == c.name)).and_then(|p| u32::try_from(p).ok())).collect()
}

/// Each data row's key in the table: `None` for a row whose key cells are all empty (a row added).
fn keys(wb: &Workbook, si: usize, data: RangeRef, offsets: &[u32]) -> Vec<Option<(String, bool)>> {
    let Some(sh) = wb.sheet(si) else { return Vec::new() };
    (data.start.row..=data.end.row)
        .map(|r| {
            let vals: Vec<Value> =
                offsets.iter().map(|o| sh.cell(CellRef::new(r, data.start.col + o)).map(|x| x.value.clone()).unwrap_or(Value::Empty)).collect();
            let empty = vals.iter().filter(|v| matches!(v, Value::Empty)).count();
            if empty == vals.len() {
                return None;
            }
            Some((vals.iter().map(repr).collect::<Vec<_>>().join("\u{1f}"), empty > 0))
        })
        .collect()
}

/// The message for an edit of a key column.
fn key_locked(src: &DbSource) -> String {
    let names: Vec<String> = match &src.key {
        DbRowKey::Columns(cols) => cols.iter().filter_map(|&c| src.columns.get(c)).map(|c| format!("\"{}\"", c.name)).collect(),
        _ => Vec::new(),
    };
    format!("The key column {} of a table loaded from a database can't be changed: it tells the database which row is which.", names.join(", "))
}

const HEADER_LOCKED: &str = "The header row of a table loaded from a database can't be changed: its names are the database's columns.";

/// Refuses an edit that changed the header row of a table loaded from a database, or the values
/// of its key columns (a key may only move with its row, as a sort moves it, or go with it, and a
/// row added must leave its key empty). `structural` edits (rows or columns inserted, deleted or
/// moved) are checked in full; others only when they changed a header or key cell.
pub(crate) fn check_locks(old: &Workbook, new: &Workbook, structural: bool) -> Result<()> {
    for src in &old.db_sources {
        let (Some((si, _, before)), Some((sj, _, after))) = (table_by_id(old, src.table_id), table_by_id(new, src.table_id)) else { continue };
        let (Some(a), Some(b)) = (old.sheets.get(si), new.sheets.get(sj)) else { continue };
        if Arc::ptr_eq(a, b) && before == after {
            continue;
        }
        let (Some(t_old), Some(t_new)) = (find_table(old, src.table_id), find_table(new, src.table_id)) else { continue };
        if !t_old.header_row {
            continue;
        }
        if !t_new.header_row {
            return Err(EngineError::Other(HEADER_LOCKED.into()));
        }
        let old_heads = heads(old, si, before);
        let offsets = key_offsets(src, &old_heads).unwrap_or_default();
        if !structural && before == after {
            let touched = a
                .cells
                .diff(&b.cells)
                .into_iter()
                .any(|c| after.contains(c) && (c.row == after.start.row || offsets.iter().any(|o| c.col == after.start.col + o)));
            if !touched {
                continue;
            }
        }
        let new_heads = heads(new, sj, after);
        // Columns added at the right end of the table aren't the database's and may be named.
        if new_heads.len() < old_heads.len() || new_heads.get(..old_heads.len()) != Some(old_heads.as_slice()) {
            return Err(EngineError::Other(HEADER_LOCKED.into()));
        }
        if offsets.is_empty() {
            continue;
        }
        let (Some(d_old), Some(d_new)) = (t_old.data_range(), t_new.data_range()) else { continue };
        let mut had: HashMap<String, usize> = HashMap::new();
        for k in keys(old, si, d_old, &offsets).into_iter().flatten() {
            *had.entry(k.0).or_default() += 1;
        }
        // Rows inserted or deleted may add rows with an empty key or take rows away; any other
        // edit may only move the keys (a sort), every one staying.
        let mut kept = 0usize;
        let total: usize = had.values().sum();
        for k in keys(new, sj, d_new, &offsets).into_iter().flatten() {
            let left = had.get_mut(&k.0).filter(|n| **n > 0 && !k.1);
            match left {
                Some(n) => {
                    *n -= 1;
                    kept += 1;
                }
                None => return Err(EngineError::Other(key_locked(src))),
            }
        }
        if !structural && kept != total {
            return Err(EngineError::Other(key_locked(src)));
        }
    }
    Ok(())
}

/// The value as the database module reads it.
pub(super) fn db_value(c: &DbCell) -> DbValue {
    match c {
        DbCell::Null => DbValue::Null,
        DbCell::Integer(n) => DbValue::Integer(*n),
        DbCell::Real(x) => DbValue::Real(*x),
        DbCell::Text(t) => DbValue::Text(t.clone()),
        DbCell::Blob(n) => DbValue::Blob(*n),
    }
}

pub(super) fn db_cell(v: DbValue) -> DbCell {
    match v {
        DbValue::Null => DbCell::Null,
        DbValue::Integer(n) => DbCell::Integer(n),
        DbValue::Real(x) => DbCell::Real(x),
        DbValue::Text(t) => DbCell::Text(t),
        DbValue::Blob(n) => DbCell::Blob(n),
    }
}

/// Says a list of up to `max` things, then how many more.
fn some_of(items: &[String], max: usize) -> String {
    let mut s = items.iter().take(max).cloned().collect::<Vec<_>>().join("; ");
    if items.len() > max {
        s.push_str(&format!("; and {} more", items.len() - max));
    }
    s
}

const NEEDS_OVERWRITE: &str = "This table was loaded before the workbook was opened, so GridKu can't tell which values changed in the database since. \
Refresh it to save safely, or save with overwrite to write every value that differs from the database now.";

/// What saving does, found from the sheet: the rows to write and where each is.
struct Plan {
    changes: Vec<RowChange>,
    /// Each change's sheet row and its row among the originals.
    at: Vec<(u32, usize)>,
    problems: Vec<String>,
    added: usize,
    removed: usize,
}

/// Compares the table's data rows with the rows as loaded.
#[allow(clippy::too_many_arguments, reason = "the table, its source and how to read both")]
fn plan(
    wb: &Workbook,
    si: usize,
    data: RangeRef,
    src: &DbSource,
    pos: &[Option<u32>],
    key: &[usize],
    originals: &[Vec<DbCell>],
    kinds: &[Kind],
) -> Result<Plan> {
    let sys = wb.date_system;
    let sh = wb.sheet(si).ok_or(EngineError::NoDocument)?;
    let uppers: Vec<String> = src.columns.iter().map(|c| c.data_type.to_ascii_uppercase()).collect();
    let shown_at =
        |row: &[DbCell], c: usize| row.get(c).map(|v| shown(v, uppers.get(c).map(String::as_str).unwrap_or(""), sys).0).unwrap_or(Value::Empty);
    let mut index: HashMap<String, usize> = HashMap::with_capacity(originals.len());
    for (i, row) in originals.iter().enumerate() {
        index.insert(key.iter().map(|&k| repr(&shown_at(row, k))).collect::<Vec<_>>().join("\u{1f}"), i);
    }
    let value_at = |r: u32, off: u32| sh.cell(CellRef::new(r, data.start.col + off)).map(|x| x.value.clone()).unwrap_or(Value::Empty);
    let mut out = Plan { changes: Vec::new(), at: Vec::new(), problems: Vec::new(), added: 0, removed: 0 };
    let mut seen = vec![false; originals.len()];
    for r in data.start.row..=data.end.row {
        let kv: Vec<Value> = key.iter().map(|&k| pos.get(k).copied().flatten().map(|o| value_at(r, o)).unwrap_or(Value::Empty)).collect();
        if kv.iter().all(|v| matches!(v, Value::Empty)) {
            if (0..pos.len()).filter_map(|c| pos.get(c).copied().flatten()).any(|o| !matches!(value_at(r, o), Value::Empty)) {
                out.added += 1;
            }
            continue;
        }
        let Some(&i) = index.get(&kv.iter().map(repr).collect::<Vec<_>>().join("\u{1f}")) else {
            out.problems.push(format!("row {}: its key isn't one the database had", r + 1));
            continue;
        };
        let Some(orig) = originals.get(i) else { continue };
        if let Some(s) = seen.get_mut(i) {
            *s = true;
        }
        let mut set = Vec::new();
        for (c, col) in src.columns.iter().enumerate() {
            let Some(off) = pos.get(c).copied().flatten() else { continue };
            if key.contains(&c) {
                continue;
            }
            let now = value_at(r, off);
            if repr(&now) == repr(&shown_at(orig, c)) {
                continue;
            }
            let k = kinds.get(c).copied().unwrap_or(Kind::Other);
            match to_db(&now, col, k, sys) {
                Ok(v) => set.push((c, orig.get(c).map(db_value).unwrap_or(DbValue::Null), v)),
                Err(m) => out.problems.push(format!("{}: {m}", CellRef::new(r, data.start.col + off).a1())),
            }
        }
        if !set.is_empty() {
            out.changes.push(RowChange { key: key.iter().map(|&k| orig.get(k).map(db_value).unwrap_or(DbValue::Null)).collect(), set });
            out.at.push((r, i));
        }
    }
    out.removed = seen.iter().filter(|s| !**s).count();
    Ok(out)
}

/// The rows of the database table now, in the order of the source's columns, for a table loaded
/// before the workbook was opened.
fn read_now(s: &Session, conn: &Conn, src: &DbSource, id: &str) -> Result<Vec<Vec<DbCell>>> {
    let data = read(s, conn, src.schema.as_deref(), &src.table)?;
    let at: Vec<usize> = src
        .columns
        .iter()
        .map(|c| {
            data.columns
                .iter()
                .position(|d| d.name == c.name)
                .ok_or_else(|| bad(id, format!("the column \"{}\" is no longer in the database", c.name)))
        })
        .collect::<Result<_>>()?;
    Ok(data.rows.into_iter().map(|row| at.iter().map(|&i| row.get(i).cloned().map(db_cell).unwrap_or(DbCell::Null)).collect()).collect())
}

pub(super) fn save(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.dbSave";
    let (id, src) = target(s, p, ID)?;
    let DbRowKey::Columns(key) = src.key.clone() else {
        return Err(bad(ID, format!("\"{}\" has no primary key, so GridKu can't tell its rows apart to save them", src.table)));
    };
    let conn = Conn::of_source(&src, p, ID)?;
    let wb = s.doc()?.wb.clone();
    let (si, tname, range) = table_by_id(&wb, id).ok_or_else(|| bad(ID, "the table loaded from the database is gone"))?;
    let t = find_table(&wb, id).ok_or_else(|| bad(ID, "the table loaded from the database is gone"))?;
    let data = t.data_range().ok_or_else(|| bad(ID, "the table has no rows"))?;
    let hs = heads(&wb, si, range);
    let pos: Vec<Option<u32>> = src.columns.iter().map(|c| hs.iter().position(|h| *h == c.name).and_then(|p| u32::try_from(p).ok())).collect();
    if let Some(k) = key.iter().find(|&&k| pos.get(k).copied().flatten().is_none()) {
        let name = src.columns.get(*k).map(|c| c.name.as_str()).unwrap_or("");
        return Err(bad(ID, format!("the key column \"{name}\" is no longer in the table")));
    }
    let originals = match &src.original.0 {
        Some(o) => o.clone(),
        None if bool_param(p, "overwrite").unwrap_or(false) => Arc::new(read_now(s, &conn, &src, ID)?),
        None => return Err(EngineError::Other(NEEDS_OVERWRITE.into())),
    };
    let kinds: Vec<Kind> = src.columns.iter().map(|c| kind(c, src.engine)).collect();
    let plan = plan(&wb, si, data, &src, &pos, &key, &originals, &kinds)?;
    if !plan.problems.is_empty() {
        return Err(EngineError::Other(format!(
            "Nothing was saved: {} value{} don't fit the database. {}",
            plan.problems.len(),
            if plan.problems.len() == 1 { "" } else { "s" },
            some_of(&plan.problems, 5)
        )));
    }
    let summary = |saved: usize| json!({"table": tname, "saved": saved, "added": plan.added, "removed": plan.removed});
    if plan.changes.is_empty() {
        return Ok(summary(0));
    }
    let names: Vec<String> = src.columns.iter().map(|c| c.name.clone()).collect();
    let to = WriteTarget { schema: src.schema.as_deref(), table: &src.table, columns: &names, key: &key };
    let back = write_to_db(s, &conn, to, &plan.changes).map_err(|e| match e {
        DbError::Cancelled => EngineError::Cancelled,
        DbError::Conflict(rows) => {
            let at: Vec<String> = rows.iter().filter_map(|&i| plan.at.get(i)).map(|(r, _)| (r + 1).to_string()).collect();
            EngineError::Other(format!(
                "Nothing was saved: {} row{} changed in the database since {} loaded (sheet row {}). Refresh the table to see {}; your changes there will be replaced.",
                at.len(),
                if at.len() == 1 { " was" } else { "s were" },
                if at.len() == 1 { "it was" } else { "they were" },
                some_of(&at, 10),
                if at.len() == 1 { "it" } else { "them" },
            ))
        }
        e => db_err(e),
    })?;
    // The rows as the database now keeps them, in the sheet and as the rows loaded.
    let sys = wb.date_system;
    let uppers: Vec<String> = src.columns.iter().map(|c| c.data_type.to_ascii_uppercase()).collect();
    let mut fresh: Vec<Vec<DbCell>> = (*originals).clone();
    let mut cells: Vec<(CellRef, Value)> = Vec::new();
    for ((r, i), row) in plan.at.iter().zip(back) {
        let row: Vec<DbCell> = row.into_iter().map(db_cell).collect();
        for (c, v) in row.iter().enumerate() {
            if let (Some(off), false) = (pos.get(c).copied().flatten(), key.contains(&c)) {
                cells.push((CellRef::new(*r, data.start.col + off), shown(v, uppers.get(c).map(String::as_str).unwrap_or(""), sys).0));
            }
        }
        if let Some(slot) = fresh.get_mut(*i) {
            *slot = row;
        }
    }
    let fresh = DbOriginal(Some(Arc::new(fresh)));
    let saved = plan.changes.len();
    unlocked(s, |s| {
        edit(s, |cx| {
            let sh = cx.sheet_mut(si)?;
            for (c, v) in &cells {
                if sh.cell(*c).map(|x| &x.value) != Some(v) {
                    sh.set_value(*c, v.clone());
                }
            }
            cx.changed.extend(cells.iter().map(|(c, _)| (si, *c)));
            for o in cx.wb.db_sources.iter_mut().filter(|o| o.table_id == id) {
                let mut n = (**o).clone();
                n.original = fresh.clone();
                *o = Arc::new(n);
            }
            Ok(())
        })
    })?;
    // What is in the database can't be taken back by Undo.
    let d = s.doc_mut()?;
    d.undo.clear();
    d.redo.clear();
    Ok(summary(saved))
}
