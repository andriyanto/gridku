//! GridKu's database sources (Data › DB Connect): list the tables and views of a database, the
//! columns of one, and read one, for the engine to load into a sheet.
//!
//! SQLite only for now. A database file is opened read-only, never created, and nothing in it is
//! changed: no statement is run but the catalogue queries and one `SELECT` of the table asked
//! for. The file is hostile input: the schema it holds is not trusted (`trusted_schema=OFF`), no
//! extension can be loaded, and a read is bounded in rows, cells, text length and time.
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

use std::time::Duration;

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
    #[error("databases can't be read in the web version")]
    Unavailable,
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

/// Reads the table or view `table` of the SQLite database at `path`, within `limits`.
pub fn sqlite_read(path: &str, table: &str, limits: Limits) -> Result<TableData, DbError> {
    #[cfg(not(target_arch = "wasm32"))]
    return sqlite::read(path, table, limits);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, table, limits);
        Err(DbError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
