//! Home ▸ Conditional Formatting: the menu laid out like Excel's (Highlight Cells Rules and
//! Top/Bottom Rules submenus, the Data Bars, Color Scales and Icon Sets galleries, then New Rule,
//! Clear Rules and Manage Rules). The pictures are drawn in code; the icon sets are
//! [`crate::cf_icons`]'s, the same ones the grid paints.

use egui::containers::menu::SubMenuButton;
use egui::{Atom, Button, Color32, CornerRadius, FontId, Mesh, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value as Json, json};

use super::controls::act;
use crate::SheetApp;
use crate::cf_icons::{self, ICON_SET_GROUPS};
use crate::i18n::Language;

/// Data bar colours: Excel's six, as (name, colour).
pub(crate) const DATA_BARS: [(&str, &str); 6] = [
    ("Blue Data Bar", "#638EC6"),
    ("Green Data Bar", "#63C384"),
    ("Red Data Bar", "#FF555A"),
    ("Orange Data Bar", "#FFB628"),
    ("Light Blue Data Bar", "#008AEF"),
    ("Purple Data Bar", "#D6007B"),
];

const GREEN: &str = "#63BE7B";
const YELLOW: &str = "#FFEB84";
const RED: &str = "#F8696B";
const WHITE: &str = "#FCFCFF";
const BLUE: &str = "#5A8AC6";
const LIGHT_YELLOW: &str = "#FFEF9C";

/// Color scales as Excel names them, highest values' colour first (the order the name reads in
/// and the tile shows top to bottom). The rule takes them lowest first.
pub(crate) const COLOR_SCALES: [(&str, &[&str]); 12] = [
    ("Green - Yellow - Red Color Scale", &[GREEN, YELLOW, RED]),
    ("Red - Yellow - Green Color Scale", &[RED, YELLOW, GREEN]),
    ("Green - White - Red Color Scale", &[GREEN, WHITE, RED]),
    ("Red - White - Green Color Scale", &[RED, WHITE, GREEN]),
    ("Blue - White - Red Color Scale", &[BLUE, WHITE, RED]),
    ("Red - White - Blue Color Scale", &[RED, WHITE, BLUE]),
    ("White - Red Color Scale", &[WHITE, RED]),
    ("Red - White Color Scale", &[RED, WHITE]),
    ("Green - White Color Scale", &[GREEN, WHITE]),
    ("White - Green Color Scale", &[WHITE, GREEN]),
    ("Green - Yellow Color Scale", &[GREEN, LIGHT_YELLOW]),
    ("Yellow - Green Color Scale", &[LIGHT_YELLOW, GREEN]),
];

/// The `colors` a color scale rule takes: lowest values' colour first.
pub(crate) fn scale_rule_colors(named: &[&str]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> =
        named.iter().filter_map(|c| [GREEN, YELLOW, RED, WHITE, BLUE, LIGHT_YELLOW].into_iter().find(|k| k == c)).collect();
    v.reverse();
    v
}

/// The picture beside a menu item.
#[derive(Clone, Copy)]
enum Art {
    Highlight,
    TopBottom,
    DataBars,
    ColorScales,
    IconSets,
    NewRule,
    ClearRules,
    ManageRules,
    /// A small sheet with a sign on it (`>`, `10`, `%`…).
    Sign(&'static str, bool),
}

const ART: f32 = 22.0;

fn hex(c: &str) -> Color32 {
    u32::from_str_radix(c.trim_start_matches('#'), 16).map_or(Color32::GRAY, |v| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// A small sheet: white with a grid of three rows and two columns. Returns the cell rects.
fn sheet(p: &Painter, r: Rect) -> Vec<Rect> {
    let r = r.shrink(1.0);
    p.rect_filled(r, 0.0, Color32::WHITE);
    let (w, h) = (r.width() / 2.0, r.height() / 3.0);
    let cells: Vec<Rect> =
        (0..6).map(|i| Rect::from_min_size(pos2(r.left() + (i % 2) as f32 * w, r.top() + (i / 2) as f32 * h), vec2(w, h))).collect();
    let line = Stroke::new(1.0, Color32::from_gray(0xB0));
    for c in &cells {
        p.rect_stroke(*c, 0.0, line, StrokeKind::Inside);
    }
    cells
}

fn paint_art(p: &Painter, r: Rect, art: Art) {
    match art {
        Art::Highlight => {
            let cells = sheet(p, r);
            if let Some(c) = cells.get(3) {
                p.rect_filled(c.shrink(1.0), 0.0, hex("#FFC7CE"));
                p.rect_stroke(c.shrink(0.5), 0.0, Stroke::new(1.0, hex("#C00000")), StrokeKind::Inside);
            }
        }
        Art::TopBottom => {
            let cells = sheet(p, r);
            for (i, c) in cells.iter().enumerate().filter(|(i, _)| *i < 2) {
                p.rect_filled(c.shrink(1.0), 0.0, if i == 0 { hex("#C6EFCE") } else { hex("#FFC7CE") });
            }
            p.text(r.center() + vec2(0.0, 3.0), egui::Align2::CENTER_CENTER, "10", FontId::proportional(9.0), hex("#2B6CC4"));
        }
        Art::DataBars => {
            let cells = sheet(p, r);
            for (i, f) in [0.9_f32, 0.55, 0.3].into_iter().enumerate() {
                if let Some(c) = cells.get(i * 2) {
                    let c = c.shrink(1.5);
                    p.rect_filled(Rect::from_min_size(c.min, vec2((r.width() - 4.0) * f, c.height())), 0.0, hex("#638EC6"));
                }
            }
        }
        Art::ColorScales => {
            let cells = sheet(p, r);
            for (i, col) in [GREEN, GREEN, YELLOW, YELLOW, RED, RED].into_iter().enumerate() {
                if let Some(c) = cells.get(i) {
                    p.rect_filled(c.shrink(1.0), 0.0, hex(col));
                }
            }
        }
        Art::IconSets => {
            let cells = sheet(p, r);
            for (i, idx) in [2usize, 1, 0].into_iter().enumerate() {
                if let Some(c) = cells.get(i * 2) {
                    let s = c.height() - 1.0;
                    cf_icons::paint(p, Rect::from_center_size(c.center(), vec2(s, s)), "3Arrows", idx);
                }
            }
        }
        Art::NewRule => {
            sheet(p, r);
            let c = pos2(r.right() - 5.0, r.bottom() - 5.0);
            p.circle_filled(c, 5.0, hex("#2E9E4F"));
            let st = Stroke::new(1.6, Color32::WHITE);
            p.line_segment([c - vec2(3.0, 0.0), c + vec2(3.0, 0.0)], st);
            p.line_segment([c - vec2(0.0, 3.0), c + vec2(0.0, 3.0)], st);
        }
        Art::ClearRules => {
            let cells = sheet(p, r);
            if let Some(c) = cells.get(2) {
                p.rect_filled(c.shrink(1.0), 0.0, hex("#FFC7CE"));
            }
            let c = pos2(r.right() - 5.0, r.bottom() - 5.0);
            let st = Stroke::new(2.0, hex("#D13B2F"));
            p.line_segment([c - vec2(4.0, 4.0), c + vec2(4.0, 4.0)], st);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], st);
        }
        Art::ManageRules => {
            let r2 = r.shrink(1.0);
            p.rect_filled(r2, 0.0, Color32::WHITE);
            p.rect_stroke(r2, 0.0, Stroke::new(1.0, Color32::from_gray(0x90)), StrokeKind::Inside);
            for (i, col) in ["#FFC7CE", "#C6EFCE", "#FFEB9C"].into_iter().enumerate() {
                let y = r2.top() + 4.0 + i as f32 * 5.5;
                p.rect_filled(Rect::from_min_size(pos2(r2.left() + 3.0, y), vec2(5.0, 3.5)), 0.0, hex(col));
                p.line_segment([pos2(r2.left() + 10.0, y + 1.75), pos2(r2.right() - 3.0, y + 1.75)], Stroke::new(1.0, Color32::from_gray(0x60)));
            }
        }
        Art::Sign(sign, bottom) => {
            let cells = sheet(p, r);
            if let Some(c) = cells.get(if bottom { 5 } else { 0 }) {
                p.rect_filled(c.shrink(1.0), 0.0, hex("#FFC7CE"));
            }
            let badge = Rect::from_center_size(r.center(), vec2(r.width() * 0.8, 11.0));
            p.rect_filled(badge, 2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 230));
            p.text(badge.center(), egui::Align2::CENTER_CENTER, sign, FontId::proportional(9.5), hex("#1F4E9A"));
        }
    }
}

fn art_atom(art: Art) -> Atom<'static> {
    Atom::paint(vec2(ART, ART), move |ui: &Ui, a: egui::AtomPaintArgs| paint_art(ui.painter(), a.rect, art))
}

/// A menu item with its picture.
fn item(lang: Language, ui: &mut Ui, art: Art, label: &str) -> Response {
    let text = lang.tr(label).to_string();
    ui.add(Button::new((art_atom(art), text)).frame_when_inactive(false).min_size(vec2(230.0, ART + 6.0)))
}

/// A submenu item with its picture.
fn submenu(lang: Language, ui: &mut Ui, art: Art, label: &str, add_contents: impl FnOnce(&mut Ui)) {
    let text = lang.tr(label).to_string();
    let button =
        Button::new((art_atom(art), text)).frame_when_inactive(false).min_size(vec2(230.0, ART + 6.0)).right_text(SubMenuButton::arrow_atom(None));
    SubMenuButton::from_button(button).ui(ui, add_contents);
}

fn more_rules(app: &mut SheetApp, ui: &mut Ui) {
    ui.separator();
    let text = app.ui.language.tr("More Rules…").to_string();
    if ui.add(Button::new(text).frame_when_inactive(false).min_size(vec2(230.0, 22.0))).clicked() {
        new_rule(app);
    }
}

fn new_rule(app: &mut SheetApp) {
    app.open_dialog("cfQuick", json!({"type": "expression", "title": "New Formatting Rule"}));
}

fn heading(app: &SheetApp, ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(app.ui.language.tr(text)).strong());
}

/// A gallery tile: a hoverable square that paints `paint` and runs `rule` when clicked.
fn tile(app: &mut SheetApp, ui: &mut Ui, size: egui::Vec2, tip: &str, rule: Json, paint: impl FnOnce(&Painter, Rect)) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let visuals = ui.style().interact(&resp);
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(rect, CornerRadius::same(2), visuals.weak_bg_fill);
        ui.painter().rect_stroke(rect, CornerRadius::same(2), visuals.bg_stroke, StrokeKind::Inside);
    }
    paint(ui.painter(), rect.shrink(4.0));
    let resp = resp.on_hover_text(app.ui.language.tr(tip));
    if resp.clicked() {
        act(app, "home.conditionalFormat", json!({"rule": rule}));
        ui.close();
    }
}

fn data_bar_tile(p: &Painter, r: Rect, color: Color32, gradient: bool) {
    p.rect_filled(r, 0.0, Color32::WHITE);
    p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(0xC8)), StrokeKind::Inside);
    let h = (r.height() - 8.0) / 3.0;
    for (i, f) in [0.9_f32, 0.6, 0.3].into_iter().enumerate() {
        let bar = Rect::from_min_size(pos2(r.left() + 2.0, r.top() + 2.0 + i as f32 * (h + 2.0)), vec2((r.width() - 4.0) * f, h));
        if gradient {
            let mut mesh = Mesh::default();
            let light = Color32::from_rgb(
                (color.r() as u16 + 3 * 255).div_ceil(4) as u8,
                (color.g() as u16 + 3 * 255).div_ceil(4) as u8,
                (color.b() as u16 + 3 * 255).div_ceil(4) as u8,
            );
            let base = mesh.vertices.len() as u32;
            for (pos, col) in [(bar.left_top(), color), (bar.right_top(), light), (bar.right_bottom(), light), (bar.left_bottom(), color)] {
                mesh.colored_vertex(pos, col);
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
            p.add(mesh);
            p.rect_stroke(bar, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
        } else {
            p.rect_filled(bar, 0.0, color);
        }
    }
}

fn color_scale_tile(p: &Painter, r: Rect, named: &[&str]) {
    let rows = 3;
    let h = r.height() / rows as f32;
    for i in 0..rows {
        let t = i as f32 / (rows - 1) as f32;
        let col = match named {
            [a, b] => lerp(hex(a), hex(b), t),
            [a, m, b] if t <= 0.5 => lerp(hex(a), hex(m), t * 2.0),
            [_, m, b] => lerp(hex(m), hex(b), (t - 0.5) * 2.0),
            _ => Color32::WHITE,
        };
        for j in 0..2 {
            let cell = Rect::from_min_size(pos2(r.left() + j as f32 * r.width() / 2.0, r.top() + i as f32 * h), vec2(r.width() / 2.0, h));
            p.rect_filled(cell, 0.0, col);
            p.rect_stroke(cell, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
        }
    }
    p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(0xC8)), StrokeKind::Outside);
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn data_bars_gallery(app: &mut SheetApp, ui: &mut Ui) {
    for (title, gradient) in [("Gradient Fill", true), ("Solid Fill", false)] {
        heading(app, ui, title);
        egui::Grid::new(("cfDataBars", gradient)).spacing(vec2(2.0, 2.0)).show(ui, |ui| {
            for (i, (name, c)) in DATA_BARS.into_iter().enumerate() {
                let color = hex(c);
                tile(app, ui, vec2(46.0, 46.0), name, json!({"type": "dataBar", "color": c, "gradient": gradient}), |p, r| {
                    data_bar_tile(p, r, color, gradient)
                });
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    }
    more_rules(app, ui);
}

fn color_scales_gallery(app: &mut SheetApp, ui: &mut Ui) {
    egui::Grid::new("cfColorScales").spacing(vec2(2.0, 2.0)).show(ui, |ui| {
        for (i, (name, named)) in COLOR_SCALES.into_iter().enumerate() {
            tile(app, ui, vec2(46.0, 46.0), name, json!({"type": "colorScale", "colors": scale_rule_colors(named)}), |p, r| {
                color_scale_tile(p, r, named)
            });
            if i % 4 == 3 {
                ui.end_row();
            }
        }
    });
    more_rules(app, ui);
}

fn icon_sets_gallery(app: &mut SheetApp, ui: &mut Ui) {
    for (group, sets) in ICON_SET_GROUPS {
        heading(app, ui, group);
        egui::Grid::new(("cfIconSets", group)).spacing(vec2(2.0, 2.0)).show(ui, |ui| {
            for (i, (set, name)) in sets.iter().enumerate() {
                let n = cf_icons::count(set);
                tile(app, ui, vec2(112.0, 26.0), name, json!({"type": "iconSet", "set": set}), |p, r| {
                    let s = r.height().min(16.0);
                    for k in 0..n {
                        let at = Rect::from_min_size(pos2(r.left() + 2.0 + k as f32 * (s + 4.0), r.center().y - s / 2.0), vec2(s, s));
                        // Highest values' icon first, as Excel's gallery shows them.
                        cf_icons::paint(p, at, set, n - 1 - k);
                    }
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    }
    more_rules(app, ui);
}

/// What the Clear Rules submenu can clear besides the selection: the table and the PivotTable
/// the active cell is in, as their ranges.
fn clear_targets(app: &SheetApp) -> (Option<String>, Option<String>) {
    let Some(doc) = app.session.active() else { return (None, None) };
    let Some(sh) = doc.wb.active() else { return (None, None) };
    let at = doc.selection.active;
    let table = sh.table_at(at).map(|t| t.range.a1());
    let pivot = sh.pivots.iter().filter_map(|p| p.last_range).find(|r| r.contains(at)).map(|r| r.a1());
    (table, pivot)
}

pub(super) fn cf_menu(app: &mut SheetApp, ui: &mut Ui) {
    let lang = app.ui.language;
    submenu(lang, ui, Art::Highlight, "Highlight Cells Rules", |ui| {
        for (sign, label, op) in
            [(">", "Greater Than…", "greater"), ("<", "Less Than…", "less"), ("< >", "Between…", "between"), ("=", "Equal To…", "equal")]
        {
            if item(lang, ui, Art::Sign(sign, false), label).clicked() {
                app.open_dialog("cfQuick", json!({"type": "cellIs", "operator": op, "title": label.trim_end_matches('…')}));
            }
        }
        if item(lang, ui, Art::Sign("ab", false), "Text that Contains…").clicked() {
            app.open_dialog("cfQuick", json!({"type": "containsText", "title": "Text that Contains"}));
        }
        if item(lang, ui, Art::Sign("31", false), "A Date Occurring…").clicked() {
            app.open_dialog("cfQuick", json!({"type": "timePeriod", "title": "A Date Occurring"}));
        }
        if item(lang, ui, Art::Sign("2x", false), "Duplicate Values…").clicked() {
            app.open_dialog("cfQuick", json!({"type": "duplicate", "title": "Duplicate Values"}));
        }
        more_rules(app, ui);
    });
    submenu(lang, ui, Art::TopBottom, "Top/Bottom Rules", |ui| {
        for (sign, label, title, rule) in [
            ("10", "Top 10 Items…", "Top 10 Items", json!({"type": "top10", "rank": 10})),
            ("%", "Top 10%…", "Top 10%", json!({"type": "top10", "rank": 10, "percent": true})),
            ("10", "Bottom 10 Items…", "Bottom 10 Items", json!({"type": "top10", "rank": 10, "bottom": true})),
            ("%", "Bottom 10%…", "Bottom 10%", json!({"type": "top10", "rank": 10, "bottom": true, "percent": true})),
            ("avg", "Above Average…", "Above Average", json!({"type": "aboveAverage"})),
            ("avg", "Below Average…", "Below Average", json!({"type": "aboveAverage", "below": true})),
        ] {
            let bottom = rule.get("bottom").is_some() || rule.get("below").is_some();
            if item(lang, ui, Art::Sign(sign, bottom), label).clicked() {
                let mut p = rule;
                p["title"] = json!(title);
                app.open_dialog("cfQuick", p);
            }
        }
        more_rules(app, ui);
    });
    ui.separator();
    submenu(lang, ui, Art::DataBars, "Data Bars", |ui| data_bars_gallery(app, ui));
    submenu(lang, ui, Art::ColorScales, "Color Scales", |ui| color_scales_gallery(app, ui));
    submenu(lang, ui, Art::IconSets, "Icon Sets", |ui| icon_sets_gallery(app, ui));
    ui.separator();
    if item(lang, ui, Art::NewRule, "New Rule…").clicked() {
        new_rule(app);
    }
    submenu(lang, ui, Art::ClearRules, "Clear Rules", |ui| {
        let (table, pivot) = clear_targets(app);
        let tr = |s: &str| lang.tr(s).to_string();
        let button = |text: String| Button::new(text).frame_when_inactive(false).min_size(vec2(230.0, 22.0));
        if ui.add(button(tr("Clear Rules from Selected Cells"))).clicked() {
            act(app, "home.clearRules", json!({}));
        }
        if ui.add(button(tr("Clear Rules from Entire Sheet"))).clicked() {
            act(app, "home.clearRules", json!({"sheet": true}));
        }
        if ui.add_enabled(table.is_some(), button(tr("Clear Rules from This Table"))).clicked()
            && let Some(range) = table
        {
            act(app, "home.clearRules", json!({"range": range}));
        }
        if ui.add_enabled(pivot.is_some(), button(tr("Clear Rules from This PivotTable"))).clicked()
            && let Some(range) = pivot
        {
            act(app, "home.clearRules", json!({"range": range}));
        }
    });
    if item(lang, ui, Art::ManageRules, "Manage Rules…").clicked() {
        app.open_dialog("manageRules", json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_scales_take_the_lowest_values_colour_first() {
        // "Green - Yellow - Red": the highest values green, so the rule's first (min) stop is red.
        assert_eq!(scale_rule_colors(&[GREEN, YELLOW, RED]), [RED, YELLOW, GREEN]);
        assert_eq!(scale_rule_colors(&[WHITE, GREEN]), [GREEN, WHITE]);
        for (name, named) in COLOR_SCALES {
            assert_eq!(scale_rule_colors(named).len(), named.len(), "{name}");
        }
    }
}
