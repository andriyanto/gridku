//! Data › Edit Links: the other workbooks this one's formulas read (`data.editLinks`), with
//! Update Values, Open Source, Change Source and Break Link for the one selected, where it is
//! used (`data.linkUses`: cells, defined names, conditional formats, data validation, charts,
//! sparklines, PivotTables) with Find Next to go to each place, and Break All.

use super::{Dialog, Json, SheetApp, json};

fn text(link: &Json, key: &str) -> String {
    link.get(key).and_then(Json::as_str).unwrap_or("").to_string()
}

/// What the Status column says of a link.
fn status(link: &Json) -> &'static str {
    match link.get("status").and_then(Json::as_str) {
        Some("open") => "Open: values follow it",
        Some("saved") => "Not open: values from the file",
        _ => "Not open: nothing known of it",
    }
}

/// The places that read `link`, from `data.linkUses`; asked again only when the workbook or
/// the selected link changes, not every frame.
fn uses(app: &mut SheetApp, d: &mut Dialog, link: &str) -> Json {
    let rev = app.session.active().map_or(0, |doc| doc.revision);
    let key = format!("{rev}\u{1}{link}");
    if d.values.get("usesKey").and_then(Json::as_str) != Some(key.as_str()) {
        let found = app.engine("data.linkUses", json!({"link": link})).unwrap_or(Json::Null);
        d.values.insert("uses".into(), found);
        d.values.insert("usesKey".into(), json!(key));
        d.values.remove("findAt");
        d.values.remove("found");
    }
    d.values.get("uses").cloned().unwrap_or(Json::Null)
}

/// "Used in": the counts of the places that read the link, or that nothing does.
fn used_in(uses: &Json) -> String {
    let c = &uses["counts"];
    let n = |k: &str| c.get(k).and_then(Json::as_u64).unwrap_or(0);
    let parts: Vec<String> = [
        ("cells", "cell", "cells"),
        ("names", "defined name", "defined names"),
        ("conditionalFormats", "conditional format", "conditional formats"),
        ("validations", "data validation rule", "data validation rules"),
        ("charts", "chart", "charts"),
        ("sparklines", "sparkline", "sparklines"),
        ("pivots", "PivotTable", "PivotTables"),
    ]
    .into_iter()
    .filter(|(k, ..)| n(k) > 0)
    .map(|(k, one, many)| format!("{} {}", n(k), if n(k) == 1 { one } else { many }))
    .collect();
    if parts.is_empty() { "Used nowhere: Break Link removes it".to_string() } else { format!("Used in {}", parts.join(", ")) }
}

/// What a place is, in words (`Sheet1!B4`, `Defined name Rate`…).
fn describe(place: &Json) -> String {
    let t = |k: &str| place.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let at = format!("{}!{}", t("sheet"), t("range"));
    let hidden = if place.get("hidden").and_then(Json::as_bool) == Some(true) { " (hidden)" } else { "" };
    match t("kind").as_str() {
        "cell" => format!("Cell {at}{hidden}"),
        "name" => format!("Defined name {}{hidden}: change or delete it in Formulas › Name Manager", t("name")),
        "conditionalFormat" => format!("Conditional formatting rule on {at}{hidden}: Home › Conditional Formatting › Manage Rules"),
        "validation" => format!("Data validation on {at}{hidden}: Data › Data Validation"),
        "chart" => format!("Chart on {}{hidden}: Chart Design › Select Data", t("sheet")),
        "sparkline" => format!("Sparkline in {at}{hidden}"),
        "pivot" => format!("PivotTable {} on {at}{hidden}: its data source", t("name")),
        _ => String::new(),
    }
}

/// Find Next: goes to the place after the last one found, from the start after the last.
fn find_next(app: &mut SheetApp, d: &mut Dialog, uses: &Json) {
    let places = uses.get("places").and_then(Json::as_array).cloned().unwrap_or_default();
    if places.is_empty() {
        return;
    }
    let at = d.values.get("findAt").and_then(Json::as_u64).map_or(0, |i| (i as usize + 1) % places.len());
    d.values.insert("findAt".into(), json!(at));
    let Some(place) = places.get(at) else { return };
    d.values.insert("found".into(), json!(format!("{} of {}: {}", at + 1, places.len(), describe(place))));
    // A hidden sheet is not shown for this: the line says where the place is.
    if place.get("hidden").and_then(Json::as_bool) == Some(true) {
        return;
    }
    let sheet = place.get("sheet").cloned().unwrap_or(Json::Null);
    match place.get("kind").and_then(Json::as_str) {
        Some("cell" | "conditionalFormat" | "validation" | "sparkline" | "pivot") => {
            if let Err(e) = app.run("selection.set", json!({"sheet": sheet, "range": place["range"]})) {
                app.message = Some(("Edit Links".into(), crate::clean_error(&e)));
            }
        }
        Some("chart") => {
            let here = app.session.active().and_then(|doc| doc.wb.active()).map(|sh| sh.name.clone());
            if here.as_deref() != sheet.as_str()
                && let Err(e) = app.run("sheet.activate", json!({"sheet": sheet}))
            {
                app.message = Some(("Edit Links".into(), crate::clean_error(&e)));
            }
            app.select_object_id(place.get("id").and_then(Json::as_u64).and_then(|id| u32::try_from(id).ok()));
        }
        _ => {}
    }
}

/// After Break Link or Break All: what still reads the workbook, so the link stayed.
fn kept_message(r: &Json) -> Option<String> {
    let k = &r["kept"];
    let n = |key: &str| k.get(key).and_then(Json::as_u64).unwrap_or(0);
    let names: Vec<&str> = r.get("names").and_then(Json::as_array).map(|v| v.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
    let mut parts = Vec::new();
    if !names.is_empty() {
        parts.push(format!("defined names ({})", names.join(", ")));
    }
    for (key, what) in [
        ("conditionalFormats", "conditional formatting rules"),
        ("validations", "data validation rules"),
        ("charts", "charts"),
        ("sparklines", "sparklines"),
        ("pivots", "PivotTables"),
    ] {
        if n(key) > 0 {
            parts.push(format!("{what} ({})", n(key)));
        }
    }
    (!parts.is_empty()).then(|| {
        format!(
            "The formulas in cells became their values. The link stays, as these still read the workbook: {}. Use Find Next to go to each and change it; Break Link then removes the link.",
            parts.join("; ")
        )
    })
}

pub(super) fn edit_links(app: &mut SheetApp, ui: &mut egui::Ui, d: &mut Dialog, open: &mut bool) {
    let list = app.engine("data.editLinks", json!({})).unwrap_or(Json::Null);
    let links = list.as_array().cloned().unwrap_or_default();
    d.list_index = d.list_index.min(links.len().saturating_sub(1));
    let selected = links.get(d.list_index).cloned();
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        egui::Grid::new("links").striped(true).num_columns(4).show(ui, |ui| {
            ui.strong("Source");
            ui.strong("Status");
            ui.strong("Read by");
            ui.strong("Where");
            ui.end_row();
            if links.is_empty() {
                ui.label(egui::RichText::new("No formula of this workbook reads another workbook").italics());
                ui.end_row();
            }
            for (i, link) in links.iter().enumerate() {
                if ui.selectable_label(i == d.list_index, text(link, "name")).clicked() {
                    d.list_index = i;
                }
                ui.label(status(link));
                let reads = link.get("reads").and_then(Json::as_u64).unwrap_or(0);
                ui.label(if reads == 1 { "1 reference".to_string() } else { format!("{reads} references") });
                // Where the file is, or would be: the path as text. It is not looked for.
                let place = Some(text(link, "path")).filter(|p| !p.is_empty()).unwrap_or_else(|| text(link, "target"));
                ui.add(egui::Label::new(if place.is_empty() { "—".to_string() } else { place }).truncate());
                ui.end_row();
            }
        });
    });
    // Where the selected link is used, and Find Next to go there.
    if let Some(link) = &selected {
        let found = uses(app, d, &text(link, "name"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(used_in(&found));
            let any = found.get("places").and_then(Json::as_array).is_some_and(|p| !p.is_empty());
            if ui
                .add_enabled(any, egui::Button::new("Find Next"))
                .on_hover_text("Goes to the next place that reads this workbook: a cell, a defined name, a conditional format, data validation, a chart, a sparkline or a PivotTable.")
                .clicked()
            {
                find_next(app, d, &found);
            }
        });
        if let Some(line) = d.values.get("found").and_then(Json::as_str) {
            ui.label(egui::RichText::new(line).italics());
        }
    }
    ui.add_space(8.0);
    let label = ui.label("Change the source to (a file, or the name of an open workbook):");
    let mut source = d.values.get("source").and_then(Json::as_str).unwrap_or("").to_string();
    let mut change = false;
    ui.horizontal(|ui| {
        if ui.add(egui::TextEdit::singleline(&mut source).desired_width(300.0)).labelled_by(label.id).changed() {
            d.values.insert("source".into(), json!(source));
        }
        if let Some(pick) = &app.services.pick_open
            && ui.add_enabled(selected.is_some(), egui::Button::new("Browse…")).clicked()
            && let Some(path) = pick()
        {
            source = path;
            d.values.insert("source".into(), json!(source));
            change = true;
        }
        change |= ui.add_enabled(selected.is_some() && !source.trim().is_empty(), egui::Button::new("Change Source")).clicked();
    });
    if change && let Some(link) = &selected {
        let source = source.trim();
        // An open workbook is named as its window names it; anything else is a file's path.
        let open_book = app.session.documents().iter().any(|doc| gridcraft_engine::formula::same_name(&doc.display_title(), source));
        let key = if open_book { "name" } else { "path" };
        match app.run("data.changeLinkSource", json!({"link": text(link, "name"), key: source})) {
            Ok(r) => {
                d.values.remove("source");
                if r.get("read").and_then(Json::as_bool) == Some(false) {
                    let why = text(&r, "message");
                    app.message = Some(("Edit Links".into(), format!("The source was changed, but its values could not be read: {why}")));
                }
            }
            Err(e) => app.message = Some(("Edit Links".into(), crate::clean_error(&e))),
        }
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let closed = selected.as_ref().is_some_and(|l| l.get("status").and_then(Json::as_str) != Some("open"));
        if ui
            .add_enabled(closed, egui::Button::new("Update Values"))
            .on_hover_text("Reads the workbook's file for the values the formulas here need. The file is not opened, and nothing in it runs.")
            .clicked()
            && let Some(link) = &selected
        {
            match app.run("data.updateLinks", json!({"link": text(link, "name")})) {
                Ok(r) => {
                    let failed: Vec<String> = r
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|l| l.get("status").and_then(Json::as_str) == Some("failed"))
                        .map(|l| text(l, "message"))
                        .collect();
                    if !failed.is_empty() {
                        app.message = Some(("Edit Links".into(), failed.join("\n\n")));
                    }
                }
                Err(e) => app.message = Some(("Edit Links".into(), crate::clean_error(&e))),
            }
        }
        if ui.add_enabled(selected.is_some(), egui::Button::new("Open Source")).clicked()
            && let Some(link) = &selected
        {
            match app.run("data.openLinkSource", json!({"link": text(link, "name")})) {
                Ok(_) => *open = false,
                Err(e) => app.message = Some(("Edit Links".into(), crate::clean_error(&e))),
            }
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Break Link"))
            .on_hover_text("The formulas in cells that read this workbook become their values. Undo brings them back.")
            .clicked()
            && let Some(link) = &selected
        {
            match app.run("data.breakLink", json!({"link": text(link, "name")})) {
                Ok(r) => app.message = kept_message(&r).map(|m| ("Edit Links".into(), m)),
                Err(e) => app.message = Some(("Edit Links".into(), crate::clean_error(&e))),
            }
        }
        if ui
            .add_enabled(!links.is_empty(), egui::Button::new("Break All"))
            .on_hover_text("Break Link for every workbook in the list, as one Undo step.")
            .clicked()
        {
            match app.run("data.breakLink", json!({"all": true})) {
                Ok(r) => app.message = kept_message(&r).map(|m| ("Edit Links".into(), m)),
                Err(e) => app.message = Some(("Edit Links".into(), crate::clean_error(&e))),
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(" Close ").clicked() {
                *open = false;
            }
        });
    });
}
