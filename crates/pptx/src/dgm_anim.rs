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

/// The `modelId`s of a diagram drawing's shapes, in drawing order.
fn drawing_ids(e: &El, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    for c in e.elements() {
        if c.is(Local("sp")) {
            out.push(c.attr_local("modelId").unwrap_or("").to_string());
            continue;
        }
        drawing_ids(c, out, depth + 1);
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
    let mut parts: HashMap<u32, HashMap<String, ShapeId>> = HashMap::new();
    for spid in targets.iter().filter(|(_, p)| p.is_some()).map(|(s, _)| *s) {
        if parts.contains_key(&spid) {
            continue;
        }
        let Some(ShapeKind::Group { children, .. }) = find_group(&slide.shapes, spid).map(|g| &g.kind) else { continue };
        let Some(data_path) = by_frame.get(&spid).and_then(|dm| slide_rels(dm)) else { continue };
        let Some(data) = read(&data_path) else { continue };
        let Some(drawing_rel) = data.find(Local("dataModelExt")).and_then(|x| x.attr_local("relId")).map(str::to_string) else { continue };
        let Some(drawing) = slide_rels(&drawing_rel).and_then(|p| read(&p)) else { continue };
        let mut ids = vec![];
        drawing_ids(&drawing, &mut ids, 0);
        if ids.len() != children.len() {
            continue;
        }
        parts.insert(spid, ids.into_iter().zip(children.iter().map(|c| c.id)).filter(|(m, _)| !m.is_empty()).collect());
    }
    for ((spid, part), a) in targets.iter().zip(slide.animations.iter_mut()) {
        if let Some(part) = part
            && let Some(child) = parts.get(spid).and_then(|m| m.get(part))
        {
            a.shape = *child;
        }
    }
}
