//! Data › DB Connect: the task pane that reads a database (`docs/databases.md`).
//!
//! The top half picks the engine (radio buttons: SQLite; PostgreSQL comes next) and the
//! database: a SQLite file's path, typed or chosen with Browse…. The bottom half lists the
//! database's tables and views (`data.dbTables`); a click selects one, **Load** puts it in a new
//! sheet as a table (`data.dbLoad`), and right-clicking one offers **Table Information…**, a
//! window with each field's name, type and length (`data.dbColumns`). **Refresh** reads again the
//! table at the active cell (`data.dbRefresh`).

use egui::{RichText, Ui};
use serde_json::{Value as Json, json};

use crate::SheetApp;
use crate::theme::Tokens;

/// What the pane remembers between frames (egui's memory, by the pane's id).
#[derive(Clone, Debug, Default)]
struct PaneState {
    engine: String,
    path: String,
    /// The path the list below was read from.
    listed: String,
    tables: Vec<(String, String)>,
    selected: Option<String>,
    error: Option<String>,
    /// Table Information: the table and its columns, while the window is open.
    info: Option<(String, Vec<Json>)>,
}

fn state_id() -> egui::Id {
    egui::Id::unique("db_connect_pane")
}

/// Reads the database's tables and views into the list.
fn list(app: &mut SheetApp, st: &mut PaneState) {
    st.listed = st.path.trim().to_string();
    st.selected = None;
    st.tables.clear();
    st.error = None;
    if st.listed.is_empty() {
        return;
    }
    match app.engine("data.dbTables", json!({"engine": st.engine, "path": st.listed})) {
        Ok(r) => {
            st.tables = r["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| (t["name"].as_str().unwrap_or("").to_string(), t["kind"].as_str().unwrap_or("table").to_string()))
                .collect();
        }
        Err(e) => st.error = Some(crate::clean_error(&e)),
    }
}

/// The sheet table at the active cell, when it was loaded from a database.
fn loaded_table_here(app: &SheetApp) -> Option<String> {
    let d = app.session.active()?;
    let sh = d.wb.active()?;
    let t = sh.table_at(d.selection.active)?;
    d.wb.db_sources.iter().any(|s| s.table_id == t.id).then(|| t.name.clone())
}

/// What a load or refresh did, in a line.
fn loaded_message(r: &Json) -> String {
    let rows = r["rows"].as_u64().unwrap_or(0);
    let what = if r["refreshed"].as_bool() == Some(true) { "Refreshed" } else { "Loaded" };
    let mut text = format!("{what} {} ({rows} {})", r["sheet"].as_str().unwrap_or(""), if rows == 1 { "row" } else { "rows" });
    if r["truncated"].as_bool() == Some(true) {
        text.push_str(": only the first rows fit on a sheet");
    }
    if r["cutTexts"].as_u64().unwrap_or(0) > 0 {
        text.push_str("; texts longer than a cell holds were cut");
    }
    text
}

pub(crate) fn pane(app: &mut SheetApp, ui: &mut Ui) {
    let mut st: PaneState = ui.data_mut(|d| d.get_temp(state_id())).unwrap_or_default();
    if st.engine.is_empty() {
        st.engine = "sqlite".into();
    }
    let t = Tokens::get(ui.ctx());

    // Top: the engine and the database.
    ui.label(RichText::new("Database engine").strong());
    ui.horizontal(|ui| {
        ui.radio_value(&mut st.engine, "sqlite".to_string(), "SQLite");
        ui.add_enabled(false, egui::RadioButton::new(st.engine == "postgresql", "PostgreSQL")).on_disabled_hover_text("Coming in the next update");
    });
    ui.add_space(6.0);
    let label = ui.label(RichText::new("Database file").strong());
    let mut read = false;
    // The path, with Browse… to its right: laid out right to left so the field takes the width left.
    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let browse = app.services.pick_database.is_some() && ui.button("Browse…").clicked();
        let field = egui::TextEdit::singleline(&mut st.path).hint_text("Path to a .db or .sqlite file").desired_width(ui.available_width());
        let r = ui.add(field).labelled_by(label.id);
        read |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if browse && let Some(path) = app.services.pick_database.as_ref().and_then(|pick| pick()) {
            st.path = path;
            read = true;
        }
    });
    ui.horizontal(|ui| {
        read |= ui.add_enabled(!st.path.trim().is_empty(), egui::Button::new("Connect")).clicked();
        if !st.listed.is_empty() && ui.button("Reload List").on_hover_text("Reads the database's tables again").clicked() {
            read = true;
        }
    });
    if read {
        list(app, &mut st);
    }
    if let Some(e) = &st.error {
        ui.label(RichText::new(e).color(t.danger));
    }
    ui.add_space(6.0);
    ui.separator();

    // Bottom: the tables and views.
    ui.label(RichText::new("Tables").strong());
    if st.listed.is_empty() {
        ui.label(RichText::new("Choose a database file to see its tables.").italics().color(t.text_dim));
    } else if st.tables.is_empty() && st.error.is_none() {
        ui.label(RichText::new("This database has no tables.").italics().color(t.text_dim));
    }
    let mut info_for: Option<String> = None;
    let mut load_now = false;
    for (kind, heading) in [("table", "Tables"), ("view", "Views")] {
        let names: Vec<String> = st.tables.iter().filter(|(_, k)| k == kind).map(|(n, _)| n.clone()).collect();
        if names.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(format!("{heading} ({})", names.len())).id_salt(("db_tree", kind)).default_open(true).show(ui, |ui| {
            for name in names {
                let selected = st.selected.as_deref() == Some(name.as_str());
                let r = ui.selectable_label(selected, &name);
                if r.clicked() {
                    st.selected = Some(name.clone());
                }
                r.context_menu(|ui| {
                    if ui.button("Table Information…").clicked() {
                        info_for = Some(name.clone());
                        ui.close();
                    }
                    if ui.button("Load").clicked() {
                        st.selected = Some(name.clone());
                        load_now = true;
                        ui.close();
                    }
                });
            }
        });
    }
    if let Some(table) = info_for {
        match app.engine("data.dbColumns", json!({"engine": st.engine, "path": st.listed, "table": table})) {
            Ok(r) => st.info = Some((table, r["columns"].as_array().cloned().unwrap_or_default())),
            Err(e) => app.message = Some(("Table Information".into(), crate::clean_error(&e))),
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let load =
            ui.add_enabled(st.selected.is_some(), egui::Button::new("Load")).on_hover_text("Puts the selected table in a new sheet, as a table");
        if (load.clicked() || load_now)
            && let Some(table) = st.selected.clone()
        {
            match app.run("data.dbLoad", json!({"engine": st.engine, "path": st.listed, "table": table})) {
                Ok(r) => app.toast = Some((loaded_message(&r), crate::now_ms())),
                Err(e) => app.message = Some(("DB Connect".into(), crate::clean_error(&e))),
            }
        }
        let here = loaded_table_here(app);
        let refresh = ui
            .add_enabled(here.is_some(), egui::Button::new("Refresh"))
            .on_hover_text("Reads again the database table of the table at the active cell")
            .on_disabled_hover_text("Select a cell in a table loaded from a database");
        if refresh.clicked() {
            match app.run("data.dbRefresh", json!({"table": here})) {
                Ok(r) => app.toast = Some((loaded_message(&r), crate::now_ms())),
                Err(e) => app.message = Some(("DB Connect".into(), crate::clean_error(&e))),
            }
        }
    });
    table_info(ui, &mut st);
    ui.data_mut(|d| d.insert_temp(state_id(), st));
}

/// The Table Information window: each field's name, type and length, whether it may be empty,
/// and its place in the primary key.
fn table_info(ui: &mut Ui, st: &mut PaneState) {
    let Some((table, cols)) = st.info.clone() else { return };
    let mut open = true;
    egui::Window::new(format!("Table Information: {table}"))
        .id(egui::Id::unique("db_table_info"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(460.0)
        .min_width(360.0)
        .show(ui.ctx(), |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                egui::Grid::new("db_table_info_grid").striped(true).num_columns(5).spacing([16.0, 4.0]).show(ui, |ui| {
                    for h in ["Field", "Type", "Length", "Null", "Key"] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for c in &cols {
                        ui.label(c["name"].as_str().unwrap_or(""));
                        ui.label(c["type"].as_str().filter(|s| !s.is_empty()).unwrap_or("—"));
                        ui.label(c["length"].as_str().unwrap_or("—"));
                        ui.label(if c["nullable"].as_bool() == Some(false) { "NOT NULL" } else { "" });
                        let pk = c["primaryKey"].as_u64().unwrap_or(0);
                        ui.label(if pk == 0 { String::new() } else { format!("PK {pk}") });
                        ui.end_row();
                    }
                });
            });
        });
    if !open {
        st.info = None;
    }
}
