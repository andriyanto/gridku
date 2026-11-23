//! Conditional-formatting icon sets, drawn in code: the icons a rule shows in its cells, and
//! the names and groups the Conditional Formatting ▸ Icon Sets gallery lists. The set names
//! are the file format's (`3Arrows`, `5Quarters`…); the drawings are GridKu's own.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// The Icon Sets gallery: Excel's four groups, each set as (file name, menu label).
pub const ICON_SET_GROUPS: [(&str, &[(&str, &str)]); 4] = [
    (
        "Directional",
        &[
            ("3Arrows", "3 Arrows (Colored)"),
            ("3ArrowsGray", "3 Arrows (Gray)"),
            ("3Triangles", "3 Triangles"),
            ("4ArrowsGray", "4 Arrows (Gray)"),
            ("4Arrows", "4 Arrows (Colored)"),
            ("5ArrowsGray", "5 Arrows (Gray)"),
            ("5Arrows", "5 Arrows (Colored)"),
        ],
    ),
    (
        "Shapes",
        &[
            ("3TrafficLights1", "3 Traffic Lights (Unrimmed)"),
            ("3TrafficLights2", "3 Traffic Lights (Rimmed)"),
            ("3Signs", "3 Signs"),
            ("4TrafficLights", "4 Traffic Lights"),
            ("4RedToBlack", "Red To Black"),
        ],
    ),
    ("Indicators", &[("3Symbols", "3 Symbols (Circled)"), ("3Symbols2", "3 Symbols (Uncircled)"), ("3Flags", "3 Flags")]),
    ("Ratings", &[("3Stars", "3 Stars"), ("4Rating", "4 Ratings"), ("5Quarters", "5 Quarters"), ("5Rating", "5 Ratings"), ("5Boxes", "5 Boxes")]),
];

const RED: Color32 = Color32::from_rgb(0xD1, 0x3B, 0x2F);
const AMBER: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x29);
const GREEN: Color32 = Color32::from_rgb(0x2E, 0x9E, 0x4F);
const GRAY: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
const BLACK: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const PINK: Color32 = Color32::from_rgb(0xEC, 0x9C, 0x96);
const GOLD: Color32 = Color32::from_rgb(0xF2, 0xA9, 0x1E);
const BLUE: Color32 = Color32::from_rgb(0x2B, 0x6C, 0xC4);
const PALE: Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);

/// Number of icons in `set`, the way the engine counts them.
pub fn count(set: &str) -> usize {
    gridcraft_engine::cf::icon_count(set)
}

/// Paints icon `idx` of `set` (0 is the lowest values' icon) in the square `r`.
pub fn paint(p: &Painter, r: Rect, set: &str, idx: usize) {
    let n = count(set);
    let idx = idx.min(n.saturating_sub(1));
    let c = r.center();
    let s = r.width().min(r.height());
    match set {
        "3Arrows" | "3ArrowsGray" | "4Arrows" | "4ArrowsGray" | "5Arrows" | "5ArrowsGray" => {
            let angles: &[f32] = match n {
                3 => &[180.0, 90.0, 0.0],
                4 => &[180.0, 135.0, 45.0, 0.0],
                _ => &[180.0, 135.0, 90.0, 45.0, 0.0],
            };
            let col = if set.ends_with("Gray") {
                GRAY
            } else if idx == 0 {
                RED
            } else if idx + 1 == n {
                GREEN
            } else {
                AMBER
            };
            arrow(p, c, s, angles.get(idx).copied().unwrap_or(90.0), col);
        }
        "3Triangles" => match idx {
            0 => triangle(p, c, s, true, RED),
            1 => {
                p.rect_filled(Rect::from_center_size(c, vec2(s * 0.7, s * 0.22)), 0.0, AMBER);
            }
            _ => triangle(p, c, s, false, GREEN),
        },
        "3TrafficLights1" => {
            p.circle_filled(c, s * 0.42, [RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY));
        }
        "3TrafficLights2" => {
            p.rect_filled(Rect::from_center_size(c, vec2(s * 0.92, s * 0.92)), s * 0.22, BLACK);
            p.circle_filled(c, s * 0.3, [RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY));
        }
        "3Signs" => match idx {
            0 => {
                let d = s * 0.46;
                let pts = vec![pos2(c.x, c.y - d), pos2(c.x + d, c.y), pos2(c.x, c.y + d), pos2(c.x - d, c.y)];
                p.add(Shape::convex_polygon(pts, RED, Stroke::NONE));
            }
            1 => triangle(p, c, s, false, AMBER),
            _ => {
                p.circle_filled(c, s * 0.42, GREEN);
            }
        },
        "4TrafficLights" => {
            p.circle_filled(c, s * 0.42, [BLACK, RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY));
        }
        "4RedToBlack" => {
            p.circle_filled(c, s * 0.42, [BLACK, GRAY, PINK, RED].get(idx).copied().unwrap_or(GRAY));
        }
        "3Symbols" => {
            let col = [RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY);
            p.circle_filled(c, s * 0.46, col);
            symbol(p, c, s * 0.62, idx, Color32::WHITE);
        }
        "3Symbols2" => symbol(p, c, s, idx, [RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY)),
        "3Flags" => flag(p, c, s, [RED, AMBER, GREEN].get(idx).copied().unwrap_or(GRAY)),
        "3Stars" => star(p, c, s, idx),
        "4Rating" => bars(p, c, s, idx + 1),
        "5Rating" => bars(p, c, s, idx),
        "5Quarters" => quarters(p, c, s, idx),
        "5Boxes" => boxes(p, c, s, idx),
        _ => {
            // A set this build doesn't draw: red to green circles.
            let t = idx as f32 / n.saturating_sub(1).max(1) as f32;
            p.circle_filled(c, s * 0.42, lerp(RED, GREEN, t));
        }
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Rotates `v` (relative to the centre) clockwise by `deg`, on screen (y down).
fn rot(c: Pos2, x: f32, y: f32, deg: f32) -> Pos2 {
    let (sin, cos) = deg.to_radians().sin_cos();
    pos2(c.x + x * cos - y * sin, c.y + x * sin + y * cos)
}

/// An arrow pointing `deg` degrees clockwise from up: a head and a shaft.
fn arrow(p: &Painter, c: Pos2, s: f32, deg: f32, col: Color32) {
    let head = vec![rot(c, 0.0, -0.48 * s, deg), rot(c, 0.42 * s, -0.02 * s, deg), rot(c, -0.42 * s, -0.02 * s, deg)];
    let shaft =
        vec![rot(c, -0.16 * s, -0.06 * s, deg), rot(c, 0.16 * s, -0.06 * s, deg), rot(c, 0.16 * s, 0.46 * s, deg), rot(c, -0.16 * s, 0.46 * s, deg)];
    p.add(Shape::convex_polygon(head, col, Stroke::NONE));
    p.add(Shape::convex_polygon(shaft, col, Stroke::NONE));
}

fn triangle(p: &Painter, c: Pos2, s: f32, down: bool, col: Color32) {
    let (h, w) = (s * 0.4, s * 0.46);
    let pts = if down {
        vec![pos2(c.x - w, c.y - h * 0.8), pos2(c.x + w, c.y - h * 0.8), pos2(c.x, c.y + h)]
    } else {
        vec![pos2(c.x, c.y - h), pos2(c.x + w, c.y + h * 0.8), pos2(c.x - w, c.y + h * 0.8)]
    };
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// A cross (0), an exclamation mark (1) or a tick (2).
fn symbol(p: &Painter, c: Pos2, s: f32, idx: usize, col: Color32) {
    let st = Stroke::new((s * 0.17).max(1.0), col);
    let d = s * 0.3;
    match idx {
        0 => {
            p.line_segment([pos2(c.x - d, c.y - d), pos2(c.x + d, c.y + d)], st);
            p.line_segment([pos2(c.x + d, c.y - d), pos2(c.x - d, c.y + d)], st);
        }
        1 => {
            p.line_segment([pos2(c.x, c.y - s * 0.38), pos2(c.x, c.y + s * 0.1)], st);
            p.circle_filled(pos2(c.x, c.y + s * 0.3), st.width * 0.6, col);
        }
        _ => {
            p.line_segment([pos2(c.x - s * 0.34, c.y), pos2(c.x - s * 0.1, c.y + s * 0.26)], st);
            p.line_segment([pos2(c.x - s * 0.1, c.y + s * 0.26), pos2(c.x + s * 0.36, c.y - s * 0.3)], st);
        }
    }
}

fn flag(p: &Painter, c: Pos2, s: f32, col: Color32) {
    let x = c.x - s * 0.3;
    p.line_segment([pos2(x, c.y - s * 0.44), pos2(x, c.y + s * 0.46)], Stroke::new((s * 0.08).max(1.0), BLACK));
    let pts = vec![pos2(x + s * 0.04, c.y - s * 0.44), pos2(c.x + s * 0.42, c.y - s * 0.22), pos2(x + s * 0.04, c.y + s * 0.02)];
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// A five-pointed star: empty (0), half filled (1) or full (2).
fn star(p: &Painter, c: Pos2, s: f32, idx: usize) {
    let (outer, inner) = (s * 0.5, s * 0.21);
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let r = if i % 2 == 0 { outer } else { inner };
            rot(c, 0.0, -r, i as f32 * 36.0)
        })
        .collect();
    let fill = |p: &Painter| {
        for i in 0..10 {
            if let (Some(a), Some(b)) = (pts.get(i), pts.get((i + 1) % 10)) {
                p.add(Shape::convex_polygon(vec![c, *a, *b], GOLD, Stroke::NONE));
            }
        }
    };
    match idx {
        0 => {}
        1 => {
            let left = Rect::from_min_max(pos2(c.x - s, c.y - s), pos2(c.x, c.y + s));
            fill(&p.with_clip_rect(p.clip_rect().intersect(left)));
        }
        _ => fill(p),
    }
    p.add(Shape::closed_line(pts, Stroke::new((s * 0.06).max(1.0), GOLD)));
}

/// Four signal bars, the first `filled` of them coloured.
fn bars(p: &Painter, c: Pos2, s: f32, filled: usize) {
    let w = s * 0.18;
    let gap = s * 0.06;
    let left = c.x - (4.0 * w + 3.0 * gap) / 2.0;
    let bottom = c.y + s * 0.44;
    for i in 0..4 {
        let h = s * (0.3 + 0.2 * i as f32);
        let x = left + i as f32 * (w + gap);
        let r = Rect::from_min_max(pos2(x, bottom - h), pos2(x + w, bottom));
        p.rect_filled(r, 0.0, if i < filled { BLUE } else { PALE });
    }
}

/// A circle filled by quarters: empty (0) to full (4).
fn quarters(p: &Painter, c: Pos2, s: f32, idx: usize) {
    let r = s * 0.44;
    p.circle_filled(c, r, Color32::WHITE);
    let sweep = 90.0 * idx.min(4) as f32;
    let steps = (idx.min(4) * 8).max(1);
    for i in 0..steps {
        let a0 = sweep * i as f32 / steps as f32;
        let a1 = sweep * (i + 1) as f32 / steps as f32;
        if idx > 0 {
            p.add(Shape::convex_polygon(vec![c, rot(c, 0.0, -r, a0), rot(c, 0.0, -r, a1)], BLACK, Stroke::NONE));
        }
    }
    p.circle_stroke(c, r, Stroke::new((s * 0.07).max(1.0), BLACK));
}

/// Four boxes in a square, the first `filled` of them coloured (reading order).
fn boxes(p: &Painter, c: Pos2, s: f32, filled: usize) {
    let b = s * 0.4;
    let gap = s * 0.08;
    let origin = pos2(c.x - b - gap / 2.0, c.y - b - gap / 2.0);
    for i in 0..4 {
        let (col, row) = (i % 2, i / 2);
        let min = pos2(origin.x + col as f32 * (b + gap), origin.y + row as f32 * (b + gap));
        p.rect_filled(Rect::from_min_size(min, vec2(b, b)), 1.0, if i < filled { BLUE } else { PALE });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_lists_twenty_sets_with_the_icon_counts_their_names_give() {
        let sets: Vec<&str> = ICON_SET_GROUPS.iter().flat_map(|(_, s)| s.iter().map(|(id, _)| *id)).collect();
        assert_eq!(sets.len(), 20);
        for set in sets {
            let n = count(set);
            assert!((3..=5).contains(&n), "{set}");
            assert_eq!(set.chars().next().and_then(|c| c.to_digit(10)), Some(n as u32), "{set}");
        }
    }

    #[test]
    fn every_icon_paints_without_panicking() {
        let ctx = egui::Context::default();
        let painter = Painter::new(ctx, egui::LayerId::background(), Rect::from_min_size(Pos2::ZERO, vec2(100.0, 100.0)));
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(16.0, 16.0));
        for (_, sets) in ICON_SET_GROUPS {
            for (set, _) in sets {
                for idx in 0..7 {
                    paint(&painter, r, set, idx);
                }
            }
        }
        paint(&painter, r, "9Bogus", 3);
        paint(&painter, Rect::from_min_size(Pos2::ZERO, vec2(0.0, 0.0)), "3Stars", 1);
    }
}
