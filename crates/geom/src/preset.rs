//! Preset shape geometry: DrawingML's from craft-drawingml, with DeckCraft's own outlines for the
//! shapes whose shared outline is still a rough stand-in (the scrolls: a body with a roll at the
//! top and one at the bottom, the curl of each roll shaded).

pub use craft_drawingml::preset::*;
use kurbo::{BezPath, Circle, RoundedRect, Shape as _};

/// The preset `name` at `w` × `h` points with adjust values `adj` (DrawingML's 1/100 000 units).
pub fn build(name: &str, w: f64, h: f64, adj: &[f64]) -> Option<Geometry> {
    let mut g = craft_drawingml::preset::build(name, w, h, adj)?;
    if matches!(name, "verticalScroll" | "horizontalScroll") && w > 0.0 && h > 0.0 {
        let a = adj.first().copied().unwrap_or(12_500.0).clamp(0.0, 25_000.0);
        g.paths = scroll(name == "verticalScroll", w, h, a);
    }
    Some(g)
}

/// A scroll: the sheet, a roll across the top reaching to the right edge and a roll across the
/// bottom reaching to the left edge, each with its curl shaded. The horizontal one is the
/// vertical one turned on its side (rolls at the left and right).
fn scroll(vertical: bool, w: f64, h: f64, a: f64) -> Vec<SubPath> {
    // Built upright in (u, v) = across × along the sheet, then mapped to x/y.
    let (uw, vh) = if vertical { (w, h) } else { (h, w) };
    let c = uw.min(vh) * a / 100_000.0;
    let c2 = c / 2.0;
    let map = |p: BezPath| -> BezPath {
        if vertical {
            p
        } else {
            // Swap the axes: the top roll becomes the left one, the bottom roll the right one.
            p.iter()
                .map(|el| {
                    let sw = |q: kurbo::Point| kurbo::Point::new(q.y, q.x);
                    match el {
                        kurbo::PathEl::MoveTo(p) => kurbo::PathEl::MoveTo(sw(p)),
                        kurbo::PathEl::LineTo(p) => kurbo::PathEl::LineTo(sw(p)),
                        kurbo::PathEl::QuadTo(a, b) => kurbo::PathEl::QuadTo(sw(a), sw(b)),
                        kurbo::PathEl::CurveTo(a, b, d) => kurbo::PathEl::CurveTo(sw(a), sw(b), sw(d)),
                        kurbo::PathEl::ClosePath => kurbo::PathEl::ClosePath,
                    }
                })
                .collect()
        }
    };
    let part = |p: BezPath, fill: FillMode| SubPath { path: map(p), fill, stroke: true, even_odd: false };
    // The sheet runs from under the top roll to the bottom edge, its bottom-right corner rounded.
    let sheet = RoundedRect::new(c, c2, uw - c, vh, kurbo::RoundedRectRadii::new(0.0, 0.0, c2, 0.0));
    let top = RoundedRect::new(c - c2, 0.0, uw, c, c2);
    vec![
        part(sheet.to_path(0.1), FillMode::Norm),
        part(top.to_path(0.1), FillMode::Norm),
        // The bottom roll: the sheet curling back at its bottom-left corner.
        part(kurbo::Rect::new(c2, vh - c, c + 0.5, vh).to_path(0.1), FillMode::Norm),
        part(Circle::new((c2, vh - c2), c2).to_path(0.1), FillMode::Norm),
        // The curls: where each roll turns into the sheet.
        part(Circle::new((c, c2 + c / 4.0), c / 4.0).to_path(0.1), FillMode::DarkenLess),
        part(Circle::new((c2 + c2 / 4.0, vh - c2), c2 * 0.55).to_path(0.1), FillMode::DarkenLess),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_scroll_has_its_sheet_and_both_rolls_inside_the_box() {
        for name in ["verticalScroll", "horizontalScroll"] {
            let g = super::build(name, 200.0, 300.0, &[]).unwrap_or_else(|| panic!("{name}"));
            assert!(g.paths.len() >= 3, "{name}: sheet and two rolls");
            for p in &g.paths {
                let b = kurbo::Shape::bounding_box(&p.path);
                assert!(b.x0 >= -0.01 && b.y0 >= -0.01 && b.x1 <= 200.01 && b.y1 <= 300.01, "{name}: {b:?}");
            }
            let all = g.paths.iter().map(|p| kurbo::Shape::bounding_box(&p.path)).reduce(|a, b| a.union(b)).unwrap_or_default();
            assert!(all.width() > 199.0 && all.height() > 299.0, "{name} fills its box: {all:?}");
        }
    }
}
