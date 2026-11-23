# Databases: Data › DB Connect

> **Last reviewed:** 2026-11-24 · **Last updated:** 2026-11-24 · **Change:** minor (a progress bar and Cancel while a large table loads; earlier: a loaded table's source is saved in the XLSX file; earlier: created, stage 1, reading SQLite) · **Target:** beyond Excel (Excel reads databases through Get Data and ODBC and never writes back)

GridKu reads tables from a database into sheets, and later will write changes back. This page
records the design of the first stage and what it keeps for the next ones.

## Stages

| Stage | What works |
|---|---|
| 1 (built) | SQLite: list a database file's tables and views, show a table's fields, load one into a sheet as a table, refresh it |
| 2 | PostgreSQL in the same pane: host, port, database, user and SSL; the password is asked each time and never stored |
| 3 | Editing: changes to a loaded table are written back to the database |

## The pane

Data › **DB Connect** opens a task pane on the right.

- **Top:** the engine, as radio buttons (SQLite; PostgreSQL is shown, not yet available), and
  the database: a SQLite file's path, typed or chosen with **Browse…**, then **Connect**.
- **Bottom:** the database's **Tables** and **Views**, each group by name. A click selects one;
  **Load** puts it in a sheet. Right-clicking one offers **Table Information…** (a window listing
  each field's name, type, length, whether it may be NULL and its place in the primary key) and
  **Load**. **Refresh** reads again the table at the active cell.

Every action is a command, so the CLI, the control channel and MCP reach them too:
`data.dbTables`, `data.dbColumns`, `data.dbLoad`, `data.dbRefresh` (`docs/control-protocol.md`
lists them through `app.commands`).

## Decisions

1. **Read-only, and nothing runs because a file says so.** The database file is opened
   read-only and never created; its schema isn't trusted (`trusted_schema=OFF`), no statement
   can write (`query_only`), no extension is loaded. The only statements are the catalogue's and
   one `SELECT` of the table chosen. A workbook that holds a loaded table doesn't connect when it
   opens: Refresh is asked for.
2. **Bounded.** A read stops at a sheet's rows (1,048,575 below the header), its columns, 20
   million cells and two minutes; a text is cut at a cell's 32,767 characters. The command says
   when a table was cut short.
3. **Loaded as a table.** The data goes in a new sheet named after the database table (the
   characters a sheet name can't hold become `_`, at most 31 characters, and `orders (2)` when
   the name is taken), from A1, with a header row, as a table with filter buttons and the default
   style; its columns are fitted to their contents. It is one Undo step. Saved, it is an ordinary
   table: the workbook opens and shows it without the database.
4. **Loading again refreshes.** Loading a database table already loaded (same engine, file and
   table, its sheet table still there) reads it again into that table instead of adding a sheet.
   Refresh grows or shrinks the table with the data; the cells it grows into must be empty and in
   no other table, else nothing changes and it says what is in the way. Rows that are gone are
   emptied. Column widths the person set stay.
5. **Values.** NULL is an empty cell. Integers and reals are numbers, except an integer beyond
   2^53, which a number can't hold exactly: it is text. A column declared as a boolean takes 0
   and 1 as FALSE and TRUE. Text in a column declared as a date or time is a date when it reads as
   one (`2026-01-31`, `2026-01-31 10:20:00`, `2026-01-31T10:20:00Z`), with the date format. A BLOB
   shows its size (`[BLOB 1024 bytes]`).
6. **What is kept for stage 3.** The workbook keeps, per loaded table (`Workbook::db_sources`,
   by the sheet table's id): the engine, the file, the database table, each column's declared type
   split into type and length (`VARCHAR(50)` → `VARCHAR`, `50`), whether it may be NULL, its place
   in the primary key, and how the rows are told apart: the primary key's columns, else SQLite's
   `rowid` with each row's, else nothing (a view). Never a password.
7. **Desktop only.** SQLite's C library is built into the app (`rusqlite` with `bundled`, MIT;
   SQLite is public domain): no system library is needed. It doesn't build for the web
   (wasm32), where the commands say a database can't be read.
8. **Macros.** A macro step that reads a database needs the person's permission, as reading a
   file does.

9. **Progress, and Cancel.** Load and Refresh run in the background like other long commands
   (`ui-egui/src/background.rs`): the busy box says **Reading rows…**, then **Writing rows…**,
   with a progress bar and the count (`412,000 / 1,048,575 (39%)`), counted first with
   `count(*)`. Cancel (or Esc) stops it in either phase, also inside a slow SQLite statement,
   and nothing changes. The engine reports through `Session::progress`, which any long command
   can use.
10. **Saved in the file, where Excel keeps it.** A workbook with loaded tables saved as XLSX has
   one custom XML part (ECMA-376 Part 1, 22.5: `customXml/itemN.xml` with `itemPropsN.xml`,
   related from the workbook), the place the format gives other applications' data. Excel
   neither reads nor runs it, asks nothing about it, and keeps it when it saves the workbook.
   Its root is `gridkuDbSources` in the namespace `urn:gridku:db-sources:1`; each `source` names
   its sheet table by name and holds the engine, the database file, the database table, each
   column's declared type and how rows are told apart (SQLite rowids are not written: a refresh
   reads them again). Excel's own data connections (`connections.xml`, query tables) are not
   used: Excel would take them for its own, warn that they are disabled and try to refresh them.
11. **The database file relative to the workbook.** Saved to a file, the database's path is
   written relative to the workbook's folder (`data/toko.db`, `../shared/toko.db`), as links to
   other workbooks are, so the file names no folder above its own and the two can move
   together; on another drive it is a `file:///` address. Opened again, it is the path on this
   system. Custom XML of other applications is kept as it was, and GridKu's part is written once.

## Not yet

- PostgreSQL (stage 2) and writing back (stage 3).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-24 | minor | Loading or refreshing a large table shows Reading rows… then Writing rows… with a progress bar and the count in the busy box; Cancel stops it in either phase and nothing changes |
| 2026-11-24 | minor | A loaded table's source is saved in the XLSX file as a custom XML part (which Excel keeps), the database file relative to the workbook; opened again, Refresh and Load find it |
| 2026-11-24 | major | Created. Stage 1: Data › DB Connect reads SQLite files read-only; Tables and Views, Table Information (field, type, length), Load as a table in a new sheet, Refresh; column types and row keys kept for writing back later |
