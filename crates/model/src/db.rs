//! Tables loaded from a database (Data › DB Connect): where each came from, so it can be
//! refreshed, and what a later stage needs to write changes back — each column's declared type,
//! and how the rows are told apart.
//!
//! A source belongs to the [`Table`](crate::Table) its data was loaded into, by the table's id:
//! deleting the table (or its sheet) leaves a source that names no table, which is ignored. No
//! password is ever kept here.

use serde::{Deserialize, Serialize};

/// Which database engine a source is read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbEngine {
    Sqlite,
}

impl DbEngine {
    pub fn id(self) -> &'static str {
        match self {
            DbEngine::Sqlite => "sqlite",
        }
    }

    pub fn parse(s: &str) -> Option<DbEngine> {
        s.eq_ignore_ascii_case("sqlite").then_some(DbEngine::Sqlite)
    }
}

/// One column as the database declares it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbColumn {
    pub name: String,
    pub data_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<String>,
    pub nullable: bool,
    /// Its place in the primary key, from 1; 0 when it isn't part of it.
    #[serde(default)]
    pub primary_key: u32,
}

/// How the rows loaded are told apart in the database.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbRowKey {
    /// By the primary key: the indexes of its columns, in key order.
    Columns(Vec<usize>),
    /// By SQLite's `rowid`, one per row loaded, in the order loaded.
    Rowid(Vec<i64>),
    /// Not at all (a view, or a table with neither).
    #[default]
    None,
}

/// A table of a database loaded into a sheet's table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSource {
    pub engine: DbEngine,
    /// Where the database is: the file's path for SQLite.
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// The database table or view loaded.
    pub table: String,
    /// The id of the sheet table ([`crate::Table::id`]) the data is in.
    pub table_id: u32,
    pub columns: Vec<DbColumn>,
    #[serde(default)]
    pub key: DbRowKey,
}
