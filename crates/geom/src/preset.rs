//! Preset shape geometry: DrawingML's from craft-drawingml, with DeckCraft's own outlines for the
//! shapes whose shared outline is still a rough stand-in (the scrolls: a body with a roll at the
//! top and one at the bottom, the curl of each roll shaded).

pub use craft_drawingml::preset::*;
use kurbo::BezPath;

/// The preset `name` at `w` × `h` points with adjust values `adj` (DrawingML's 1/100 000 units).
pub fn build(name: &str, w: f64, h: f64, adj: &[f64]) -> Option<Geometry> {
    let mut g = craft_drawingml::preset::build(name, w, h, adj)?;
    if matches!(name, "verticalScroll" | "horizontalScroll") && w > 0.0 && h > 0.0 {
        let a = adj.first().copied().unwrap_or(12_500.0).clamp(0.0, 25_000.0);
        g.paths = scroll(name == "verticalScroll", w, h, a);
    }
    Some(g)
}

/// Path pen with DrawingML's `arcTo`: angles in degrees, clockwise (y down), circular arcs.
struct Pen {
    p: BezPath,
    at: kurbo::Point,
}

impl Pen {
    fn new() -> Self {
        Pen { p: BezPath::new(), at: kurbo::Point::ZERO }
    }
    fn mv(&mut self, x: f64, y: f64) {
        self.at = kurbo::Point::new(x, y);
        self.p.move_to(self.at);
    }
    fn ln(&mut self, x: f64, y: f64) {
        self.at = kurbo::Point::new(x, y);
        self.p.line_to(self.at);
    }
    /// An arc of radius `r` that starts at the current point, at angle `st` on its circle, and
    /// turns through `sw` degrees.
    fn arc(&mut self, r: f64, st: f64, sw: f64) {
        let (st, sw) = (st.to_radians(), sw.to_radians());
        let c = kurbo::Point::new(self.at.x - r * st.cos(), self.at.y - r * st.sin());
        let arc = kurbo::Arc::new(c, (r, r), st, sw, 0.0);
        for el in arc.append_iter(0.05) {
            self.p.push(el);
        }
        let end = st + sw;
        self.at = kurbo::Point::new(c.x + r * end.cos(), c.y + r * end.sin());
    }
    fn close(&mut self) {
        self.p.close_path();
    }
}

/// A scroll after the standard's preset definition: the sheet with a roll across the top (to the
/// right edge) and one at the bottom left, the inside of each curl shaded. The horizontal one is
/// the vertical one turned on its side.
fn scroll(vertical: bool, w: f64, h: f64, a: f64) -> Vec<SubPath> {
    // Built upright in (u, v) = across × along the sheet, then mapped to x/y.
    let (r, b) = if vertical { (w, h) } else { (h, w) };
    let ch = r.min(b) * a / 100_000.0;
    let (ch2, ch4) = (ch / 2.0, ch / 4.0);
    let (x3, x4, x6, x7) = (ch + ch2, ch + ch, r - ch, r - ch2);
    let (y3, y4) = (b - ch, b - ch2);

    let mut body = Pen::new();
    body.mv(ch2, b);
    body.arc(ch2, 90.0, -90.0);
    body.ln(ch2, y4);
    body.arc(ch4, 90.0, -180.0);
    body.ln(ch, y3);
    body.ln(ch, ch2);
    body.arc(ch2, 180.0, 90.0);
    body.ln(x7, 0.0);
    body.arc(ch2, 270.0, 180.0);
    body.ln(x6, ch);
    body.ln(x6, y4);
    body.arc(ch2, 0.0, 90.0);
    body.close();
    body.mv(x4, ch2);
    body.arc(ch2, 0.0, 90.0);
    body.arc(ch4, 90.0, 180.0);
    body.close();

    let mut curls = Pen::new();
    curls.mv(x4, ch2);
    curls.arc(ch2, 0.0, 90.0);
    curls.arc(ch4, 90.0, 180.0);
    curls.close();
    curls.mv(ch, y4);
    curls.arc(ch2, 0.0, 270.0);
    curls.arc(ch4, 270.0, 180.0);
    curls.close();

    let mut lines = Pen::new();
    lines.mv(ch, y3);
    lines.ln(ch, ch2);
    lines.arc(ch2, 180.0, 90.0);
    lines.ln(x7, 0.0);
    lines.arc(ch2, 270.0, 180.0);
    lines.ln(x6, ch);
    lines.ln(x6, y4);
    lines.arc(ch2, 0.0, 90.0);
    lines.ln(ch2, b);
    lines.arc(ch2, 90.0, 180.0);
    lines.close();
    lines.mv(x3, 0.0);
    lines.arc(ch2, 270.0, 180.0);
    lines.arc(ch4, 90.0, 180.0);
    lines.ln(x4, ch2);
    lines.mv(x6, ch);
    lines.ln(x3, ch);
    lines.mv(ch2, y3);
    lines.arc(ch4, 270.0, 180.0);
    lines.ln(ch, y4);
    lines.mv(ch2, b);
    lines.arc(ch2, 90.0, -90.0);
    lines.ln(ch, y3);

    let map = |p: BezPath| -> BezPath {
        if vertical {
            return p;
        }
        let sw = |q: kurbo::Point| kurbo::Point::new(q.y, q.x);
        p.iter()
            .map(|el| match el {
                kurbo::PathEl::MoveTo(p) => kurbo::PathEl::MoveTo(sw(p)),
                kurbo::PathEl::LineTo(p) => kurbo::PathEl::LineTo(sw(p)),
                kurbo::PathEl::QuadTo(a, b) => kurbo::PathEl::QuadTo(sw(a), sw(b)),
                kurbo::PathEl::CurveTo(a, b, d) => kurbo::PathEl::CurveTo(sw(a), sw(b), sw(d)),
                kurbo::PathEl::ClosePath => kurbo::PathEl::ClosePath,
            })
            .collect()
    };
    vec![
        SubPath { path: map(body.p), fill: FillMode::Norm, stroke: false, even_odd: false },
        SubPath { path: map(curls.p), fill: FillMode::DarkenLess, stroke: false, even_odd: false },
        SubPath { path: map(lines.p), fill: FillMode::None, stroke: true, even_odd: false },
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
