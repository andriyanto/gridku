# Databases: Data › DB Connect

> **Last reviewed:** 2026-11-24 · **Last updated:** 2026-11-24 · **Change:** minor (stage 3: changed values are saved back to the database, with the header row and key columns locked and each column validated by its type; earlier: waiting for a server shows what is happening, with Cancel; earlier: stage 2: PostgreSQL; earlier: a progress bar and Cancel while a large table loads; earlier: a loaded table's source is saved in the XLSX file; earlier: created, stage 1, reading SQLite) · **Target:** beyond Excel (Excel reads databases through Get Data and ODBC and never writes back)

GridKu reads tables from a database into sheets and writes the values changed in them back. This
page records the design of each stage.

## Stages

| Stage | What works |
|---|---|
| 1 (built) | SQLite: list a database file's tables and views, show a table's fields, load one into a sheet as a table, refresh it |
| 2 (built) | PostgreSQL in the same pane: host, port, database, user, password and SSL; tables and views schema by schema; the password is asked for each time and never stored |
| 3 (built) | Saving: the values changed in a loaded table are written back to the database; the header row and key columns are locked, and each column is validated by its type |
| 4 | Adding and deleting rows |

## The pane

Data › **DB Connect** opens a task pane on the right.

- **Top:** the engine, as radio buttons (SQLite, PostgreSQL), and the database: a SQLite
  file's path, typed or chosen with **Browse…**; or a PostgreSQL server's **Host**, **Port**
  (5432), **Database**, **User**, **Password** and **SSL** (When offered, Required, Off). Then
  **Connect**.
- **Bottom:** the database's **Tables** and **Views**, each group by name; PostgreSQL's under
  each schema (`public` open, the system's own left out). A click selects one;
  **Load** puts it in a sheet. Right-clicking one offers **Table Information…** (a window listing
  each field's name, type, length, whether it may be NULL and its place in the primary key) and
  **Load**. **Refresh** reads again the table at the active cell, and **Save to Database** writes
  the values changed in it back.

Every action is a command, so the CLI, the control channel and MCP reach them too:
`data.dbTables`, `data.dbColumns`, `data.dbLoad`, `data.dbRefresh`, `data.dbSave`
(`docs/control-protocol.md` lists them through `app.commands`).

## Decisions

1. **Reading is read-only, and nothing runs because a file says so.** Listing, Table
   Information, Load and Refresh open the database file read-only (only Save to Database opens it
   to write, decision 19), and it is never created; its schema isn't trusted (`trusted_schema=OFF`), no statement
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
   with the count (`412,000 / 1,048,575 (39%)`) and under it a bar as wide as the box, a light
   band sweeping across its filled part, counted first with
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

12. **PostgreSQL.** The `postgres` client (MIT OR Apache-2.0) connects within 15 seconds; with
   SSL it uses rustls with ring and checks the server's certificate against the computer's
   certificate authorities (*When offered*: TLS if the server offers it; *Required*: refused
   without it; *Off*: never). The session is read-only (`default_transaction_read_only`), its
   statements stop at the time limit (`statement_timeout`), and dates come as ISO text. Each
   column is read as text and turned into a value by its type: integers, reals and `numeric`
   (as text beyond 15 significant digits) are numbers, `boolean` TRUE/FALSE, `date`, `time` and
   `timestamp` dates, `timestamp with time zone` in the session's time zone, `bytea` its size,
   anything else (`json`, `uuid`, arrays, enums) its text. Rows are told apart by the primary
   key (PostgreSQL has no stable row id). A table of a schema other than `public` is loaded into
   a sheet named `schema.table`.
13. **The password.** It is a parameter of each call (`password`), which the engine never keeps
   and the journal, recorded macros and scripts leave out. The pane holds it in memory while it
   is connected; Connect asks for it again. A loaded table keeps its connection as a URL without
   it (`postgresql://user@host:5432/db?sslmode=prefer`), so the saved file has none; Refresh of
   a table whose server the pane isn't connected to (a workbook opened from a file) asks for it
   in a **Database Password** box.
14. **Waiting.** Connect and Table Information… run on their own thread, so the window never
   waits for a server: the pane shows a spinner and what it is doing (**Connecting to
   localhost…**, **Opening toko.db…**, **Reading the fields of pelanggan…**) with a
   **Cancel** button, which drops the answer when it comes; Connect, Load and Refresh are off
   meanwhile. In the busy box of Load and Refresh, **Connecting…** and **Counting rows…** come
   before the counted phases, with a bar that moves back and forth because nothing can be
   counted yet; Cancel works in every phase.

## Saving changes back (stage 3)

15. **Only values.** Saving runs `UPDATE`s of rows that are already in the database, nothing
   else: no column is renamed, added or dropped, and no row is added or deleted (stage 4). The
   result counts the rows added to the table and those taken out of it, and says they weren't
   written.
16. **Locked: the header row and the key.** The header row holds the database's column names, and
   the primary key's columns tell the database which row is which, so an edit that changes either
   is refused, whichever command makes it (`cmd::data::db_write::check_locks`, beside sheet
   protection in `commit`). Keys may move with their rows (a sort, a row deleted from the table)
   and a row added to the table leaves its key empty. Columns may be added at the right end of the
   table (they aren't the database's and aren't saved); a column inserted, deleted or moved inside
   the table changes the header and is refused. Load, Refresh and Save themselves change both. A
   table loaded from a view, or from a table without a primary key, can't be saved: its rows can't
   be told apart once the table is sorted.
17. **Validation from the type.** Loading gives each column other than the key a data validation
   rule from its declared type, with a Stop message naming the column and its type: whole
   numbers within the type's range (`smallint`, `integer`, `bigint`; SQLite's `INTEGER`), numbers
   (`real`, `double precision`), numbers of at most so many digits before the point
   (`numeric(p,s)`, `DECIMAL(p,s)`), TRUE or FALSE (a list), dates, times, and texts of at most so
   many characters (`varchar(n)`); a column that is `NOT NULL` doesn't ignore a blank. A column
   without a rule (`text`, `json`, `uuid`, an enum, an array, a BLOB) is checked by the database.
   Refresh puts the rules again over the table's new rows.
18. **Checked again when saving.** Validation only sees what is typed; a paste, a fill or a macro
   passes it. So Save converts every changed value by its column's type, and if one doesn't fit
   (empty in a `NOT NULL` column, text in a number column, too long, too many digits, a changed
   BLOB) nothing is written and the message names the cells (`B2: customer is VARCHAR(40): at
   most 40 characters`). Values go to the database as the type takes them: whole numbers as
   integers, dates as `YYYY-MM-DD`, times as `HH:MM:SS`, booleans as 1 and 0; PostgreSQL casts
   each from text to the column's type, and storing it checks the length and precision (a text
   too long is refused, never cut).
19. **One transaction, and nobody's changes lost.** The workbook keeps in memory (never in the
   file) each row as the database had it when it was loaded (`DbSource::original`). Saving
   compares the sheet with it to find the values changed; then, in one transaction, it reads
   each of those rows again by its key (PostgreSQL: `FOR UPDATE`), and if a value it changes is
   no longer what was loaded, or the row is gone, nothing is written and the message names the
   sheet rows: Refresh shows the database's values (replacing the person's in those rows). A
   value changed there in another column of the same row is no conflict. The rows saved are read
   back, so the sheet shows what the database keeps (`numeric(12,2)` rounds 12.345 to 12.35), and
   become the rows as loaded for the next save.
20. **A table loaded before the workbook was opened.** Its rows as loaded aren't known, so the
   changes can't be told from changes made in the database since. Save then asks first, and with
   `overwrite` writes every value of the table that differs from the database now; Refresh, then
   editing, saves safely.
21. **Progress, Cancel, and no Undo.** Save runs in the background like Load: the busy box says
   **Connecting…**, **Checking rows…** and **Saving rows…** with the count and the bar, and Cancel
   stops it before anything is committed. What is in the database can't be taken back by Undo, so
   a save clears Undo and Redo.

## Not yet

- Adding and deleting rows (stage 4).
- Materialized views and foreign tables other than those `information_schema` lists.

## Testing against PostgreSQL

The PostgreSQL tests run when `GRIDKU_TEST_PG` names a server (`host:port:user:password`) with a
database `toko` made by this script; without it they are skipped.

```sql
CREATE DATABASE toko;
\c toko
CREATE SCHEMA gudang;
CREATE TYPE mood AS ENUM ('senang', 'sedih');
CREATE TABLE public.pelanggan (id serial PRIMARY KEY, nama varchar(60) NOT NULL, saldo numeric(12,2), aktif boolean,
  daftar date, waktu timestamptz, besar bigint, foto bytea, data jsonb, suasana mood, tags int4[]);
INSERT INTO public.pelanggan (nama, saldo, aktif, daftar, waktu, besar, foto, data, suasana, tags) VALUES
  ('Ana', 1234.50, true, '2025-01-31', '2026-01-31 10:20:00+07', 9007199254740993, '\x0102', '{"a":1}', 'senang', '{1,2}'),
  ('Budi', NULL, false, NULL, NULL, 5, NULL, NULL, NULL, NULL);
CREATE TABLE gudang.stok (kode char(8), lokasi text, jumlah int, PRIMARY KEY (lokasi, kode));
INSERT INTO gudang.stok SELECT 'K' || g, 'Rak ' || (g % 5), g FROM generate_series(1, 2500) g;
CREATE VIEW public.aktif AS SELECT nama FROM public.pelanggan WHERE aktif;
```

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-24 | minor | Stage 3: Save to Database writes the values changed in a loaded table back, in one transaction, rows found by their primary key and compared with the rows as loaded; the header row and key columns are locked; each column gets data validation from its type, and saving checks every changed value again; progress and Cancel; Undo cleared |
| 2026-11-24 | minor | Connect and Table Information run in the background with a spinner, the step in words and Cancel in the pane; Load and Refresh say Connecting… and Counting rows… with a moving bar before the counted phases |
| 2026-11-24 | minor | Stage 2: PostgreSQL in DB Connect (host, port, database, user, password, SSL); tables and views schema by schema, Table Information, Load and Refresh; read-only sessions over TLS with the system's certificate authorities; the password asked for each time and never kept |
| 2026-11-24 | minor | Loading or refreshing a large table shows Reading rows… then Writing rows… with a progress bar and the count in the busy box; Cancel stops it in either phase and nothing changes |
| 2026-11-24 | minor | A loaded table's source is saved in the XLSX file as a custom XML part (which Excel keeps), the database file relative to the workbook; opened again, Refresh and Load find it |
| 2026-11-24 | major | Created. Stage 1: Data › DB Connect reads SQLite files read-only; Tables and Views, Table Information (field, type, length), Load as a table in a new sheet, Refresh; column types and row keys kept for writing back later |
