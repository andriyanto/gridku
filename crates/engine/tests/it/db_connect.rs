//! Data › DB Connect (`docs/databases.md`): listing a SQLite database's tables and columns,
//! loading a table into a sheet as a table, and Refresh.

use gridcraft_core::{CellRef, Value};
use gridcraft_engine::Session;
use serde_json::{Value as Json, json};

fn run(s: &mut Session, id: &str, params: Json) -> Json {
    s.execute(id, params).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// A database file for one test, removed when dropped.
struct Db(std::path::PathBuf);

impl Db {
    fn new(name: &str, sql: &str) -> Db {
        let path = std::env::temp_dir().join(format!("gridku-dbconnect-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        rusqlite::Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        Db(path)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
    fn exec(&self, sql: &str) {
        rusqlite::Connection::open(&self.0).unwrap().execute_batch(sql).unwrap();
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const SHOP: &str = "
    CREATE TABLE orders (id INTEGER PRIMARY KEY, customer VARCHAR(40) NOT NULL, total DECIMAL(10,2), paid BOOLEAN, at DATETIME, big INTEGER, note TEXT, scan BLOB);
    INSERT INTO orders VALUES (1, 'Ana', 12.5, 1, '2026-01-31 10:20:00', 9007199254740993, NULL, x'0102');
    INSERT INTO orders VALUES (2, 'Budi', 7, 0, '2026-02-01T08:00:00Z', 5, 'rush', NULL);
    CREATE VIEW paid_orders AS SELECT id, total FROM orders WHERE paid;
";

fn cell(s: &Session, sheet: &str, a1: &str) -> Value {
    let wb = &s.doc().unwrap().wb;
    wb.sheet(wb.sheet_index(sheet).unwrap()).unwrap().value(CellRef::parse(a1).unwrap())
}

#[test]
fn lists_tables_and_columns() {
    let db = Db::new("list", SHOP);
    let mut s = Session::new();
    s.new_workbook();
    let t = run(&mut s, "data.dbTables", json!({"path": db.path()}));
    assert_eq!(t["tables"], json!([{"name": "orders", "kind": "table"}, {"name": "paid_orders", "kind": "view"}]));
    let c = run(&mut s, "data.dbColumns", json!({"path": db.path(), "table": "orders"}));
    assert_eq!(c["columns"][0], json!({"name": "id", "type": "INTEGER", "length": null, "nullable": true, "primaryKey": 1}));
    assert_eq!(c["columns"][1], json!({"name": "customer", "type": "VARCHAR", "length": "40", "nullable": false, "primaryKey": 0}));
    assert_eq!(c["columns"][2]["length"], "10,2");
    assert!(s.execute("data.dbTables", json!({"path": "/no/such/file.db"})).is_err());
    assert!(s.execute("data.dbTables", json!({"engine": "oracle", "path": db.path()})).is_err());
    assert!(s.execute("data.dbColumns", json!({"path": db.path(), "table": "nope"})).is_err());
}

#[test]
fn load_puts_the_table_in_a_new_sheet_and_refresh_reads_it_again() {
    let db = Db::new("load", SHOP);
    let mut s = Session::new();
    s.new_workbook();
    let r = run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "orders"}));
    assert_eq!(
        (r["sheet"].as_str(), r["range"].as_str(), r["rows"].as_u64(), r["refreshed"].as_bool()),
        (Some("orders"), Some("A1:H3"), Some(2), Some(false))
    );
    let wb = &s.doc().unwrap().wb;
    assert_eq!(wb.sheets.len(), 2);
    assert_eq!(wb.active_sheet(), 1, "the new sheet is shown");
    let sh = wb.sheet(1).unwrap();
    assert_eq!(sh.tables.len(), 1);
    assert_eq!(
        sh.tables[0].columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "customer", "total", "paid", "at", "big", "note", "scan"]
    );
    // Values.
    assert_eq!(cell(&s, "orders", "B2"), Value::text("Ana"));
    assert_eq!(cell(&s, "orders", "C2"), Value::Number(12.5));
    assert_eq!((cell(&s, "orders", "D2"), cell(&s, "orders", "D3")), (Value::Bool(true), Value::Bool(false)));
    assert!(matches!(cell(&s, "orders", "E2"), Value::Number(n) if (n - 46053.430555).abs() < 1e-3), "{:?}", cell(&s, "orders", "E2"));
    assert!(matches!(cell(&s, "orders", "E3"), Value::Number(_)), "ISO with T and Z");
    assert_eq!(cell(&s, "orders", "F2"), Value::text("9007199254740993"), "beyond 2^53 stays exact as text");
    assert_eq!(cell(&s, "orders", "G2"), Value::Empty);
    assert_eq!(cell(&s, "orders", "H2"), Value::text("[BLOB 2 bytes]"));
    let wb = &s.doc().unwrap().wb;
    let fmt = wb.styles.get(wb.sheet(1).unwrap().style_id(CellRef::parse("E2").unwrap())).num_fmt.as_str().to_string();
    assert!(fmt.contains('y') || fmt.contains('m'), "a date format: {fmt}");
    // The source, with the column types and the key.
    let src = &wb.db_sources[0];
    assert_eq!((src.table.as_str(), src.columns.len(), &src.key), ("orders", 8, &gridcraft_engine::model::DbRowKey::Columns(vec![0])));
    assert_eq!((src.columns[1].data_type.as_str(), src.columns[1].length.as_deref()), ("VARCHAR", Some("40")));
    // One Undo takes the whole load back.
    run(&mut s, "edit.undo", json!({}));
    assert_eq!((s.doc().unwrap().wb.sheets.len(), s.doc().unwrap().wb.db_sources.len()), (1, 0));
    run(&mut s, "edit.redo", json!({}));
    assert_eq!(s.doc().unwrap().wb.sheets.len(), 2);

    // The database changes; Refresh grows the table, and loading it again refreshes it too.
    db.exec("INSERT INTO orders (id, customer, total) VALUES (3, 'Citra', 1); UPDATE orders SET customer = 'Ani' WHERE id = 1;");
    run(&mut s, "selection.set", json!({"range": "B2"}));
    let r = run(&mut s, "data.dbRefresh", json!({}));
    assert_eq!((r["range"].as_str(), r["refreshed"].as_bool()), (Some("A1:H4"), Some(true)));
    assert_eq!((cell(&s, "orders", "B2"), cell(&s, "orders", "B4")), (Value::text("Ani"), Value::text("Citra")));
    assert_eq!(s.doc().unwrap().wb.sheet(1).unwrap().tables[0].range.a1(), "A1:H4");
    db.exec("DELETE FROM orders WHERE id > 1;");
    let r = run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "orders"}));
    assert_eq!((r["range"].as_str(), r["refreshed"].as_bool()), (Some("A1:H2"), Some(true)));
    assert_eq!(s.doc().unwrap().wb.sheets.len(), 2, "no second sheet");
    assert_eq!((cell(&s, "orders", "B3"), cell(&s, "orders", "B4")), (Value::Empty, Value::Empty), "the rows gone are cleared");
    // Growing into a cell that holds something is refused, and nothing changes.
    db.exec("INSERT INTO orders (id, customer) VALUES (7, 'Dewi');");
    run(&mut s, "cell.set", json!({"cell": "A3", "input": "mine"}));
    assert!(s.execute("data.dbRefresh", json!({"table": r["table"]})).is_err());
    assert_eq!(cell(&s, "orders", "A3"), Value::text("mine"));
}

#[test]
fn sheet_names_are_made_valid_and_unused() {
    let db =
        Db::new("names", "CREATE TABLE \"a/b:c\" (x); INSERT INTO \"a/b:c\" VALUES (1); CREATE TABLE Sheet1 (y); INSERT INTO Sheet1 VALUES (2);");
    let mut s = Session::new();
    s.new_workbook();
    let r = run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "a/b:c"}));
    assert_eq!(r["sheet"], "a_b_c");
    let r = run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "Sheet1"}));
    assert_eq!(r["sheet"], "Sheet1 (2)", "the workbook's own Sheet1 is left alone");
    // A view loads too.
    let v = Db::new("view", SHOP);
    let r = run(&mut s, "data.dbLoad", json!({"path": v.path(), "table": "paid_orders"}));
    assert_eq!((r["sheet"].as_str(), r["rows"].as_u64()), (Some("paid_orders"), Some(1)));
}

#[test]
fn a_saved_workbook_keeps_the_table_without_the_database() {
    let db = Db::new("save", SHOP);
    let mut s = Session::new();
    s.new_workbook();
    run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "orders"}));
    let bytes = gridcraft_xlsx::write_xlsx(&s.doc().unwrap().wb).unwrap();
    drop(db);
    let (wb, _) = gridcraft_xlsx::read_xlsx(&bytes).unwrap();
    let sh = wb.sheet(wb.sheet_index("orders").unwrap()).unwrap();
    assert_eq!(sh.tables.len(), 1);
    assert_eq!(sh.value(CellRef::parse("B3").unwrap()), Value::text("Budi"));
}

/// The names of the parts of an XLSX package, and one part's text.
fn zip_parts(bytes: &[u8]) -> Vec<String> {
    let z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    z.file_names().filter_map(Result::ok).map(|n| n.into_owned()).collect()
}

fn zip_text(bytes: &[u8], name: &str) -> String {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

/// Saved to a file, a loaded table's source goes in a custom XML part (Excel keeps those), with
/// the database file relative to the workbook's folder; opened again, the table refreshes from
/// it, and loading the database table again refreshes it too. Saved again, there is still one
/// such part.
#[test]
fn the_source_is_saved_in_the_file_relative_to_it_and_refreshes_after_opening() {
    let dir = std::env::temp_dir().join(format!("gridku-dbsave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let db_path = dir.join("data").join("toko lama.db");
    rusqlite::Connection::open(&db_path).unwrap().execute_batch(SHOP).unwrap();
    let book = dir.join("Laporan.xlsx").to_string_lossy().into_owned();
    let mut s = Session::new();
    s.new_workbook();
    run(&mut s, "data.dbLoad", json!({"path": db_path.to_string_lossy(), "table": "orders"}));
    run(&mut s, "file.saveAs", json!({"path": book}));
    let bytes = std::fs::read(&book).unwrap();
    let parts = zip_parts(&bytes);
    assert!(parts.contains(&"customXml/item1.xml".to_string()), "{parts:?}");
    assert!(parts.contains(&"customXml/itemProps1.xml".to_string()), "{parts:?}");
    let item = zip_text(&bytes, "customXml/item1.xml");
    assert!(item.contains("xmlns=\"urn:gridku:db-sources:1\""), "{item}");
    assert!(item.contains("location=\"data/toko%20lama.db\""), "relative to the workbook: {item}");
    assert!(!item.contains(&dir.to_string_lossy().to_string()), "no folder above the workbook's: {item}");
    assert!(item.contains("<column name=\"customer\" type=\"VARCHAR\" length=\"40\" nullable=\"0\" primaryKey=\"0\"/>"), "{item}");
    assert!(item.contains("<key kind=\"columns\" columns=\"0\"/>"), "{item}");
    assert!(zip_text(&bytes, "xl/_rels/workbook.xml.rels").contains("relationships/customXml\" Target=\"../customXml/item1.xml\""));
    assert!(
        zip_text(&bytes, "[Content_Types].xml")
            .contains("/customXml/itemProps1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.customXmlProperties+xml\"")
    );

    // Opened again in a new session: the source is back, with the database's path on this system.
    let mut again = Session::new();
    run(&mut again, "file.open", json!({"path": book}));
    let src = again.doc().unwrap().wb.db_sources.first().cloned().expect("the source is read back");
    assert_eq!(std::path::Path::new(&src.location), db_path.as_path());
    assert_eq!((src.table.as_str(), src.columns.len()), ("orders", 8));
    rusqlite::Connection::open(&db_path).unwrap().execute_batch("INSERT INTO orders (id, customer) VALUES (3, 'Citra');").unwrap();
    run(&mut again, "sheet.activate", json!({"sheet": "orders"}));
    run(&mut again, "selection.set", json!({"range": "A2"}));
    let r = run(&mut again, "data.dbRefresh", json!({}));
    assert_eq!(r["range"], "A1:H4");
    let r = run(&mut again, "data.dbLoad", json!({"path": db_path.to_string_lossy(), "table": "orders"}));
    assert_eq!((r["refreshed"].as_bool(), again.doc().unwrap().wb.sheets.len()), (Some(true), 2));
    // Saved again: one part, not a kept copy beside the new one.
    run(&mut again, "file.save", json!({}));
    let bytes = std::fs::read(&book).unwrap();
    let items = zip_parts(&bytes).into_iter().filter(|p| p.starts_with("customXml/item") && !p.contains("Props")).count();
    assert_eq!(items, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// PostgreSQL, against a real server when `GRIDKU_TEST_PG` names one (`host:port:user:password`)
/// with the database `toko` of `docs/databases.md`'s test script; skipped otherwise. The
/// password is given for each call and never kept: not in the source, not in the saved file.
#[test]
fn postgresql_tables_load_and_refresh_with_the_password_each_time() {
    let Ok(v) = std::env::var("GRIDKU_TEST_PG") else { return };
    let parts: Vec<&str> = v.splitn(4, ':').collect();
    let [host, port, user, password] = parts[..] else { panic!("GRIDKU_TEST_PG: host:port:user:password") };
    let conn = json!({"engine": "postgresql", "host": host, "port": port, "database": "toko", "user": user, "password": password});
    let with = |extra: Json| {
        let mut p = conn.clone();
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    let mut s = Session::new();
    s.new_workbook();
    let t = run(&mut s, "data.dbTables", conn.clone());
    assert_eq!(t["tables"][0], json!({"schema": "gudang", "name": "stok", "kind": "table"}));
    let c = run(&mut s, "data.dbColumns", with(json!({"table": "pelanggan"})));
    assert_eq!(c["columns"][1], json!({"name": "nama", "type": "character varying", "length": "60", "nullable": false, "primaryKey": 0}));
    let r = run(&mut s, "data.dbLoad", with(json!({"schema": "gudang", "table": "stok"})));
    assert_eq!((r["sheet"].as_str(), r["rows"].as_u64()), (Some("gudang.stok"), Some(2500)));
    let r = run(&mut s, "data.dbLoad", with(json!({"table": "pelanggan"})));
    assert_eq!(r["sheet"], "pelanggan", "public's tables are named alone");
    assert_eq!(cell(&s, "pelanggan", "C2"), Value::Number(1234.5));
    assert_eq!(cell(&s, "pelanggan", "D2"), Value::Bool(true));
    assert!(matches!(cell(&s, "pelanggan", "E2"), Value::Number(_)), "a date");
    assert_eq!(cell(&s, "pelanggan", "G2"), Value::text("9007199254740993"));
    assert_eq!(cell(&s, "pelanggan", "H2"), Value::text("[BLOB 2 bytes]"));
    let src = s.doc().unwrap().wb.db_sources.iter().find(|x| x.table == "pelanggan").cloned().unwrap();
    assert_eq!(src.location, format!("postgresql://{user}@{host}:{port}/toko?sslmode=prefer"));
    assert!(!src.location.contains(password));
    // Refresh asks for the password again.
    run(&mut s, "selection.set", json!({"range": "A2"}));
    assert!(s.execute("data.dbRefresh", json!({})).is_err(), "without the password");
    let r = run(&mut s, "data.dbRefresh", json!({"password": password}));
    assert_eq!(r["refreshed"], true);
    let bytes = gridcraft_xlsx::write_xlsx(&s.doc().unwrap().wb).unwrap();
    assert!(!String::from_utf8_lossy(&zip_text(&bytes, "customXml/item1.xml").into_bytes()).contains(password));
}

/// The rows of `orders` in the database: (customer, total, paid).
fn db_orders(db: &Db) -> Vec<(String, Option<f64>, Option<i64>)> {
    let conn = rusqlite::Connection::open(&db.0).unwrap();
    let mut st = conn.prepare("SELECT customer, total, paid FROM orders ORDER BY id").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
}

fn loaded(name: &str) -> (Db, Session) {
    let db = Db::new(name, SHOP);
    let mut s = Session::new();
    s.new_workbook();
    run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "orders"}));
    (db, s)
}

#[test]
fn changed_values_are_saved_back_and_read_back_and_undo_is_cleared() {
    let (db, mut s) = loaded("dbsave");
    run(&mut s, "cell.set", json!({"cell": "B2", "input": "Ani"}));
    run(&mut s, "cell.set", json!({"cell": "C3", "input": "8.5"}));
    run(&mut s, "cell.set", json!({"cell": "D3", "input": "TRUE"}));
    let r = run(&mut s, "data.dbSave", json!({}));
    assert_eq!((r["saved"].as_u64(), r["added"].as_u64(), r["removed"].as_u64()), (Some(2), Some(0), Some(0)));
    assert_eq!(db_orders(&db), [("Ani".into(), Some(12.5), Some(1)), ("Budi".into(), Some(8.5), Some(1))]);
    let d = s.doc().unwrap();
    assert!(d.undo.is_empty() && d.redo.is_empty(), "saving to a database can't be undone");
    // Nothing changed since: nothing to save. A change made after is compared with what was saved.
    assert_eq!(run(&mut s, "data.dbSave", json!({}))["saved"], 0);
    run(&mut s, "cell.set", json!({"cell": "B2", "input": "Ana"}));
    assert_eq!(run(&mut s, "data.dbSave", json!({}))["saved"], 1);
    assert_eq!(db_orders(&db)[0].0, "Ana");
    // A row added to the table (its key left empty) is counted, not written.
    let t = s.doc().unwrap().wb.sheet(1).unwrap().tables[0].name.clone();
    run(&mut s, "table.resize", json!({"table": t, "range": "A1:H4"}));
    run(&mut s, "cell.set", json!({"cell": "B4", "input": "Citra"}));
    let r = run(&mut s, "data.dbSave", json!({}));
    assert_eq!((r["saved"].as_u64(), r["added"].as_u64()), (Some(0), Some(1)));
    assert_eq!(db_orders(&db).len(), 2);
}

#[test]
fn the_header_row_and_key_columns_stay_as_loaded() {
    let (db, mut s) = loaded("dblocks");
    let refused = |s: &mut Session, id: &str, p: Json| {
        let e = s.execute(id, p.clone()).expect_err(&format!("{id} {p}")).to_string();
        assert!(e.contains("database"), "{e}");
    };
    refused(&mut s, "cell.set", json!({"cell": "B1", "input": "name"}));
    refused(&mut s, "cell.set", json!({"cell": "A2", "input": "5"}));
    refused(&mut s, "cell.set", json!({"cell": "A2", "input": "2"}));
    // A row added to the table keeps its key empty.
    let t = s.doc().unwrap().wb.sheet(1).unwrap().tables[0].name.clone();
    run(&mut s, "table.resize", json!({"table": t, "range": "A1:H4"}));
    refused(&mut s, "cell.set", json!({"cell": "A4", "input": "9"}));
    run(&mut s, "cell.set", json!({"cell": "B4", "input": "Citra"}));
    refused(&mut s, "edit.clearContents", json!({"range": "A2:A3"}));
    assert_eq!((cell(&s, "orders", "B1"), cell(&s, "orders", "A2")), (Value::text("customer"), Value::Number(1.0)));
    // Sorting moves each key with its row, and saving still finds the rows.
    run(&mut s, "selection.set", json!({"range": "B2"}));
    run(&mut s, "data.sortDescending", json!({}));
    assert_eq!(
        (cell(&s, "orders", "B2"), cell(&s, "orders", "A2"), cell(&s, "orders", "A3")),
        (Value::text("Citra"), Value::Empty, Value::Number(2.0))
    );
    run(&mut s, "cell.set", json!({"cell": "B3", "input": "Budiman"}));
    let r = run(&mut s, "data.dbSave", json!({}));
    assert_eq!((r["saved"].as_u64(), r["added"].as_u64()), (Some(1), Some(1)));
    assert_eq!(db_orders(&db)[1].0, "Budiman");
    // Other cells of the sheet are free, and a table without the database's lock is untouched.
    run(&mut s, "cell.set", json!({"cell": "K1", "input": "notes"}));
}

#[test]
fn each_column_gets_validation_from_its_type_and_saving_checks_every_value() {
    let (db, mut s) = loaded("dbcheck");
    let ok = |s: &mut Session, a1: &str, input: &str| run(s, "data.validate", json!({"cell": a1, "input": input}))["ok"].as_bool();
    assert_eq!(ok(&mut s, "B2", &"x".repeat(40)), Some(true));
    assert_eq!(ok(&mut s, "B2", &"x".repeat(41)), Some(false), "VARCHAR(40)");
    assert_eq!(ok(&mut s, "C2", "abc"), Some(false), "DECIMAL takes numbers");
    assert_eq!(ok(&mut s, "C2", "99999999"), Some(true));
    assert_eq!(ok(&mut s, "C2", "123456789"), Some(false), "DECIMAL(10,2): 8 digits before the point");
    assert_eq!(ok(&mut s, "D2", "maybe"), Some(false), "BOOLEAN");
    assert_eq!(ok(&mut s, "F2", "1.5"), Some(false), "INTEGER");
    assert_eq!(ok(&mut s, "G2", "anything"), Some(true), "TEXT without a length");
    // What validation doesn't see (a value set by a command, as a paste or a macro does) is
    // caught when saving, and nothing is written.
    run(&mut s, "cell.set", json!({"cell": "B2", "input": "x".repeat(41)}));
    run(&mut s, "cell.set", json!({"cell": "B3", "input": ""}));
    run(&mut s, "cell.set", json!({"cell": "C3", "input": "lots"}));
    run(&mut s, "cell.set", json!({"cell": "D2", "input": "Ani"}));
    let e = s.execute("data.dbSave", json!({})).unwrap_err().to_string();
    assert!(e.contains("Nothing was saved: 4 values"), "{e}");
    assert!(e.contains("B2: customer is VARCHAR(40): at most 40 characters"), "{e}");
    assert!(e.contains("B3: customer can't be empty"), "{e}");
    assert!(e.contains("C3: total is DECIMAL(10,2): it takes a number"), "{e}");
    assert!(e.contains("D2: paid is BOOLEAN"), "{e}");
    assert_eq!(db_orders(&db)[0].0, "Ana");
    // A BLOB can't be changed from a sheet.
    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "cell.set", json!({"cell": "H2", "input": "x"}));
    assert!(s.execute("data.dbSave", json!({})).unwrap_err().to_string().contains("binary data"));
}

#[test]
fn a_row_changed_in_the_database_since_it_was_loaded_stops_the_save() {
    let (db, mut s) = loaded("dbconflict");
    db.exec("UPDATE orders SET customer = 'Anna' WHERE id = 1;");
    run(&mut s, "cell.set", json!({"cell": "B2", "input": "Ani"}));
    run(&mut s, "cell.set", json!({"cell": "B3", "input": "Budiman"}));
    let e = s.execute("data.dbSave", json!({})).unwrap_err().to_string();
    assert!(e.contains("1 row was changed in the database") && e.contains("sheet row 2"), "{e}");
    assert_eq!(db_orders(&db).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["Anna", "Budi"], "nothing written");
    // Another column of the same row changed there is no conflict for this one.
    run(&mut s, "cell.set", json!({"cell": "B2", "input": "Ana"}));
    db.exec("UPDATE orders SET customer = 'Ana', note = 'x' WHERE id = 1;");
    run(&mut s, "cell.set", json!({"cell": "C2", "input": "13"}));
    assert_eq!(run(&mut s, "data.dbSave", json!({}))["saved"], 2);
    assert_eq!(cell(&s, "orders", "G2"), Value::text("x"), "the row reads back as the database has it");
}

#[test]
fn a_view_or_a_table_opened_from_a_file_saves_only_as_said() {
    let db = Db::new("dbview", SHOP);
    let mut s = Session::new();
    s.new_workbook();
    run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "paid_orders"}));
    assert!(s.execute("data.dbSave", json!({})).unwrap_err().to_string().contains("no primary key"));
    // Opened from a file, the rows as loaded aren't known: saving asks for `overwrite`.
    let dir = std::env::temp_dir().join(format!("gridku-dbreopen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let book = dir.join("b.xlsx").to_string_lossy().into_owned();
    let mut s = Session::new();
    s.new_workbook();
    run(&mut s, "data.dbLoad", json!({"path": db.path(), "table": "orders"}));
    run(&mut s, "file.saveAs", json!({"path": book}));
    let mut again = Session::new();
    run(&mut again, "file.open", json!({"path": book}));
    run(&mut again, "sheet.activate", json!({"sheet": "orders"}));
    run(&mut again, "cell.set", json!({"cell": "B3", "input": "Budiman"}));
    assert!(again.execute("data.dbSave", json!({})).unwrap_err().to_string().contains("overwrite"));
    assert_eq!(run(&mut again, "data.dbSave", json!({"overwrite": true}))["saved"], 1);
    assert_eq!(db_orders(&db)[1].0, "Budiman");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_and_columns_inserted_or_deleted_keep_the_rows_found() {
    let (db, mut s) = loaded("dbshift");
    // A column inserted before the table moves it; its header and keys go with it.
    run(&mut s, "home.insertColumns", json!({"range": "A1"}));
    assert_eq!(cell(&s, "orders", "C1"), Value::text("customer"));
    // A row deleted is taken out of the table, not out of the database.
    run(&mut s, "home.deleteRows", json!({"range": "A2"}));
    run(&mut s, "cell.set", json!({"cell": "C2", "input": "Budiman"}));
    let r = run(&mut s, "data.dbSave", json!({}));
    assert_eq!((r["saved"].as_u64(), r["removed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(db_orders(&db).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["Ana", "Budiman"]);
    // A column deleted from inside the table changes its header: refused.
    assert!(s.execute("home.deleteColumns", json!({"range": "C1"})).is_err());
}
