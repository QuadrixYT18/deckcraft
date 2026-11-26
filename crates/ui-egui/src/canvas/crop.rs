//! Crop mode (Picture Format › Crop): black handles on the picture's edges and corners; dragging
//! one moves that edge, so the picture is cut to any rectangle (or opened up again, up to the
//! whole image). The image stays where it is. The whole image's outline and the part cut away
//! show while cropping. Releasing a handle runs `picture.cropTo` (one undo step); Esc or Enter,
//! or selecting something else, ends the mode.

use deckcraft_geom::Xfrm;
use deckcraft_model::{ShapeId, ShapeKind};
use egui::{Color32, CursorIcon, Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use super::Xf;
use crate::SlideApp;

/// Which edges a handle moves: (left, top, right, bottom).
const HANDLES: [(bool, bool, bool, bool); 8] = [
    (true, true, false, false),
    (false, true, false, false),
    (false, true, true, false),
    (false, false, true, false),
    (false, false, true, true),
    (false, false, false, true),
    (true, false, false, true),
    (true, false, false, false),
];

/// The smallest piece a crop keeps, in points.
const MIN_SIDE: f64 = 4.0;

/// The picture crop mode works on, while it is still the selected, unrotated picture.
fn target(app: &SlideApp, id: ShapeId) -> Option<(Xfrm, [f64; 4])> {
    let st = app.session.active()?;
    if st.selection.shapes != [id] {
        return None;
    }
    let sh = st.shape(id)?;
    let ShapeKind::Picture { fill } = &sh.kind else { return None };
    let b = sh.xfrm?;
    (b.rot.abs() <= 0.01).then_some((b, fill.crop))
}

fn screen(xf: Xf, r: Xfrm) -> Rect {
    Rect::from_min_max(xf.to_screen(deckcraft_geom::Point::new(r.x, r.y)), xf.to_screen(deckcraft_geom::Point::new(r.x + r.w, r.y + r.h)))
}

/// Draw and run crop mode. Returns whether it is on (then the caller skips its own handles).
pub(super) fn crop_mode(app: &mut SlideApp, ui: &mut Ui, painter: &Painter, xf: Xf) -> bool {
    let Some(id) = app.ui.crop else { return false };
    let Some((b, crop)) = target(app, id) else {
        app.ui.crop = None;
        return false;
    };
    if ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter)) {
        app.ui.crop = None;
        return false;
    }
    let full = deckcraft_engine::tools::picture_full_rect(b, crop);
    let pending_id = egui::Id::unique(("crop-pending", id.0));
    let pending: Option<Xfrm> = ui.data(|d| d.get_temp(pending_id));
    let cur = pending.unwrap_or(b);
    let (full_s, cur_s) = (screen(xf, full), screen(xf, cur));
    // The part of the image that is (or will be) cut away, shaded; the whole image outlined.
    let shade = Color32::from_black_alpha(90);
    for r in [
        Rect::from_min_max(full_s.min, pos2(full_s.max.x, cur_s.min.y)),
        Rect::from_min_max(pos2(full_s.min.x, cur_s.max.y), full_s.max),
        Rect::from_min_max(pos2(full_s.min.x, cur_s.min.y), pos2(cur_s.min.x, cur_s.max.y)),
        Rect::from_min_max(pos2(cur_s.max.x, cur_s.min.y), pos2(full_s.max.x, cur_s.max.y)),
    ] {
        if r.is_positive() {
            painter.rect_filled(r, 0.0, shade);
        }
    }
    painter.rect_stroke(full_s, 0.0, Stroke::new(1.0, Color32::from_gray(150)), egui::StrokeKind::Middle);
    painter.rect_stroke(cur_s, 0.0, Stroke::new(1.0, Color32::from_gray(40)), egui::StrokeKind::Middle);
    let mut next = cur;
    let mut released = false;
    for (k, (l, t, r, bo)) in HANDLES.iter().enumerate() {
        let x = if *l {
            cur_s.min.x
        } else if *r {
            cur_s.max.x
        } else {
            cur_s.center().x
        };
        let y = if *t {
            cur_s.min.y
        } else if *bo {
            cur_s.max.y
        } else {
            cur_s.center().y
        };
        let at = pos2(x, y);
        let hit = Rect::from_center_size(at, vec2(18.0, 18.0));
        let resp = ui.interact(hit, egui::Id::unique(("crop-handle", id.0, k)), Sense::drag());
        paint_handle(painter, at, (*l, *t, *r, *bo));
        let cursor = match (l | r, t | bo) {
            (true, false) => CursorIcon::ResizeHorizontal,
            (false, true) => CursorIcon::ResizeVertical,
            _ if (*l && *t) || (*r && *bo) => CursorIcon::ResizeNwSe,
            _ => CursorIcon::ResizeNeSw,
        };
        let resp = resp.on_hover_cursor(cursor);
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let q = xf.to_slide(p);
            let (mut x0, mut y0, mut x1, mut y1) = (next.x, next.y, next.x + next.w, next.y + next.h);
            if *l {
                x0 = q.x.clamp(full.x, x1 - MIN_SIDE);
            }
            if *r {
                x1 = q.x.clamp(x0 + MIN_SIDE, full.x + full.w);
            }
            if *t {
                y0 = q.y.clamp(full.y, y1 - MIN_SIDE);
            }
            if *bo {
                y1 = q.y.clamp(y0 + MIN_SIDE, full.y + full.h);
            }
            next = Xfrm { x: x0, y: y0, w: x1 - x0, h: y1 - y0, ..next };
        }
        released |= resp.drag_stopped();
    }
    if released {
        ui.data_mut(|d| d.remove::<Xfrm>(pending_id));
        let r = json!({"ids": [id.0], "rect": [next.x, next.y, next.w, next.h]});
        if let Err(e) = app.run("picture.cropTo", r) {
            app.set_status(e);
        }
    } else if next != cur || pending.is_some() {
        ui.data_mut(|d| d.insert_temp(pending_id, next));
    }
    true
}

/// A crop handle: a black bar on an edge, or an L at a corner, outlined in white.
fn paint_handle(p: &Painter, at: Pos2, (l, t, r, b): (bool, bool, bool, bool)) {
    let (len, thick) = (12.0, 4.0);
    let mut bars: Vec<Rect> = vec![];
    let horizontal = |sx: f32| Rect::from_min_size(pos2(if sx < 0.0 { at.x - len } else { at.x }, at.y - thick / 2.0), vec2(len, thick));
    let vertical = |sy: f32| Rect::from_min_size(pos2(at.x - thick / 2.0, if sy < 0.0 { at.y - len } else { at.y }), vec2(thick, len));
    match (l || r, t || b) {
        (true, true) => {
            bars.push(horizontal(if l { 1.0 } else { -1.0 }));
            bars.push(vertical(if t { 1.0 } else { -1.0 }));
        }
        (true, false) => bars.push(Rect::from_center_size(at, vec2(thick, len * 1.6))),
        _ => bars.push(Rect::from_center_size(at, vec2(len * 1.6, thick))),
    }
    for r in bars {
        p.rect_filled(r.expand(1.0), 1.0, Color32::WHITE);
        p.rect_filled(r, 0.0, Color32::BLACK);
    }
}
