//! Design Ideas: arrangements of what is on the current slide (picture full-bleed with the title
//! on a band, picture on one half, a framed picture, text-only layouts). An idea moves and
//! resizes the slide's own shapes and crops pictures to their new box; it changes only that
//! slide. Everything is computed here, offline.

use deckcraft_geom::Xfrm;
use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::{Fill, Presentation, Rgba, Shape, ShapeKind};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_slide, opt_usize_param, str_param, xfrm_of};
use crate::{Result, Selection, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "design.ideas", "Design Ideas", [], None, "{slide?} → [{id, label}] ideas for the slide", has_slide, ideas_cmd),
        cmd!("design.applyIdea", "Apply Design Idea", ["Home", "Designer"], None, "{id, slide?}", has_slide, apply_cmd),
    ]
}

/// An idea: id and label.
pub type Idea = (&'static str, &'static str);

const PICTURE_IDEAS: [Idea; 4] = [
    ("fullBleed", "Full-bleed picture, title on a band"),
    ("pictureLeft", "Picture left, text right"),
    ("pictureRight", "Picture right, text left"),
    ("framed", "Framed picture beside the title"),
];
/// Colour ideas: any slide with text. They set the slide's background and the colour of its text.
const COLOR_IDEAS: [Idea; 3] =
    [("colorDark", "Dark background, light text"), ("colorLight", "Light background, dark text"), ("colorAccent", "Accent colour background")];
const TEXT_IDEAS: [Idea; 3] =
    [("titleColumn", "Title column beside the text"), ("centred", "Centred title over the text"), ("titleBottom", "Text above, title at the foot")];

/// The roles of a slide's top-level shapes, by index.
struct Roles {
    title: Option<usize>,
    picture: Option<usize>,
    text: Vec<usize>,
}

fn roles(shapes: &[Shape]) -> Roles {
    let mut r = Roles { title: None, picture: None, text: vec![] };
    for (i, s) in shapes.iter().enumerate() {
        if s.hidden {
            continue;
        }
        if s.ph.as_ref().is_some_and(|p| p.kind.is_title()) && r.title.is_none() {
            r.title = Some(i);
        } else if matches!(s.kind, ShapeKind::Picture { .. }) {
            // The largest picture leads.
            let area = |j: usize| shapes.get(j).and_then(|s| s.xfrm).map(|x| x.w * x.h).unwrap_or(0.0);
            if r.picture.is_none_or(|p| area(i) > area(p)) {
                r.picture = Some(i);
            }
        } else if s.text.as_ref().is_some_and(|t| !t.text().trim().is_empty()) {
            r.text.push(i);
        }
    }
    r
}

/// Whether `s` paints a fill or an outline once inherited and style-referenced values are
/// resolved, as the renderer does (`fill: None` alone means inherited, not invisible).
fn shows(ctx: Option<&Ctx>, s: &Shape) -> bool {
    let visible = |f: &Option<Fill>| f.as_ref().is_some_and(|f| !f.is_none());
    match ctx {
        Some(ctx) => visible(&resolve::fill(ctx, s).0) || visible(&resolve::line(ctx, s).0.fill),
        None => visible(&s.fill) || visible(&s.line.as_ref().and_then(|l| l.fill.clone())),
    }
}

/// Whether an idea may move this shape: a picture, a placeholder or a plain text box. Diagram
/// boxes, connectors, groups, tables, charts and media are laid out by their own geometry, which
/// the ideas do not know, so a slide holding one gets no ideas.
fn arrangeable(ctx: Option<&Ctx>, s: &Shape) -> bool {
    match s.kind {
        ShapeKind::Picture { .. } => true,
        ShapeKind::Shape => {
            let is_title = s.ph.as_ref().is_some_and(|p| p.kind.is_title());
            let has_text = s.text.as_ref().is_some_and(|t| !t.text().trim().is_empty());
            // Only titles and shapes with text are moved (see `roles`): an empty one that shows
            // (filled or outlined) would stay where it is, over the rearranged slide.
            (s.ph.is_some() || s.text_box) && (is_title || has_text || !shows(ctx, s))
        }
        _ => false,
    }
}

/// A filled or outlined shape without text (a band, a frame, a backdrop): decoration that a
/// layout idea may move along with the title, not something that rules the idea out.
fn decoration(s: &Shape) -> bool {
    matches!(s.kind, ShapeKind::Shape) && s.ph.is_none() && !s.text_box && s.text.as_ref().is_none_or(|t| t.text().trim().is_empty())
}

/// The layout ideas for slide `index` (none when it has nothing to arrange, or other content in the way).
fn layout_ideas(doc: &Presentation, index: usize) -> Vec<Idea> {
    let Some(slide) = doc.slides.get(index) else { return vec![] };
    let ctx = Ctx::for_slide(doc, slide);
    if slide.shapes.iter().any(|s| !s.hidden && !arrangeable(ctx.as_ref(), s) && !decoration(s)) {
        return vec![];
    }
    // The ideas move one picture; a second would stay put, over the rearranged text.
    if slide.shapes.iter().filter(|s| !s.hidden && matches!(s.kind, ShapeKind::Picture { .. })).count() > 1 {
        return vec![];
    }
    let r = roles(&slide.shapes);
    if r.picture.is_some() {
        PICTURE_IDEAS.to_vec()
    } else if r.title.is_some() && !r.text.is_empty() {
        TEXT_IDEAS.to_vec()
    } else {
        vec![]
    }
}

/// The ideas for slide `index`: layouts where its content allows, colours wherever it has text.
pub fn ideas_for(doc: &Presentation, index: usize) -> Vec<Idea> {
    let Some(slide) = doc.slides.get(index) else { return vec![] };
    let mut ideas = layout_ideas(doc, index);
    let mut has_text = false;
    deckcraft_model::walk(&slide.shapes, &mut |sh, _| has_text |= sh.text.as_ref().is_some_and(|t| !t.text().trim().is_empty()));
    if has_text {
        ideas.extend(COLOR_IDEAS);
    }
    ideas
}

/// Colour idea `id`: the slide's background, and every run of text in the colour that reads on it.
/// Text on a picture or on a filled shape keeps its colour (it was chosen for what is behind it).
fn apply_colors(doc: &mut Presentation, index: usize, id: &str) -> Result<()> {
    use deckcraft_model::{Background, ColorRef};
    let white = ColorRef::rgb(Rgba::rgb(255, 255, 255));
    let (bg, fg) = match id {
        "colorDark" => (ColorRef::rgb(Rgba::rgb(30, 34, 40)), white),
        "colorLight" => (white, ColorRef::rgb(Rgba::rgb(33, 33, 33))),
        "colorAccent" => (ColorRef::scheme(deckcraft_color::SchemeSlot::Accent1), white),
        _ => return Err(bad("design.applyIdea", format!("no idea `{id}`"))),
    };
    let slide = doc.slides.get_mut(index).ok_or_else(|| bad("design.applyIdea", "no slide"))?;
    let slide = std::sync::Arc::make_mut(slide);
    slide.background = Some(Background::Fill { fill: Fill::solid(bg) });
    fn recolor(shapes: &mut [Shape], fg: &ColorRef) {
        for sh in shapes {
            if let Some(kids) = sh.children_mut() {
                recolor(kids, fg);
                continue;
            }
            // A filled shape keeps its own text colour; so does text on a picture.
            if sh.fill.as_ref().is_some_and(|f| !f.is_none()) || matches!(sh.kind, ShapeKind::Picture { .. }) {
                continue;
            }
            if let Some(body) = sh.text.as_mut() {
                for p in &mut body.paragraphs {
                    for r in &mut p.runs {
                        r.props.fill = Some(Fill::solid(fg.clone()));
                    }
                }
            }
        }
    }
    recolor(&mut slide.shapes, &fg);
    Ok(())
}

/// Crop a picture whose source has aspect `src` (w/h) to fill a box of aspect `dst`, centred.
fn cover_crop(src: f64, dst: f64) -> [f64; 4] {
    if !(src.is_finite() && dst.is_finite() && src > 0.0 && dst > 0.0) {
        return [0.0; 4];
    }
    if src > dst {
        let cut = (1.0 - dst / src) / 2.0;
        [cut, 0.0, cut, 0.0]
    } else {
        let cut = (1.0 - src / dst) / 2.0;
        [0.0, cut, 0.0, cut]
    }
}

/// The light fill behind the title in the full-bleed idea. Its opacity is an `alpha` transform
/// (about 220/255), as PPTX stores it, so the fill compares equal after a save and reopen.
fn band_fill() -> Fill {
    Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgb(255, 255, 255)).with(deckcraft_model::ColorTransform::Alpha(86274)))
}

/// The band as released versions up to 0.10 stored it (opacity in the colour's own alpha).
fn legacy_band_fill() -> Fill {
    Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgba(255, 255, 255, 220)))
}

/// Whether `f` is a Full-bleed band, in the current or the legacy form.
fn is_band(f: &Fill) -> bool {
    *f == band_fill() || *f == legacy_band_fill()
}

/// Apply idea `id` to slide `index` of `doc` (`sel` resolves inherited placeholder boxes).
pub fn apply_idea(doc: &mut Presentation, sel: &Selection, index: usize, id: &str) -> Result<()> {
    if !ideas_for(doc, index).iter().any(|i| i.0 == id) {
        return Err(bad("design.applyIdea", format!("no idea `{id}` for this slide (see design.ideas)")));
    }
    if id.starts_with("color") {
        return apply_colors(doc, index, id);
    }
    let (sw, sh) = (doc.slide_size.width.max(1.0), doc.slide_size.height.max(1.0));
    let sel = Selection { slide: index, ..sel.clone() };
    let shapes = doc.slides.get(index).map(|s| s.shapes.clone()).ok_or_else(|| bad("design.applyIdea", "no slide"))?;
    let r = roles(&shapes);
    let boxes: Vec<Xfrm> = shapes.iter().map(|s| xfrm_of(doc, &sel, s)).collect();
    let m = sw.min(sh) * 0.06;
    let mut place: Vec<(usize, Xfrm)> = vec![];
    // Text shapes stacked in a box.
    let stack = |b: Xfrm, out: &mut Vec<(usize, Xfrm)>| {
        let n = r.text.len().max(1) as f64;
        let gap = m * 0.4;
        let h = ((b.h - gap * (n - 1.0)) / n).max(1.0);
        for (k, i) in r.text.iter().enumerate() {
            out.push((*i, Xfrm::new(b.x, b.y + k as f64 * (h + gap), b.w, h)));
        }
    };
    let mut band = false;
    match id {
        "fullBleed" => {
            if let Some(p) = r.picture {
                place.push((p, Xfrm::new(0.0, 0.0, sw, sh)));
            }
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(m, sh * 0.68, sw - 2.0 * m, sh * 0.2)));
                band = true;
            }
            stack(Xfrm::new(m, sh * 0.06, sw * 0.45, sh * 0.5), &mut place);
        }
        "pictureLeft" | "pictureRight" => {
            let left = id == "pictureLeft";
            let half = sw * 0.5;
            let (px, tx) = if left { (0.0, half + m) } else { (half, m) };
            if let Some(p) = r.picture {
                place.push((p, Xfrm::new(px, 0.0, half, sh)));
            }
            let tw = half - 2.0 * m;
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(tx, m, tw, sh * 0.24)));
            }
            stack(Xfrm::new(tx, m + sh * 0.28, tw, sh - 2.0 * m - sh * 0.28), &mut place);
        }
        "framed" => {
            if let Some(p) = r.picture {
                place.push((p, Xfrm::new(sw * 0.42, m * 1.5, sw * 0.58 - m * 1.5, sh - 3.0 * m)));
            }
            let tw = sw * 0.42 - 2.0 * m;
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(m, m * 1.5, tw, sh * 0.3)));
            }
            stack(Xfrm::new(m, m * 1.5 + sh * 0.34, tw, sh - 3.0 * m - sh * 0.34), &mut place);
        }
        "titleColumn" => {
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(m, m, sw * 0.32, sh - 2.0 * m)));
            }
            stack(Xfrm::new(sw * 0.36, m, sw * 0.64 - m, sh - 2.0 * m), &mut place);
        }
        "centred" => {
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(sw * 0.1, m, sw * 0.8, sh * 0.26)));
            }
            stack(Xfrm::new(sw * 0.18, m + sh * 0.3, sw * 0.64, sh - 2.0 * m - sh * 0.3), &mut place);
        }
        "titleBottom" => {
            stack(Xfrm::new(m, m, sw - 2.0 * m, sh * 0.62), &mut place);
            if let Some(t) = r.title {
                place.push((t, Xfrm::new(m, sh * 0.72, sw - 2.0 * m, sh * 0.2)));
            }
        }
        _ => return Err(bad("design.applyIdea", format!("no idea `{id}`"))),
    }
    // A decoration behind the title (a band, a box) moves with it, a little larger than the title;
    // it then replaces the full-bleed band.
    let mut backdrop = false;
    if let (Some(t), Some(new_t)) = (r.title, place.iter().find(|(i, _)| Some(*i) == r.title).map(|(_, x)| *x))
        && let Some(old_t) = boxes.get(t)
    {
        let (cx, cy) = (old_t.x + old_t.w / 2.0, old_t.y + old_t.h / 2.0);
        for (i, s) in shapes.iter().enumerate() {
            let Some(b) = boxes.get(i) else { continue };
            if !s.hidden && decoration(s) && b.x <= cx && cx <= b.x + b.w && b.y <= cy && cy <= b.y + b.h {
                let pad = m * 0.3;
                let x0 = (new_t.x - pad).max(0.0);
                let y0 = (new_t.y - pad).max(0.0);
                let x1 = (new_t.x + new_t.w + pad).min(sw);
                let y1 = (new_t.y + new_t.h + pad).min(sh);
                place.push((i, Xfrm::new(x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0))));
                backdrop = true;
            }
        }
    }
    if backdrop {
        band = false;
    }
    let slide = doc.slides.get_mut(index).ok_or_else(|| bad("design.applyIdea", "no slide"))?;
    let slide = std::sync::Arc::make_mut(slide);
    for (i, x) in place {
        let (Some(s), Some(old)) = (slide.shapes.get_mut(i), boxes.get(i)) else { continue };
        if let ShapeKind::Picture { fill } = &mut s.kind {
            // The source's aspect, from the current box and crop.
            let c = fill.crop;
            let (fw, fh) = ((1.0 - c[0] - c[2]).max(0.01), (1.0 - c[1] - c[3]).max(0.01));
            let src = (old.w / fw) / (old.h / fh).max(1e-6);
            fill.crop = cover_crop(src, x.w / x.h.max(1e-6));
        }
        s.xfrm = Some(Xfrm { rot: 0.0, ..x });
    }
    // The picture goes to the back, so text over it stays readable; decoration right above it,
    // so it never covers the rearranged text.
    let decor_ids: Vec<_> = slide.shapes.iter().filter(|s| !s.hidden && decoration(s)).map(|s| s.id).collect();
    if let Some(p) = r.picture
        && p < slide.shapes.len()
    {
        let pic = slide.shapes.remove(p);
        slide.shapes.insert(0, pic);
    }
    let first = usize::from(r.picture.is_some());
    for (k, id) in decor_ids.iter().enumerate() {
        if let Some(i) = slide.shapes.iter().position(|s| s.id == *id) {
            let d = slide.shapes.remove(i);
            slide.shapes.insert((first + k).min(slide.shapes.len()), d);
        }
    }
    if let Some(t) = slide.shapes.iter_mut().find(|s| !s.hidden && s.ph.as_ref().is_some_and(|p| p.kind.is_title())) {
        if band {
            t.fill = Some(band_fill());
        } else if t.fill.as_ref().is_some_and(is_band) {
            // The band belongs to full-bleed only: another idea takes it away again.
            t.fill = None;
        }
    }
    Ok(())
}

fn ideas_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.doc()?;
    let i = opt_usize_param(p, "slide", "design.ideas")?.unwrap_or(st.selection.slide);
    Ok(json!(ideas_for(&st.doc, i).iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()))
}

fn apply_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_param(p, "id").ok_or_else(|| bad("design.applyIdea", "missing `id`"))?.to_string();
    let slide = opt_usize_param(p, "slide", "design.applyIdea")?;
    s.edit(|doc, sel| {
        let i = slide.unwrap_or(sel.slide);
        apply_idea(doc, sel, i, &id)?;
        Ok(json!({"slide": i, "id": id}))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use deckcraft_model::style::PictureFill;
    use deckcraft_model::{PhType, Placeholder, ShapeId};

    use super::*;

    fn deck(picture: bool) -> Session {
        let mut s = Session::with_new();
        let st = s.doc_mut().unwrap();
        let doc = Arc::make_mut(&mut st.doc);
        let slide = Arc::make_mut(&mut doc.slides[0]);
        slide.shapes.clear();
        let title = Shape {
            id: ShapeId(500),
            ph: Some(Placeholder { kind: PhType::Title, ..Default::default() }),
            xfrm: Some(Xfrm::new(50.0, 30.0, 860.0, 90.0)),
            text: Some(deckcraft_model::text::TextBody::from_text("Harbour at dawn")),
            ..Default::default()
        };
        let body = Shape {
            id: ShapeId(501),
            text_box: true,
            xfrm: Some(Xfrm::new(50.0, 140.0, 860.0, 350.0)),
            text: Some(deckcraft_model::text::TextBody::from_text("one\ntwo")),
            ..Default::default()
        };
        slide.shapes.push(title);
        slide.shapes.push(body);
        if picture {
            // A 2:1 picture covering most of the slide, over the title.
            let pic = Shape {
                id: ShapeId(502),
                kind: ShapeKind::Picture { fill: PictureFill::default() },
                xfrm: Some(Xfrm::new(0.0, 0.0, 900.0, 450.0)),
                ..Default::default()
            };
            slide.shapes.push(pic);
        }
        s
    }

    fn shape(s: &Session, id: u32) -> Shape {
        s.doc().unwrap().doc.slides[0].shapes.iter().find(|x| x.id == ShapeId(id)).cloned().unwrap()
    }

    fn overlaps(a: Xfrm, b: Xfrm) -> bool {
        a.x < b.x + b.w - 0.5 && b.x < a.x + a.w - 0.5 && a.y < b.y + b.h - 0.5 && b.y < a.y + a.h - 0.5
    }

    #[test]
    fn picture_slides_get_layout_ideas_and_text_slides_get_text_ideas() {
        let s = deck(true);
        let ids: Vec<&str> = ideas_for(&s.doc().unwrap().doc, 0).iter().map(|i| i.0).collect();
        assert_eq!(ids, ["fullBleed", "pictureLeft", "pictureRight", "framed", "colorDark", "colorLight", "colorAccent"]);
        let s = deck(false);
        assert_eq!(ideas_for(&s.doc().unwrap().doc, 0).len(), 6);
        assert!(ideas_for(&s.doc().unwrap().doc, 9).is_empty());
    }

    #[test]
    fn picture_left_puts_text_beside_the_picture_cropped_to_fill() {
        let mut s = deck(true);
        let theme_before = s.doc().unwrap().doc.masters[0].theme.name.clone();
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        let (pic, title, body) = (shape(&s, 502), shape(&s, 500), shape(&s, 501));
        let px = pic.xfrm.unwrap();
        assert_eq!((px.x, px.y, px.w, px.h), (0.0, 0.0, 480.0, 540.0));
        assert!(!overlaps(px, title.xfrm.unwrap()) && !overlaps(px, body.xfrm.unwrap()));
        assert!(!overlaps(title.xfrm.unwrap(), body.xfrm.unwrap()));
        // 2:1 source into an 8:9 box: cut equally from left and right.
        let ShapeKind::Picture { fill } = &pic.kind else { panic!() };
        assert!(fill.crop[0] > 0.25 && (fill.crop[0] - fill.crop[2]).abs() < 1e-9 && fill.crop[1] == 0.0, "{:?}", fill.crop);
        // Only this slide's arrangement changed, not the theme; undo restores it.
        assert_eq!(s.doc().unwrap().doc.masters[0].theme.name, theme_before);
        assert_eq!(s.doc().unwrap().doc.slides[0].shapes[0].id, ShapeId(502), "picture at the back");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(shape(&s, 502).xfrm.unwrap().w, 900.0);
    }

    #[test]
    fn full_bleed_covers_the_slide_and_puts_the_title_on_a_band() {
        let mut s = deck(true);
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        let px = shape(&s, 502).xfrm.unwrap();
        assert_eq!((px.w, px.h), (960.0, 540.0));
        let t = shape(&s, 500);
        assert!(t.fill.is_some());
        assert!(t.xfrm.unwrap().y > 300.0);
    }

    #[test]
    fn every_idea_keeps_shapes_on_the_slide_and_unknown_ideas_are_refused() {
        for pic in [true, false] {
            let probe = deck(pic);
            for (id, _) in ideas_for(&probe.doc().unwrap().doc, 0) {
                let mut s = deck(pic);
                s.execute("design.applyIdea", &json!({"id": id})).unwrap();
                for sh in &s.doc().unwrap().doc.slides[0].shapes {
                    let x = sh.xfrm.unwrap();
                    assert!(x.x >= -0.01 && x.y >= -0.01 && x.x + x.w <= 960.01 && x.y + x.h <= 540.01 && x.w > 0.0 && x.h > 0.0, "{id}: {x:?}");
                }
            }
        }
        let mut s = deck(false);
        assert!(s.execute("design.applyIdea", &json!({"id": "fullBleed"})).is_err());
        assert!(s.execute("design.applyIdea", &json!({"id": "nope"})).is_err());
    }

    #[test]
    fn an_invalid_slide_index_is_refused_not_replaced_by_the_selection() {
        let mut s = deck(false);
        let before = s.doc().unwrap().doc.clone();
        for bad_slide in [json!(-1), json!(1.5), json!("0")] {
            assert!(s.execute("design.applyIdea", &json!({"id": "centred", "slide": bad_slide})).is_err(), "{bad_slide}");
            assert!(s.execute("design.ideas", &json!({"slide": bad_slide})).is_err(), "{bad_slide}");
        }
        assert!(Arc::ptr_eq(&before, &s.doc().unwrap().doc) || *before == *s.doc().unwrap().doc);
        assert!(s.execute("design.applyIdea", &json!({"id": "centred", "slide": 0})).is_ok());
        assert!(s.execute("design.ideas", &json!({})).is_ok());
    }

    /// A slide as PowerPoint decks often have it: a photo, a coloured band behind the title.
    fn deck_with_band() -> Session {
        let mut s = deck(true);
        let st = s.doc_mut().unwrap();
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut st.doc).slides[0]);
        let band = Shape {
            id: ShapeId(503),
            fill: Some(Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgb(200, 20, 20)))),
            xfrm: Some(Xfrm::new(0.0, 20.0, 960.0, 110.0)),
            ..Default::default()
        };
        slide.shapes.insert(1, band);
        s
    }

    #[test]
    fn a_band_behind_the_title_moves_with_it() {
        let s = deck_with_band();
        let ids: Vec<&str> = ideas_for(&s.doc().unwrap().doc, 0).iter().map(|i| i.0).collect();
        assert!(ids.contains(&"pictureLeft"), "a decorated slide still gets layout ideas: {ids:?}");
        for id in ["fullBleed", "pictureLeft", "pictureRight", "framed"] {
            let mut s = deck_with_band();
            s.execute("design.applyIdea", &json!({"id": id})).unwrap();
            let (t, b) = (shape(&s, 500).xfrm.unwrap(), shape(&s, 503).xfrm.unwrap());
            assert!(b.x <= t.x && b.y <= t.y && b.x + b.w >= t.x + t.w - 0.01 && b.y + b.h >= t.y + t.h - 0.01, "{id}: band {b:?} title {t:?}");
            assert!(shape(&s, 500).fill.as_ref().is_none_or(|f| !is_band(f)), "{id}: the slide's own band replaces the white one");
        }
    }

    #[test]
    fn colour_ideas_change_background_and_text_but_not_text_on_filled_shapes() {
        let mut s = deck_with_band();
        s.execute("design.applyIdea", &json!({"id": "colorDark"})).unwrap();
        let slide = s.doc().unwrap().doc.slides[0].clone();
        assert!(slide.background.is_some());
        let body = shape(&s, 501);
        assert!(body.text.unwrap().paragraphs[0].runs[0].props.fill.is_some(), "text on the slide is recoloured");
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.doc().unwrap().doc.slides[0].background.is_none());
    }

    #[test]
    fn switching_from_full_bleed_removes_the_title_band() {
        let mut s = deck(true);
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        assert_eq!(shape(&s, 500).fill, Some(band_fill()));
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        assert!(shape(&s, 500).fill.is_none(), "the band belongs to full-bleed only");
    }

    #[test]
    fn the_band_goes_on_the_visible_title_not_a_hidden_one_before_it() {
        let mut s = deck(true);
        let st = s.doc_mut().unwrap();
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut st.doc).slides[0]);
        let hidden = Shape {
            id: ShapeId(503),
            hidden: true,
            ph: Some(Placeholder { kind: PhType::Title, ..Default::default() }),
            xfrm: Some(Xfrm::new(50.0, 30.0, 860.0, 90.0)),
            text: Some(deckcraft_model::text::TextBody::from_text("Old title")),
            ..Default::default()
        };
        slide.shapes.insert(0, hidden);
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        assert_eq!(shape(&s, 500).fill, Some(band_fill()), "visible title gets the band");
        assert!(shape(&s, 503).fill.is_none(), "hidden title is untouched");
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        assert!(shape(&s, 500).fill.is_none(), "switching clears the visible title's band");
        assert!(shape(&s, 503).fill.is_none());
    }

    #[test]
    fn switching_ideas_removes_the_band_after_a_pptx_round_trip() {
        let mut s = deck(true);
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        let bytes = deckcraft_pptx::export(&s.doc().unwrap().doc).unwrap();
        let back = deckcraft_pptx::import(&bytes).unwrap();
        s.doc_mut().unwrap().doc = Arc::new(back);
        let title = s.doc().unwrap().doc.slides[0].shapes.iter().position(|x| x.ph.as_ref().is_some_and(|p| p.kind.is_title())).unwrap();
        let id = s.doc().unwrap().doc.slides[0].shapes[title].id.0;
        assert_eq!(shape(&s, id).fill, Some(band_fill()), "band survives the save");
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        assert!(shape(&s, id).fill.is_none(), "the band is removed after reopening");
    }

    #[test]
    fn switching_ideas_removes_a_band_saved_by_earlier_versions() {
        let mut s = deck(true);
        let legacy = Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgba(255, 255, 255, 220)));
        let st = s.doc_mut().unwrap();
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut st.doc).slides[0]);
        slide.shapes.iter_mut().find(|x| x.id == ShapeId(500)).unwrap().fill = Some(legacy);
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        assert!(shape(&s, 500).fill.is_none());
    }

    #[test]
    fn a_title_fill_set_by_the_user_survives_other_ideas() {
        // Set through the command, and set to exactly the band's own fill.
        let mut s = deck(true);
        s.execute("shape.fill", &json!({"id": 500, "color": "#ffffff", "transparency": 0.14})).unwrap();
        let user = shape(&s, 500).fill;
        assert!(user.is_some());
        s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).unwrap();
        assert_eq!(shape(&s, 500).fill, user);
    }

    #[test]
    fn full_bleed_twice_keeps_the_band_and_undo_after_switching_brings_it_back() {
        let mut s = deck(true);
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        s.execute("design.applyIdea", &json!({"id": "fullBleed"})).unwrap();
        assert_eq!(shape(&s, 500).fill, Some(band_fill()));
        s.execute("design.applyIdea", &json!({"id": "framed"})).unwrap();
        assert!(shape(&s, 500).fill.is_none());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(shape(&s, 500).fill, Some(band_fill()));
    }

    #[test]
    fn slides_with_diagrams_or_other_graphics_get_no_ideas() {
        // A filled autoshape with text (a diagram box) is not text to restack.
        let mut s = deck(false);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes.push(Shape {
            id: ShapeId(510),
            xfrm: Some(Xfrm::new(300.0, 200.0, 200.0, 80.0)),
            fill: Some(Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgba(200, 80, 20, 255)))),
            text: Some(deckcraft_model::text::TextBody::from_text("Solar wind")),
            ..Default::default()
        });
        assert!(layout_ideas(&s.doc().unwrap().doc, 0).is_empty());
        assert!(s.execute("design.applyIdea", &json!({"id": "titleColumn"})).is_err());
        // A connector too, and a hidden one does not count.
        let mut s = deck(false);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes.push(Shape {
            id: ShapeId(511),
            kind: ShapeKind::Connector { start: None, end: None },
            xfrm: Some(Xfrm::new(300.0, 200.0, 100.0, 0.0)),
            ..Default::default()
        });
        assert!(layout_ideas(&s.doc().unwrap().doc, 0).is_empty());
        Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]).shapes[2].hidden = true;
        assert_eq!(layout_ideas(&s.doc().unwrap().doc, 0).len(), 3);
    }

    #[test]
    fn two_visible_pictures_get_no_ideas_but_a_hidden_second_one_does_not_count() {
        let mut s = deck(true);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes.push(Shape {
            id: ShapeId(503),
            kind: ShapeKind::Picture { fill: PictureFill::default() },
            xfrm: Some(Xfrm::new(600.0, 300.0, 200.0, 100.0)),
            ..Default::default()
        });
        assert!(layout_ideas(&s.doc().unwrap().doc, 0).is_empty());
        assert!(s.execute("design.applyIdea", &json!({"id": "pictureLeft"})).is_err());
        Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]).shapes[3].hidden = true;
        assert_eq!(layout_ideas(&s.doc().unwrap().doc, 0).len(), 4);
    }

    #[test]
    fn an_empty_text_box_blocks_ideas_only_when_it_shows() {
        let mut s = deck(false);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes.push(Shape { id: ShapeId(512), text_box: true, xfrm: Some(Xfrm::new(10.0, 10.0, 100.0, 40.0)), ..Default::default() });
        assert_eq!(layout_ideas(&s.doc().unwrap().doc, 0).len(), 3, "invisible and empty: nothing in the way");
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes[2].fill = Some(Fill::solid(deckcraft_model::ColorRef::rgb(Rgba::rgba(200, 80, 20, 255))));
        assert!(layout_ideas(&s.doc().unwrap().doc, 0).is_empty(), "a filled empty box would stay on top");
    }

    #[test]
    fn an_empty_box_with_an_explicit_no_fill_does_not_block_ideas() {
        let mut s = deck(false);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        slide.shapes.push(Shape {
            id: ShapeId(512),
            text_box: true,
            xfrm: Some(Xfrm::new(10.0, 10.0, 100.0, 40.0)),
            fill: Some(Fill::None),
            ..Default::default()
        });
        assert_eq!(layout_ideas(&s.doc().unwrap().doc, 0).len(), 3, "an explicit no fill shows nothing");
    }

    #[test]
    fn an_empty_box_filled_through_its_style_blocks_ideas() {
        let mut s = deck(false);
        let slide = Arc::make_mut(&mut Arc::make_mut(&mut s.doc_mut().unwrap().doc).slides[0]);
        let mut style = deckcraft_model::style::ShapeStyle::accent(deckcraft_model::SchemeSlot::Accent1);
        style.line_ref.0 = 0;
        slide.shapes.push(Shape {
            id: ShapeId(512),
            text_box: true,
            xfrm: Some(Xfrm::new(10.0, 10.0, 100.0, 40.0)),
            style: Some(style),
            ..Default::default()
        });
        assert!(layout_ideas(&s.doc().unwrap().doc, 0).is_empty(), "the theme fill shows, so the box would stay on top");
    }

    #[test]
    fn cover_crop_matches_the_box_aspect() {
        let c = cover_crop(2.0, 1.0);
        assert!((c[0] - 0.25).abs() < 1e-12 && c[1] == 0.0);
        let c = cover_crop(1.0, 2.0);
        assert!((c[1] - 0.25).abs() < 1e-12 && c[0] == 0.0);
        assert_eq!(cover_crop(f64::NAN, 1.0), [0.0; 4]);
    }
}
