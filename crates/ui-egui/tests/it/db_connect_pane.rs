//! Data › DB Connect's pane: Connect and Table Information run off the interface thread, and
//! their answers show when they come (`db_pane.rs`).

use std::time::Duration;

use crate::common::Harness;
use egui_kittest::kittest::Queryable;

fn database(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("gridku-pane-{name}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE orders (id INTEGER PRIMARY KEY, customer VARCHAR(40) NOT NULL); INSERT INTO orders VALUES (1, 'Ana');")
        .unwrap();
    path
}

/// Frames until `label` shows, a while at most.
fn until_shown(h: &mut Harness, label: &str) -> bool {
    for _ in 0..500 {
        h.run_steps(1);
        if h.query_by_label(label).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

#[test]
fn connect_and_table_information_answer_off_the_interface_thread() {
    let db = database("connect");
    let mut h = crate::support::blank_harness(4);
    h.state_mut().grid.pane = Some("dbConnect".into());
    h.run_steps(2);
    h.get_by_label("Database file").focus();
    h.run_steps(1);
    h.get_by_label("Database file").type_text(&db.to_string_lossy());
    h.run_steps(1);
    h.get_by_label("Connect").click();
    assert!(until_shown(&mut h, "orders"), "the list comes when the file has been read");
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    h.get_by_label("Table Information…").click();
    assert!(until_shown(&mut h, "Table Information: orders"), "the window opens when the fields have been read");
    assert!(h.query_by_label("VARCHAR").is_some() && h.query_by_label("NOT NULL").is_some());
    // Load still runs as a command, the table in a new sheet.
    h.get_by_label("orders").click();
    h.run_steps(1);
    h.get_all_by_label("Load").next().unwrap().click();
    h.run_steps(3);
    assert_eq!(h.state().session.active().unwrap().wb.sheets.len(), 2);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_file_that_is_not_a_database_says_so_and_the_pane_stays_usable() {
    let path = std::env::temp_dir().join(format!("gridku-pane-text-{}.db", std::process::id()));
    std::fs::write(&path, "not a database, just text").unwrap();
    let mut h = crate::support::blank_harness(4);
    h.state_mut().grid.pane = Some("dbConnect".into());
    h.run_steps(2);
    h.get_by_label("Database file").focus();
    h.run_steps(1);
    h.get_by_label("Database file").type_text(&path.to_string_lossy());
    h.run_steps(1);
    h.get_by_label("Connect").click();
    let mut said = false;
    for _ in 0..500 {
        h.run_steps(1);
        if h.query_all_by_label_contains("not a database").next().is_some() || h.query_all_by_label_contains("file is not").next().is_some() {
            said = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(said, "the error shows in the pane");
    assert!(h.query_by_label("Connect").is_some());
    let _ = std::fs::remove_file(&path);
}

fn customer(db: &std::path::Path) -> String {
    rusqlite::Connection::open(db).unwrap().query_row("SELECT customer FROM orders WHERE id = 1", [], |r| r.get(0)).unwrap()
}

#[test]
fn save_to_database_writes_the_changed_values_back() {
    let db = database("save");
    let mut h = crate::support::blank_harness(4);
    h.state_mut().grid.pane = Some("dbConnect".into());
    h.run_steps(2);
    h.get_by_label("Database file").focus();
    h.run_steps(1);
    h.get_by_label("Database file").type_text(&db.to_string_lossy());
    h.run_steps(1);
    h.get_by_label("Connect").click();
    assert!(until_shown(&mut h, "orders"));
    h.get_by_label("orders").click();
    h.run_steps(1);
    h.get_all_by_label("Load").next().unwrap().click();
    h.run_steps(3);
    h.state_mut().run("cell.set", serde_json::json!({"cell": "B2", "input": "Ani"})).unwrap();
    h.state_mut().run("selection.set", serde_json::json!({"range": "B2"})).unwrap();
    h.run_steps(2);
    h.get_by_label("Save to Database").click();
    let mut saved = false;
    for _ in 0..500 {
        h.run_steps(1);
        if customer(&db) == "Ani" {
            saved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(saved, "the database has the new value");
    // The header row stays: typing over it is refused.
    assert!(h.state_mut().run("cell.set", serde_json::json!({"cell": "B1", "input": "name"})).is_err());
    let wb = &h.state().session.active().unwrap().wb;
    assert_eq!(wb.active().unwrap().value(gridcraft_engine::core::CellRef::new(0, 1)).display(), "customer");
    let _ = std::fs::remove_file(&db);
}

#[test]
fn save_asks_for_the_password_or_to_overwrite_when_it_must() {
    let mut h = crate::support::blank_harness(4);
    h.state_mut().open_dialog(
        "dbSave",
        serde_json::json!({"table": "Table1", "url": "postgresql://andri@db.lan:5432/toko?sslmode=prefer", "password": null, "overwrite": true}),
    );
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("loaded before the workbook was opened").next().is_some());
    assert!(h.query_all_by_label_contains("The password of andri@db.lan:5432/toko").next().is_some());
    assert!(h.query_by_label("Password:").is_some());
}
