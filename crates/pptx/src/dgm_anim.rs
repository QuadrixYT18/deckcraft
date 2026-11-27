//! Animations on the parts of a SmartArt graphic ("One by one"): PowerPoint targets each part with
//! `p:spTgt/p:graphicEl/a:dgm@id`, the `modelId` of that part's shape in the diagram's drawing.
//! The shared parser keeps only the graphic frame; this pass points each such effect at the part's
//! own shape in the imported group, so the parts come in one after another.

use std::collections::HashMap;

use craft_xml::{El, Local};
use deckcraft_model::{Shape, ShapeId, ShapeKind, Slide};

/// Each effect's target in document order: (shape id, diagram part id).
fn effect_targets(e: &El, out: &mut Vec<(u32, Option<String>)>, depth: usize) {
    if depth > 64 {
        return;
    }
    for c in e.elements() {
        if c.is(Local("cTn")) && c.attr_local("presetClass").is_some() {
            if let Some(sp) = c.find(Local("spTgt"))
                && let Some(spid) = sp.attr_local("spid").and_then(craft_xml::value::to_u32_lossy)
            {
                let part = sp.path(&[Local("graphicEl"), Local("dgm")]).and_then(|d| d.attr_local("id")).map(str::to_string);
                out.push((spid, part));
            }
            continue;
        }
        effect_targets(c, out, depth + 1);
    }
}

/// The graphic frames on the slide (any depth), by shape id, with their data-model relationship.
fn frames(e: &El, out: &mut HashMap<u32, String>, depth: usize) {
    if depth > 64 {
        return;
    }
    for c in e.elements() {
        if c.is(Local("graphicFrame")) {
            let id = c.find(Local("cNvPr")).and_then(|n| n.attr_local("id")).and_then(craft_xml::value::to_u32_lossy);
            let dm = c.find(Local("relIds")).and_then(|r| r.attr_local("r:dm")).map(str::to_string);
            if let (Some(id), Some(dm)) = (id, dm) {
                out.insert(id, dm);
            }
            continue;
        }
        frames(c, out, depth + 1);
    }
}

/// A diagram drawing's shapes in drawing order: each `modelId` with the boxes (points, in the
/// drawing's space) its shape and its text sit in.
fn drawing_parts(e: &El, out: &mut Vec<(String, Vec<[f64; 4]>)>, depth: usize) {
    if depth > 64 {
        return;
    }
    let rect = |x: &El| -> Option<[f64; 4]> {
        let off = x.child(Local("off"))?;
        let ext = x.child(Local("ext"))?;
        let n = |el: &El, k: &str| el.attr_local(k).and_then(|v| v.parse::<f64>().ok()).map(|v| v / 12_700.0);
        Some([n(off, "x")?, n(off, "y")?, n(ext, "cx")?, n(ext, "cy")?])
    };
    for c in e.elements() {
        if c.is(Local("sp")) {
            let mut boxes = vec![];
            if let Some(r) = c.child(Local("spPr")).and_then(|p| p.child(Local("xfrm"))).and_then(rect) {
                boxes.push(r);
            }
            if let Some(r) = c.child(Local("txXfrm")).and_then(rect) {
                boxes.push(r);
            }
            out.push((c.attr_local("modelId").unwrap_or("").to_string(), boxes));
            continue;
        }
        drawing_parts(c, out, depth + 1);
    }
}

fn find_group(shapes: &[Shape], id: u32) -> Option<&Shape> {
    shapes.iter().find(|s| s.id.0 == id && s.diagram.is_some() && matches!(s.kind, ShapeKind::Group { .. }))
}

/// Point the slide's SmartArt part effects at the parts' shapes. `read` reads a part by its
/// package path; `slide_rels` resolves the slide's relationship ids.
pub(crate) fn retarget(
    slide: &mut Slide,
    timing: &El,
    slide_xml: &El,
    slide_rels: &dyn Fn(&str) -> Option<String>,
    read: &mut dyn FnMut(&str) -> Option<El>,
) {
    let mut targets = vec![];
    effect_targets(timing, &mut targets, 0);
    if targets.len() != slide.animations.len() || targets.iter().zip(&slide.animations).any(|((spid, _), a)| *spid != a.shape.0) {
        return;
    }
    if targets.iter().all(|(_, part)| part.is_none()) {
        return;
    }
    let mut by_frame = HashMap::new();
    frames(slide_xml, &mut by_frame, 0);
    // modelId → part shape, per diagram frame.
    let mut parts: HashMap<u32, HashMap<String, Vec<ShapeId>>> = HashMap::new();
    for spid in targets.iter().filter(|(_, p)| p.is_some()).map(|(s, _)| *s) {
        if parts.contains_key(&spid) {
            continue;
        }
        let Some(ShapeKind::Group { children, .. }) = find_group(&slide.shapes, spid).map(|g| &g.kind) else { continue };
        let Some(data_path) = by_frame.get(&spid).and_then(|dm| slide_rels(dm)) else { continue };
        let Some(data) = read(&data_path) else { continue };
        let Some(drawing_rel) = data.find(Local("dataModelExt")).and_then(|x| x.attr_local("relId")).map(str::to_string) else { continue };
        let Some(drawing) = slide_rels(&drawing_rel).and_then(|p| read(&p)) else { continue };
        let mut drawn = vec![];
        drawing_parts(&drawing, &mut drawn, 0);
        // Each part's shapes: the group's children that sit where the part or its text does
        // (the import may give a part's text a shape of its own).
        let near = |a: &[f64; 4], c: &Shape| {
            c.xfrm.is_some_and(|x| (x.x - a[0]).abs() < 1.0 && (x.y - a[1]).abs() < 1.0 && (x.w - a[2]).abs() < 1.0 && (x.h - a[3]).abs() < 1.0)
        };
        let mut map: HashMap<String, Vec<ShapeId>> = HashMap::new();
        for (model, boxes) in drawn.into_iter().filter(|(m, _)| !m.is_empty()) {
            let ids: Vec<ShapeId> = children.iter().filter(|c| boxes.iter().any(|b| near(b, c))).map(|c| c.id).collect();
            if !ids.is_empty() {
                map.insert(model, ids);
            }
        }
        parts.insert(spid, map);
    }
    let mut out = Vec::with_capacity(slide.animations.len());
    for ((spid, part), a) in targets.iter().zip(slide.animations.drain(..)) {
        match part.as_ref().and_then(|p| parts.get(spid)?.get(p)) {
            Some(ids) => {
                for (k, id) in ids.iter().enumerate() {
                    let start = if k == 0 { a.start } else { deckcraft_model::anim::AnimStart::WithPrevious };
                    out.push(deckcraft_model::anim::Animation { shape: *id, start, ..a.clone() });
                }
            }
            None => out.push(a),
        }
    }
    slide.animations = out;
}
