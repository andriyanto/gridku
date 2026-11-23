//! Data › Edit Links: the other workbooks this one's formulas read, and what can be done about
//! each (`docs/external-references.md`, stages B and C).
//!
//! A link is another workbook's name, where it is, and the values this workbook read of it
//! ([`gridcraft_model::links`]). The session keeps those values current while the other workbook
//! is open ([`crate::book_links`]). The commands here are for the ones that are not: the list
//! says which they are, Change Source points the formulas at another workbook, and Break Link
//! leaves their values and takes the formulas away.
//!
//! A link is not only read by cell formulas: defined names, conditional formats, data
//! validation, chart series, sparklines and PivotTable sources can read it too, and Excel's
//! Break Link leaves those (the "links that won't break"). `data.linkUses` lists every place
//! that reads a link, so each can be found and fixed; Break Link turns the cell formulas into
//! values and keeps the link while any other place still reads it, saying which.

use std::sync::Arc;

use gridcraft_core::{CellRef, RangeRef, Value};
use gridcraft_formula::{SheetSel, same_name};
use gridcraft_model::{CfRule, CfValueKind, ExternalLink, Formula, Visibility, Workbook};
use serde_json::{Value as Json, json};

use crate::cmd::{CommandSpec, bad, cmd, edit, has_doc, str_param};
use crate::{EngineError, Result, Session};

/// Most values Break Link writes for the results of formulas that fill many cells.
const MAX_BROKEN_VALUES: u64 = 2_000_000;
/// Most places `data.linkUses` lists for one link (all are counted).
const MAX_LISTED_USES: usize = 10_000;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "data.editLinks", "Edit Links", ["Data", "Queries & Connections"], None, "{} → [{name, target, path, status: \"open\" | \"saved\" | \"unknown\", sheets, values, reads, fromFile}]: the other workbooks this one's formulas read (`saved`: the values the file has; `unknown`: none)", has_doc, list),
        cmd!(
            "data.changeLinkSource",
            "Change Source",
            ["Data", "Queries & Connections"],
            None,
            "{link: \"Budget.xlsx\", path: a file | name: an open workbook} → {name, read?, message?}: the formulas that read `link` read that workbook instead; a file that is not open is read at once (`read`)",
            has_doc,
            change_source
        ),
        cmd!(
            noundo "data.updateLinks",
            "Update Values",
            ["Data", "Queries & Connections"],
            None,
            "{link?: one link, else all} → [{name, status: \"updated\" | \"unchanged\" | \"open\" | \"failed\", message?}]: reads from the files of the linked workbooks that are not open the values this workbook's formulas need",
            has_doc,
            update
        ),
        cmd!(
            noundo "data.openLinkSource",
            "Open Source",
            ["Data", "Queries & Connections"],
            None,
            "{link} → {path}: opens the linked workbook's file (or goes to it when it is open)",
            has_doc,
            open_source
        ),
        cmd!(
            "data.breakLink",
            "Break Link",
            ["Data", "Queries & Connections"],
            None,
            "{link | all: true} → {cells, names: [defined names that still read it], kept: {names, conditionalFormats, validations, charts, sparklines, pivots}, links: [links kept]}: the cell formulas that read the workbook (or every linked workbook) become their values; the link stays while another place still reads it",
            has_doc,
            break_link
        ),
        cmd!(
            query "data.linkUses",
            "Find Link",
            ["Data", "Queries & Connections"],
            None,
            "{link} → {places: [{kind: \"cell\" | \"name\" | \"conditionalFormat\" | \"validation\" | \"chart\" | \"sparkline\" | \"pivot\", sheet?, range?, name?, id?, hidden?}], more, counts: {cells, names, conditionalFormats, validations, charts, sparklines, pivots}}: every place that reads the linked workbook, in order (none: the link is read nowhere)",
            has_doc,
            link_uses
        ),
    ]
}

/// The name of the active workbook's link that `p.link` asks for, as the link spells it.
fn link_param(s: &Session, p: &Json, id: &str) -> Result<String> {
    let asked = str_param(p, "link").ok_or_else(|| bad(id, "missing `link`: the linked workbook's name"))?;
    gridcraft_model::links::find(&s.doc()?.wb.links, asked)
        .map(|l| l.name.clone())
        .ok_or_else(|| bad(id, format!("this workbook has no link to a workbook called \"{asked}\"")))
}

fn list(s: &mut Session, _: &Json) -> Result<Json> {
    let reader = s.active_index();
    let d = s.doc()?;
    let folder = d.wb.source_dir.as_deref();
    let links = d.wb.links.iter().map(|link| {
        let source = s.source_of(reader, &link.name);
        let status = match source {
            Some(_) => "open",
            None if link.sheets.is_empty() => "unknown",
            None => "saved",
        };
        // Where the other workbook is: the file it is open from, else what the link's path
        // names (text: not looked for).
        let path = source
            .and_then(crate::book_links::file_of)
            .or_else(|| link.target.as_deref().and_then(|t| gridcraft_xlsx::link_path(t, folder)))
            .map(|p| p.to_string_lossy().into_owned());
        json!({
            "name": link.name,
            "target": link.target,
            "path": path,
            "status": status,
            "sheets": link.sheets.len(),
            "values": link.cell_count(),
            "reads": d.links.reads(&link.name),
            "fromFile": link.number.is_some(),
        })
    });
    Ok(Json::Array(links.collect()))
}

/// Takes the link at `at` out of `wb`. One the workbook's file brought also leaves the content
/// kept from that file, when that can be done without renumbering the file's other links.
fn drop_link(wb: &mut Workbook, at: usize) {
    if at >= wb.links.len() {
        return;
    }
    let link = wb.links.remove(at);
    if let Some(kept) = link.number.and_then(|n| gridcraft_xlsx::forget_link(wb, n)) {
        wb.preserved = Some(Arc::new(kept));
    }
}

fn change_source(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.changeLinkSource";
    let old = link_param(s, p, ID)?;
    let d = s.doc()?;
    let (name, target) = match (str_param(p, "path"), str_param(p, "name")) {
        (Some(path), _) => {
            let file = crate::io::source_path(path).filter(|f| f.is_absolute()).ok_or_else(|| bad(ID, "`path` is not a file's path"))?;
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| bad(ID, "`path` names no file"))?;
            // Relative to this workbook's folder when it has one; a save writes it so anyway.
            let relative = d.wb.source_dir.as_deref().and_then(|folder| gridcraft_xlsx::link_target(&file, folder));
            (name, relative.or_else(|| gridcraft_xlsx::link_target_absolute(&file)))
        }
        (None, Some(name)) => (name.to_string(), None),
        (None, None) => return Err(bad(ID, "give the new source: `path` (a file) or `name` (an open workbook)")),
    };
    let from_file = str_param(p, "path").is_some();
    if !SheetSel::can_name_book(&name) {
        return Err(bad(ID, format!("a formula can't name a workbook called \"{name}\"")));
    }
    if same_name(&name, &d.display_title()) {
        return Err(bad(ID, "a workbook's formulas read its own cells without a link"));
    }
    edit(s, |cx| {
        let at = cx.wb.links.iter().position(|l| same_name(&l.name, &old)).ok_or_else(|| bad(ID, "the link is gone"))?;
        if !same_name(&old, &name) {
            cx.wb.rename_books(&|book| same_name(book, &old).then(|| name.clone()));
        }
        // A link to that workbook already: the formulas read that one from now on.
        let other = cx.wb.links.iter().position(|l| same_name(&l.name, &name)).filter(|o| *o != at);
        match (other, cx.wb.links.get_mut(at)) {
            (Some(_), _) => drop_link(&mut cx.wb, at),
            (None, Some(link)) => {
                let link = Arc::make_mut(link);
                link.name.clone_from(&name);
                link.target = target;
                // Nothing is known of the new workbook until it is open here, or read.
                link.sheets.clear();
            }
            (None, None) => {}
        }
        cx.structural = true;
        Ok(())
    })?;
    // A file was named: what the formulas read of it is read at once, unless the workbook is
    // open here (its values are followed anyway).
    let reader = s.active_index();
    let mut result = json!({"name": name});
    if from_file && s.source_of(reader, &name).is_none() {
        match s.update_link(reader, &name) {
            Ok(_) => result["read"] = json!(true),
            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
            // The change stands; its formulas say `#REF!` until the workbook can be read.
            Err(e) => {
                result["read"] = json!(false);
                result["message"] = json!(e.to_string());
            }
        }
    }
    Ok(result)
}

fn update(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.updateLinks";
    let reader = s.active_index();
    let names: Vec<String> = match str_param(p, "link") {
        Some(_) => vec![link_param(s, p, ID)?],
        None => s.doc()?.wb.links.iter().map(|l| l.name.clone()).collect(),
    };
    let mut results = Vec::new();
    for name in names {
        let (status, message) = if s.source_of(reader, &name).is_some() {
            ("open", None)
        } else {
            match s.update_link(reader, &name) {
                Ok(true) => ("updated", None),
                Ok(false) => ("unchanged", None),
                Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                Err(e) => ("failed", Some(e.to_string())),
            }
        };
        results.push(json!({"name": name, "status": status, "message": message}));
    }
    Ok(Json::Array(results))
}

fn open_source(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.openLinkSource";
    let name = link_param(s, p, ID)?;
    let reader = s.active_index();
    if let Some(uid) = s.source_of(reader, &name).map(|d| d.uid) {
        s.execute("window.activate", json!({"doc": uid}))?;
        return Ok(json!({"open": true}));
    }
    let path = s.link_file(reader, &name)?.to_string_lossy().into_owned();
    s.execute("file.open", json!({"path": path}))?;
    Ok(json!({"path": path}))
}

/// Whether the cell formula `formula` reads `link` (its parsed form is kept, so this is cheap).
fn reads(formula: &Formula, link: &ExternalLink) -> bool {
    crate::book_links::may_read(&formula.text)
        && formula
            .with_expr(|e| {
                let mut found = Vec::new();
                gridcraft_formula::adjust::external_refs(e, &mut found);
                found.iter().any(|(b, _, _)| is_link(b, link))
            })
            .unwrap_or(false)
}

/// Whether `book`, as a formula names it between the brackets, is `link`: by its name, or by
/// its number in the file it came from (`[1]`).
fn is_link(book: &str, link: &ExternalLink) -> bool {
    same_name(book, &link.name) || link.number.is_some_and(|n| book == n.to_string())
}

/// Whether formula text (with or without its `=`) reads `link`.
fn text_reads(text: &str, link: &ExternalLink) -> bool {
    let t = text.trim();
    let t = t.strip_prefix('=').unwrap_or(t);
    if !crate::book_links::may_read(t) {
        return false;
    }
    let Ok(e) = gridcraft_formula::parse(t).or_else(|_| gridcraft_formula::parse_name(t)) else { return false };
    let mut found = Vec::new();
    gridcraft_formula::adjust::external_refs(&e, &mut found);
    found.iter().any(|(b, _, _)| is_link(b, link))
}

/// The formula texts a conditional format holds: comparison values, its rule's formula, and
/// thresholds given as formulas.
fn cf_texts(rule: &CfRule) -> Vec<&str> {
    fn kind(k: &CfValueKind) -> Option<&str> {
        match k {
            CfValueKind::Formula(f) => Some(f.as_str()),
            _ => None,
        }
    }
    match rule {
        CfRule::CellIs { a, b, .. } => std::iter::once(a.as_str()).chain(b.as_deref()).collect(),
        CfRule::Expression { formula, .. } => vec![formula.as_str()],
        CfRule::ColorScale { stops } => stops.iter().filter_map(|(k, _)| kind(k)).collect(),
        CfRule::DataBar { min, max, .. } => [min, max].into_iter().filter_map(kind).collect(),
        CfRule::IconSet { thresholds, .. } => thresholds.iter().filter_map(kind).collect(),
        _ => vec![],
    }
}

/// Every place of a workbook that reads one link, counted by kind.
#[derive(Default)]
struct Uses {
    places: Vec<Json>,
    more: bool,
    cells: u64,
    names: Vec<String>,
    conditional_formats: u64,
    validations: u64,
    charts: u64,
    sparklines: u64,
    pivots: u64,
}

impl Uses {
    fn add(&mut self, place: Json) {
        if self.places.len() < MAX_LISTED_USES {
            self.places.push(place);
        } else {
            self.more = true;
        }
    }

    /// Places Break Link does not turn into values.
    fn others(&self) -> u64 {
        self.conditional_formats + self.validations + self.charts + self.sparklines + self.pivots
    }

    fn kept(&self) -> Json {
        json!({
            "names": self.names.len(),
            "conditionalFormats": self.conditional_formats,
            "validations": self.validations,
            "charts": self.charts,
            "sparklines": self.sparklines,
            "pivots": self.pivots,
        })
    }

    fn counts(&self) -> Json {
        let mut c = self.kept();
        c["cells"] = json!(self.cells);
        c
    }
}

/// The places of `wb` that read `link`: cells sheet by sheet, then defined names, then per
/// sheet its conditional formats, data validation, charts, sparklines and PivotTables.
fn uses_of(wb: &Workbook, link: &ExternalLink) -> Uses {
    let mut u = Uses::default();
    for sheet in &wb.sheets {
        let hidden = sheet.visibility != Visibility::Visible;
        for (c, cell) in sheet.cells.iter() {
            if cell.formula.as_deref().is_some_and(|f| reads(f, link)) {
                u.cells += 1;
                u.add(json!({"kind": "cell", "sheet": sheet.name, "range": c.a1(), "hidden": hidden}));
            }
        }
    }
    for n in wb.names.iter().filter(|n| text_reads(&n.formula, link)) {
        u.names.push(n.name.clone());
        let scope = n.scope.and_then(|i| wb.sheet(i)).map(|s| s.name.clone());
        u.add(json!({"kind": "name", "name": n.name, "sheet": scope, "hidden": n.hidden}));
    }
    let a1 = |ranges: &[RangeRef]| ranges.iter().map(RangeRef::a1).collect::<Vec<_>>().join(",");
    for sheet in &wb.sheets {
        let hidden = sheet.visibility != Visibility::Visible;
        for (i, cf) in sheet.cond_formats.iter().enumerate() {
            if cf_texts(&cf.rule).into_iter().any(|t| text_reads(t, link)) {
                u.conditional_formats += 1;
                u.add(json!({"kind": "conditionalFormat", "sheet": sheet.name, "range": a1(&cf.ranges), "index": i, "hidden": hidden}));
            }
        }
        for (i, v) in sheet.validations.iter().enumerate() {
            if std::iter::once(v.f1.as_str()).chain(v.f2.as_deref()).any(|t| text_reads(t, link)) {
                u.validations += 1;
                u.add(json!({"kind": "validation", "sheet": sheet.name, "range": a1(&v.ranges), "index": i, "hidden": hidden}));
            }
        }
        for chart in &sheet.charts {
            let series =
                chart.series.iter().flat_map(|s| [s.name.as_deref(), s.categories.as_deref(), Some(s.values.as_str()), s.bubble_sizes.as_deref()]);
            if series.chain([chart.title.as_deref()]).flatten().any(|t| text_reads(t, link)) {
                u.charts += 1;
                u.add(json!({"kind": "chart", "sheet": sheet.name, "id": chart.id, "name": chart.title, "hidden": hidden}));
            }
        }
        for sp in sheet.sparklines.iter().filter(|sp| text_reads(&sp.source, link)) {
            u.sparklines += 1;
            u.add(json!({"kind": "sparkline", "sheet": sheet.name, "range": sp.cell.a1(), "hidden": hidden}));
        }
        for pivot in sheet.pivots.iter().filter(|p| text_reads(&p.source, link)) {
            u.pivots += 1;
            let range = pivot.last_range.unwrap_or(RangeRef::cell(pivot.anchor));
            u.add(json!({"kind": "pivot", "sheet": sheet.name, "name": pivot.name, "range": range.a1(), "hidden": hidden}));
        }
    }
    u
}

fn link_uses(s: &mut Session, p: &Json) -> Result<Json> {
    let name = link_param(s, p, "data.linkUses")?;
    let wb = &s.doc()?.wb;
    let Some(link) = gridcraft_model::links::find(&wb.links, &name) else { return Ok(json!({"places": [], "more": false})) };
    let u = uses_of(wb, link);
    Ok(json!({"places": u.places, "more": u.more, "counts": u.counts()}))
}

/// Turns the cell formulas of `cx` that read `name` into their values. Returns how many
/// formulas there were.
fn break_cells(cx: &mut crate::cmd::Ctx, name: &str, written: &mut u64) -> Result<u64> {
    const ID: &str = "data.breakLink";
    let Some(link) = gridcraft_model::links::find(&cx.wb.links, name).cloned() else { return Ok(0) };
    let mut cells = 0u64;
    for si in 0..cx.wb.sheets.len() {
        let Some(sheet) = cx.wb.sheet(si) else { continue };
        // Each formula that reads the workbook, with the cells its result fills.
        let found: Vec<(CellRef, Option<RangeRef>)> = sheet
            .cells
            .iter()
            .filter(|(_, cell)| cell.formula.as_deref().is_some_and(|f| reads(f, &link)))
            .map(|(c, _)| (c, sheet.spill_ranges.get(&c).copied().filter(|r| !r.is_single())))
            .collect();
        let mut values: Vec<(CellRef, Value)> = Vec::new();
        for (c, spill) in found {
            cells += 1;
            let range = spill.unwrap_or(RangeRef::cell(c));
            *written = written.saturating_add(range.count());
            if *written > MAX_BROKEN_VALUES {
                return Err(bad(ID, format!("the formulas that read \"{name}\" fill more than {MAX_BROKEN_VALUES} cells")));
            }
            for row in range.start.row..=range.end.row {
                for col in range.start.col..=range.end.col {
                    let at = CellRef::new(row, col);
                    values.push((at, sheet.value(at)));
                }
            }
        }
        let sheet = cx.sheet_mut(si)?;
        for (at, value) in values {
            sheet.set_value(at, value);
        }
    }
    Ok(cells)
}

fn break_link(s: &mut Session, p: &Json) -> Result<Json> {
    const ID: &str = "data.breakLink";
    let all = p.get("all").and_then(Json::as_bool).unwrap_or(false);
    let targets: Vec<String> = if all { s.doc()?.wb.links.iter().map(|l| l.name.clone()).collect() } else { vec![link_param(s, p, ID)?] };
    edit(s, |cx| {
        let mut cells = 0u64;
        let mut written = 0u64;
        let mut names: Vec<String> = Vec::new();
        let mut kept: Vec<Json> = Vec::new();
        let mut totals = Uses::default();
        for name in &targets {
            cells += break_cells(cx, name, &mut written)?;
            // What still reads the workbook keeps the link, as it still needs its values: a
            // defined name is not a value, and conditional formats, data validation, charts,
            // sparklines and PivotTables are left for a person to change (as in Excel).
            let Some(at) = cx.wb.links.iter().position(|l| same_name(&l.name, name)) else { continue };
            let Some(link) = cx.wb.links.get(at).cloned() else { continue };
            let u = uses_of(&cx.wb, &link);
            if u.names.is_empty() && u.others() == 0 {
                drop_link(&mut cx.wb, at);
                continue;
            }
            for n in &u.names {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
            kept.push(json!({"name": name, "kept": u.kept()}));
            totals.names.extend(u.names);
            totals.conditional_formats += u.conditional_formats;
            totals.validations += u.validations;
            totals.charts += u.charts;
            totals.sparklines += u.sparklines;
            totals.pivots += u.pivots;
        }
        cx.structural = true;
        let mut kept_counts = totals.kept();
        kept_counts["names"] = json!(names.len());
        Ok(json!({"cells": cells, "names": names, "kept": kept_counts, "links": kept}))
    })
}
