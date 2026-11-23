//! Data › DB Connect: the task pane that reads a database (`docs/databases.md`).
//!
//! The top half picks the engine (radio buttons: SQLite or PostgreSQL) and the database: a
//! SQLite file's path, typed or chosen with Browse…; or a PostgreSQL server's host, port,
//! database, user, password and SSL. The bottom half lists the database's tables and views
//! (`data.dbTables`), PostgreSQL's schema by schema; a click selects one, **Load** puts it in a
//! new sheet as a table (`data.dbLoad`), and right-clicking one offers **Table Information…**, a
//! window with each field's name, type and length (`data.dbColumns`). **Refresh** reads again the
//! table at the active cell (`data.dbRefresh`). A large table loads in the background: the busy
//! box shows the rows read, then written, as a progress bar, and Cancel stops it.
//!
//! A PostgreSQL password is held only in this pane's memory while it is connected (egui's
//! temporary memory, never saved), and asked for again on the next Connect; a table loaded
//! earlier, or in a workbook opened from a file, asks for it when it is refreshed.

use egui::{RichText, Ui};
use serde_json::{Value as Json, json};

use crate::SheetApp;
use crate::theme::Tokens;

/// One table or view of the list.
#[derive(Clone, Debug, PartialEq)]
struct TableRef {
    schema: Option<String>,
    name: String,
    kind: String,
}

impl TableRef {
    /// The parameters that name it, for the commands.
    fn params(&self, conn: &Json) -> Json {
        let mut p = conn.clone();
        p["table"] = json!(self.name);
        if let Some(schema) = &self.schema {
            p["schema"] = json!(schema);
        }
        p
    }
}

/// What the pane remembers between frames (egui's memory, by the pane's id; never saved).
#[derive(Clone, Debug)]
struct PaneState {
    engine: String,
    path: String,
    host: String,
    port: String,
    database: String,
    user: String,
    password: String,
    ssl: String,
    /// The parameters the list below was read with (a PostgreSQL password included), while
    /// connected.
    connected: Option<Json>,
    tables: Vec<TableRef>,
    selected: Option<TableRef>,
    error: Option<String>,
    /// Table Information: the table and its columns, while the window is open.
    info: Option<(String, Vec<Json>)>,
}

impl Default for PaneState {
    fn default() -> Self {
        PaneState {
            engine: "sqlite".into(),
            path: String::new(),
            host: "localhost".into(),
            port: "5432".into(),
            database: String::new(),
            user: String::new(),
            password: String::new(),
            ssl: "prefer".into(),
            connected: None,
            tables: Vec::new(),
            selected: None,
            error: None,
            info: None,
        }
    }
}

impl PaneState {
    /// The connection the fields describe, as the commands take it.
    fn conn(&self) -> Json {
        match self.engine.as_str() {
            "postgresql" => json!({
                "engine": "postgresql",
                "host": self.host.trim(),
                "port": self.port.trim(),
                "database": self.database.trim(),
                "user": self.user.trim(),
                "password": self.password,
                "ssl": self.ssl,
            }),
            _ => json!({"engine": "sqlite", "path": self.path.trim()}),
        }
    }

    fn can_connect(&self) -> bool {
        match self.engine.as_str() {
            "postgresql" => [&self.host, &self.database, &self.user].iter().all(|f| !f.trim().is_empty()),
            _ => !self.path.trim().is_empty(),
        }
    }

    /// The connection URL of the PostgreSQL server connected to (as a loaded table's source
    /// keeps it), to tell whether a table to refresh came from it.
    fn connected_url(&self) -> Option<String> {
        let c = self.connected.as_ref()?;
        if c["engine"] != "postgresql" {
            return None;
        }
        let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
        let cfg = gridcraft_db::PgConfig {
            host: s("host"),
            port: s("port").parse().unwrap_or(5432),
            database: s("database"),
            user: s("user"),
            password: String::new(),
            ssl: gridcraft_db::SslMode::parse(&s("ssl")).unwrap_or_default(),
        };
        Some(cfg.url())
    }
}

fn state_id() -> egui::Id {
    egui::Id::unique("db_connect_pane")
}

/// Reads the database's tables and views into the list.
fn list(app: &mut SheetApp, st: &mut PaneState) {
    st.selected = None;
    st.tables.clear();
    st.error = None;
    st.connected = None;
    if !st.can_connect() {
        return;
    }
    let conn = st.conn();
    match app.engine("data.dbTables", conn.clone()) {
        Ok(r) => {
            st.tables = r["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| TableRef {
                    schema: t["schema"].as_str().map(str::to_string),
                    name: t["name"].as_str().unwrap_or("").to_string(),
                    kind: t["kind"].as_str().unwrap_or("table").to_string(),
                })
                .collect();
            st.connected = Some(conn);
        }
        Err(e) => st.error = Some(crate::clean_error(&e)),
    }
}

/// The sheet table at the active cell, when it was loaded from a database: its name, and the
/// PostgreSQL connection it came from (`None` for SQLite).
fn loaded_table_here(app: &SheetApp) -> Option<(String, Option<String>)> {
    let d = app.session.active()?;
    let sh = d.wb.active()?;
    let t = sh.table_at(d.selection.active)?;
    let src = d.wb.db_sources.iter().find(|s| s.table_id == t.id)?;
    let pg = (src.engine == gridcraft_engine::model::DbEngine::Postgres).then(|| src.location.clone());
    Some((t.name.clone(), pg))
}

/// Runs Load or Refresh. A large table goes on in the background with a progress bar and Cancel
/// (`background.rs`); what it did is said when it is over.
pub(crate) fn run_db(app: &mut SheetApp, id: &str, params: Json) {
    match app.run_detached(id, params, crate::background::After::Report(loaded_message)) {
        Some(Ok(r)) => app.toast = Some((loaded_message(&r), crate::now_ms())),
        Some(Err(e)) if e == gridcraft_engine::EngineError::Cancelled.to_string() => {}
        Some(Err(e)) => app.message = Some(("DB Connect".into(), crate::clean_error(&e))),
        None => {}
    }
}

/// What a load or refresh did, in a line.
pub(crate) fn loaded_message(r: &Json) -> String {
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

/// A labelled text field the width of the pane.
fn field(ui: &mut Ui, label: &str, value: &mut String, password: bool) -> egui::Response {
    let l = ui.label(label);
    ui.add(egui::TextEdit::singleline(value).password(password).desired_width(ui.available_width() - 6.0)).labelled_by(l.id)
}

/// The SQLite file: its path, with Browse… to its right.
fn sqlite_fields(app: &SheetApp, ui: &mut Ui, st: &mut PaneState) -> bool {
    let label = ui.label(RichText::new("Database file").strong());
    let mut read = false;
    // Laid out right to left so the field takes the width Browse… leaves.
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
    read
}

/// The PostgreSQL server: host and port, database, user, password and SSL.
fn postgres_fields(ui: &mut Ui, st: &mut PaneState) -> bool {
    let mut enter = false;
    ui.columns(2, |cols| {
        if let [left, right] = cols {
            left.vertical(|ui| enter |= field(ui, "Host", &mut st.host, false).lost_focus());
            right.vertical(|ui| enter |= field(ui, "Port", &mut st.port, false).lost_focus());
        }
    });
    enter |= field(ui, "Database", &mut st.database, false).lost_focus();
    enter |= field(ui, "User", &mut st.user, false).lost_focus();
    enter |= field(ui, "Password", &mut st.password, true).on_hover_text("Asked for each time you connect; never saved").lost_focus();
    ui.horizontal(|ui| {
        ui.label("SSL");
        craft_ui::ComboBox::from_id_salt("db_ssl")
            .selected_text(match st.ssl.as_str() {
                "disable" => "Off",
                "require" => "Required",
                _ => "When offered",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut st.ssl, "prefer".to_string(), "When offered");
                ui.selectable_value(&mut st.ssl, "require".to_string(), "Required");
                ui.selectable_value(&mut st.ssl, "disable".to_string(), "Off");
            })
            .response
            .on_hover_text("With SSL the server's certificate is checked against this computer's certificate authorities");
    });
    enter && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// One table or view of the tree; right-click offers Table Information… and Load.
fn table_row(ui: &mut Ui, st: &mut PaneState, t: &TableRef, info_for: &mut Option<TableRef>, load_now: &mut bool) {
    let selected = st.selected.as_ref() == Some(t);
    let r = ui.selectable_label(selected, &t.name);
    if r.clicked() {
        st.selected = Some(t.clone());
    }
    r.context_menu(|ui| {
        if ui.button("Table Information…").clicked() {
            *info_for = Some(t.clone());
            ui.close();
        }
        if ui.button("Load").clicked() {
            st.selected = Some(t.clone());
            *load_now = true;
            ui.close();
        }
    });
}

/// Tables, then Views, of one schema (or of a SQLite file).
fn tables_and_views(ui: &mut Ui, st: &mut PaneState, schema: Option<&str>, info_for: &mut Option<TableRef>, load_now: &mut bool) {
    for (kind, heading) in [("table", "Tables"), ("view", "Views")] {
        let items: Vec<TableRef> = st.tables.iter().filter(|t| t.kind == kind && t.schema.as_deref() == schema).cloned().collect();
        if items.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(format!("{heading} ({})", items.len())).id_salt(("db_tree", schema, kind)).default_open(true).show(ui, |ui| {
            for t in &items {
                table_row(ui, st, t, info_for, load_now);
            }
        });
    }
}

pub(crate) fn pane(app: &mut SheetApp, ui: &mut Ui) {
    let mut st: PaneState = ui.data_mut(|d| d.get_temp(state_id())).unwrap_or_default();
    let t = Tokens::get(ui.ctx());

    // Top: the engine and the database.
    ui.label(RichText::new("Database engine").strong());
    let before = st.engine.clone();
    ui.horizontal(|ui| {
        ui.radio_value(&mut st.engine, "sqlite".to_string(), "SQLite");
        ui.radio_value(&mut st.engine, "postgresql".to_string(), "PostgreSQL");
    });
    if st.engine != before {
        // Another engine: the list was another database's.
        st.connected = None;
        st.tables.clear();
        st.selected = None;
        st.error = None;
    }
    ui.add_space(6.0);
    let mut read = if st.engine == "postgresql" { postgres_fields(ui, &mut st) } else { sqlite_fields(app, ui, &mut st) };
    ui.horizontal(|ui| {
        read |= ui.add_enabled(st.can_connect(), egui::Button::new("Connect")).clicked();
        if st.connected.is_some() && ui.button("Reload List").on_hover_text("Reads the database's tables again").clicked() {
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
    tree_and_actions(app, ui, &mut st, &t);
    table_info(ui, &mut st);
    ui.data_mut(|d| d.insert_temp(state_id(), st));
}

/// Bottom: the tables and views (PostgreSQL's schema by schema), then Load and Refresh.
fn tree_and_actions(app: &mut SheetApp, ui: &mut Ui, st: &mut PaneState, t: &Tokens) {
    ui.label(RichText::new("Tables").strong());
    if st.connected.is_none() && st.error.is_none() {
        let hint = if st.engine == "postgresql" { "Connect to a server to see its tables." } else { "Choose a database file to see its tables." };
        ui.label(RichText::new(hint).italics().color(t.text_dim));
    } else if st.connected.is_some() && st.tables.is_empty() {
        ui.label(RichText::new("This database has no tables.").italics().color(t.text_dim));
    }
    let mut info_for: Option<TableRef> = None;
    let mut load_now = false;
    let mut schemas: Vec<Option<String>> = Vec::new();
    for t in &st.tables {
        if !schemas.contains(&t.schema) {
            schemas.push(t.schema.clone());
        }
    }
    for schema in schemas {
        match schema {
            None => tables_and_views(ui, st, None, &mut info_for, &mut load_now),
            Some(name) => {
                egui::CollapsingHeader::new(RichText::new(&name).strong()).id_salt(("db_schema", &name)).default_open(name == "public").show(
                    ui,
                    |ui| {
                        tables_and_views(ui, st, Some(&name), &mut info_for, &mut load_now);
                    },
                );
            }
        }
    }
    if let (Some(t), Some(conn)) = (info_for, st.connected.clone()) {
        match app.engine("data.dbColumns", t.params(&conn)) {
            Ok(r) => {
                let title = t.schema.as_ref().filter(|s| *s != "public").map_or_else(|| t.name.clone(), |s| format!("{s}.{}", t.name));
                st.info = Some((title, r["columns"].as_array().cloned().unwrap_or_default()));
            }
            Err(e) => app.message = Some(("Table Information".into(), crate::clean_error(&e))),
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let load =
            ui.add_enabled(st.selected.is_some(), egui::Button::new("Load")).on_hover_text("Puts the selected table in a new sheet, as a table");
        if (load.clicked() || load_now)
            && let (Some(table), Some(conn)) = (st.selected.clone(), st.connected.clone())
        {
            run_db(app, "data.dbLoad", table.params(&conn));
        }
        let here = loaded_table_here(app);
        let refresh = ui
            .add_enabled(here.is_some(), egui::Button::new("Refresh"))
            .on_hover_text("Reads again the database table of the table at the active cell")
            .on_disabled_hover_text("Select a cell in a table loaded from a database");
        if refresh.clicked()
            && let Some((table, pg)) = here
        {
            match pg {
                None => run_db(app, "data.dbRefresh", json!({"table": table})),
                // Connected to that server now: its password; else it is asked for.
                Some(url) if st.connected_url().as_deref() == Some(url.as_str()) => {
                    let password = st.connected.as_ref().and_then(|c| c["password"].as_str()).unwrap_or("").to_string();
                    run_db(app, "data.dbRefresh", json!({"table": table, "password": password}));
                }
                Some(url) => app.open_dialog("dbPassword", json!({"table": table, "url": url})),
            }
        }
    });
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
