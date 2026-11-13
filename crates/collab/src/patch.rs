//! The presentation as keyed values, diffs between presentations and applying a patch.
//!
//! Keys:
//! - `meta`: everything that is not a slide or a media item (slide size, masters, layouts,
//!   themes, sections, show settings, properties…), as one value.
//! - `order`: the slide ids in order.
//! - `slide/<id>`: a slide without its shapes, plus `shapeOrder` (its top-level shape ids).
//! - `shape/<slide>/<shape>`: one top-level shape (a group carries its children).
//! - `media/<id>`: an embedded file, its bytes base64 encoded.
//!
//! A `null` value deletes the key.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use base64::Engine as _;
use deckcraft_model::{MediaId, MediaItem, Presentation, Shape, ShapeId, Slide, SlideId};
use serde_json::Value;

use crate::{CollabError, Result};

/// The largest embedded file one key may carry (decoded).
pub const MAX_MEDIA_BYTES: usize = 64 << 20;

/// A slide's own properties and the order of its top-level shapes.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideHead {
    /// The slide with no shapes.
    pub slide: Slide,
    pub shape_order: Vec<ShapeId>,
}

/// A typed set of changes. Entries for the same key appear at most once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub meta: Option<Box<Presentation>>,
    pub order: Option<Vec<SlideId>>,
    /// `None`: the slide was deleted.
    pub slides: BTreeMap<SlideId, Option<SlideHead>>,
    /// `None`: the shape was deleted.
    pub shapes: BTreeMap<(SlideId, ShapeId), Option<Shape>>,
    /// `None`: the media item was deleted.
    pub media: BTreeMap<MediaId, Option<MediaItem>>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.meta.is_none() && self.order.is_none() && self.slides.is_empty() && self.shapes.is_empty() && self.media.is_empty()
    }

    /// The keys this patch writes.
    pub fn keys(&self) -> Vec<String> {
        let mut k = Vec::new();
        if self.meta.is_some() {
            k.push(META.to_string());
        }
        if self.order.is_some() {
            k.push(ORDER.to_string());
        }
        k.extend(self.slides.keys().map(|s| slide_key(*s)));
        k.extend(self.shapes.keys().map(|(s, sh)| shape_key(*s, *sh)));
        k.extend(self.media.keys().map(|m| media_key(*m)));
        k
    }

    /// Drop every entry whose key is in `skip`.
    pub fn without_keys(mut self, skip: &BTreeSet<String>) -> Patch {
        if skip.is_empty() {
            return self;
        }
        if skip.contains(META) {
            self.meta = None;
        }
        if skip.contains(ORDER) {
            self.order = None;
        }
        self.slides.retain(|s, _| !skip.contains(&slide_key(*s)));
        self.shapes.retain(|(s, sh), _| !skip.contains(&shape_key(*s, *sh)));
        self.media.retain(|m, _| !skip.contains(&media_key(*m)));
        self
    }

    /// Later entries replace earlier ones for the same key.
    pub fn merge(&mut self, later: Patch) {
        if later.meta.is_some() {
            self.meta = later.meta;
        }
        if later.order.is_some() {
            self.order = later.order;
        }
        self.slides.extend(later.slides);
        self.shapes.extend(later.shapes);
        self.media.extend(later.media);
    }

    /// The wire form: one `{k, v}` entry per key.
    pub fn to_entries(&self) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        if let Some(meta) = &self.meta {
            out.push(Entry { k: META.into(), v: Some(meta_value(meta)?) });
        }
        if let Some(order) = &self.order {
            out.push(Entry { k: ORDER.into(), v: Some(to_value(ORDER, order)?) });
        }
        for (id, head) in &self.slides {
            let v = match head {
                Some(h) => {
                    let mut v = to_value("slide", &h.slide)?;
                    if let Some(o) = v.as_object_mut() {
                        o.insert("shapes".into(), Value::Array(vec![]));
                        o.insert("shapeOrder".into(), to_value("slide", &h.shape_order)?);
                    }
                    Some(v)
                }
                None => None,
            };
            out.push(Entry { k: slide_key(*id), v });
        }
        for ((s, sh), shape) in &self.shapes {
            let v = match shape {
                Some(shape) => Some(to_value("shape", shape)?),
                None => None,
            };
            out.push(Entry { k: shape_key(*s, *sh), v });
        }
        for (id, item) in &self.media {
            let v = match item {
                Some(item) => {
                    let mut v = to_value("media", item)?;
                    if let Some(o) = v.as_object_mut() {
                        o.insert("data".into(), Value::String(base64::engine::general_purpose::STANDARD.encode(item.data.as_slice())));
                    }
                    Some(v)
                }
                None => None,
            };
            out.push(Entry { k: media_key(*id), v });
        }
        Ok(out)
    }

    /// Decode wire entries. Unknown keys and malformed values are errors: a peer sending them is
    /// broken or hostile, and nothing of the operation is applied.
    pub fn from_entries(entries: &[Entry]) -> Result<Patch> {
        let mut p = Patch::default();
        for e in entries {
            let key = parse_key(&e.k)?;
            match key {
                Key::Meta => {
                    let Some(v) = &e.v else { return Err(CollabError::BadValue(e.k.clone(), "meta can't be deleted".into())) };
                    p.meta = Some(Box::new(from_value(&e.k, v.clone())?));
                }
                Key::Order => {
                    let Some(v) = &e.v else { return Err(CollabError::BadValue(e.k.clone(), "order can't be deleted".into())) };
                    p.order = Some(from_value(&e.k, v.clone())?);
                }
                Key::Slide(id) => {
                    let head = match &e.v {
                        Some(v) => {
                            let shape_order: Vec<ShapeId> = match v.get("shapeOrder") {
                                Some(o) => from_value(&e.k, o.clone())?,
                                None => vec![],
                            };
                            let mut slide: Slide = from_value(&e.k, v.clone())?;
                            slide.shapes.clear();
                            slide.id = id;
                            Some(SlideHead { slide, shape_order })
                        }
                        None => None,
                    };
                    p.slides.insert(id, head);
                }
                Key::Shape(s, sh) => {
                    let shape = match &e.v {
                        Some(v) => {
                            let mut shape: Shape = from_value(&e.k, v.clone())?;
                            shape.id = sh;
                            Some(shape)
                        }
                        None => None,
                    };
                    p.shapes.insert((s, sh), shape);
                }
                Key::Media(id) => {
                    let item = match &e.v {
                        Some(v) => {
                            let mut item: MediaItem = from_value(&e.k, v.clone())?;
                            item.id = id;
                            let data = v.get("data").and_then(Value::as_str).unwrap_or("");
                            // base64 is 4/3 of the bytes: refuse before decoding.
                            if data.len() / 4 * 3 > MAX_MEDIA_BYTES {
                                return Err(CollabError::TooLarge(e.k.clone()));
                            }
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(data)
                                .map_err(|err| CollabError::BadValue(e.k.clone(), err.to_string()))?;
                            item.data = Arc::new(bytes);
                            Some(item)
                        }
                        None => None,
                    };
                    p.media.insert(id, item);
                }
            }
        }
        Ok(p)
    }
}

/// One keyed value on the wire. `v: null` deletes the key.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub k: String,
    pub v: Option<Value>,
}

pub const META: &str = "meta";
pub const ORDER: &str = "order";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Meta,
    Order,
    Slide(SlideId),
    Shape(SlideId, ShapeId),
    Media(MediaId),
}

pub fn slide_key(id: SlideId) -> String {
    format!("slide/{}", id.0)
}
pub fn shape_key(slide: SlideId, shape: ShapeId) -> String {
    format!("shape/{}/{}", slide.0, shape.0)
}
pub fn media_key(id: MediaId) -> String {
    format!("media/{}", id.0)
}

pub fn parse_key(k: &str) -> Result<Key> {
    let bad = || CollabError::BadKey(k.chars().take(64).collect());
    let num = |s: &str| s.parse::<u32>().map_err(|_| bad());
    let mut parts = k.split('/');
    let key = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("meta"), None, _, _) => Key::Meta,
        (Some("order"), None, _, _) => Key::Order,
        (Some("slide"), Some(id), None, _) => Key::Slide(SlideId(num(id)?)),
        (Some("shape"), Some(s), Some(sh), None) => Key::Shape(SlideId(num(s)?), ShapeId(num(sh)?)),
        (Some("media"), Some(id), None, _) => Key::Media(MediaId(num(id)?)),
        _ => return Err(bad()),
    };
    Ok(key)
}

fn to_value<T: serde::Serialize>(what: &str, v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| CollabError::BadValue(what.into(), e.to_string()))
}

fn from_value<T: serde::de::DeserializeOwned>(key: &str, v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| CollabError::BadValue(key.chars().take(64).collect(), e.to_string()))
}

/// The presentation without slides, media and the id counter.
fn stripped(p: &Presentation) -> Presentation {
    Presentation { slides: vec![], media: vec![], next_id: 0, ..p.clone() }
}

fn meta_value(p: &Presentation) -> Result<Value> {
    let mut v = to_value(META, &stripped(p))?;
    if let Some(o) = v.as_object_mut() {
        o.remove("slides");
        o.remove("media");
        o.remove("nextId");
    }
    Ok(v)
}

/// Whether everything `meta` covers is the same. Masters are `Arc`s; shared ones are equal
/// without a deep comparison.
fn meta_eq(a: &Presentation, b: &Presentation) -> bool {
    let same_masters = a.masters.len() == b.masters.len() && a.masters.iter().zip(&b.masters).all(|(x, y)| Arc::ptr_eq(x, y) || x == y);
    if !same_masters {
        return false;
    }
    let strip = |p: &Presentation| Presentation { slides: vec![], media: vec![], next_id: 0, masters: vec![], ..p.clone() };
    strip(a) == strip(b)
}

fn head_of(s: &Slide) -> SlideHead {
    SlideHead { slide: Slide { shapes: vec![], ..s.clone() }, shape_order: s.shapes.iter().map(|sh| sh.id).collect() }
}

fn head_eq(a: &Slide, b: &Slide) -> bool {
    a.shapes.len() == b.shapes.len()
        && a.shapes.iter().zip(&b.shapes).all(|(x, y)| x.id == y.id)
        && Slide { shapes: vec![], ..a.clone() } == Slide { shapes: vec![], ..b.clone() }
}

fn media_eq(a: &MediaItem, b: &MediaItem) -> bool {
    a.id == b.id && a.name == b.name && a.content_type == b.content_type && a.link == b.link && (Arc::ptr_eq(&a.data, &b.data) || a.data == b.data)
}

/// What changed from `old` to `new`. With no `old`, every key (a snapshot).
pub fn diff(old: Option<&Presentation>, new: &Presentation) -> Patch {
    let mut p = Patch::default();
    if old.is_none_or(|o| !meta_eq(o, new)) {
        p.meta = Some(Box::new(stripped(new)));
    }
    let new_order: Vec<SlideId> = new.slides.iter().map(|s| s.id).collect();
    if old.is_none_or(|o| o.slides.iter().map(|s| s.id).ne(new_order.iter().copied())) {
        p.order = Some(new_order);
    }
    let old_slides: HashMap<SlideId, &Arc<Slide>> = old.map(|o| o.slides.iter().map(|s| (s.id, s)).collect()).unwrap_or_default();
    for s in &new.slides {
        match old_slides.get(&s.id) {
            Some(o) if Arc::ptr_eq(o, s) => {}
            Some(o) => {
                if !head_eq(o, s) {
                    p.slides.insert(s.id, Some(head_of(s)));
                }
                let before: HashMap<ShapeId, &Shape> = o.shapes.iter().map(|sh| (sh.id, sh)).collect();
                for sh in &s.shapes {
                    if before.get(&sh.id).is_none_or(|b| *b != sh) {
                        p.shapes.insert((s.id, sh.id), Some(sh.clone()));
                    }
                }
                let now: BTreeSet<ShapeId> = s.shapes.iter().map(|sh| sh.id).collect();
                for sh in &o.shapes {
                    if !now.contains(&sh.id) {
                        p.shapes.insert((s.id, sh.id), None);
                    }
                }
            }
            None => {
                p.slides.insert(s.id, Some(head_of(s)));
                for sh in &s.shapes {
                    p.shapes.insert((s.id, sh.id), Some(sh.clone()));
                }
            }
        }
    }
    if let Some(o) = old {
        let now: BTreeSet<SlideId> = new.slides.iter().map(|s| s.id).collect();
        for s in &o.slides {
            if !now.contains(&s.id) {
                p.slides.insert(s.id, None);
                for sh in &s.shapes {
                    p.shapes.insert((s.id, sh.id), None);
                }
            }
        }
    }
    let old_media: HashMap<MediaId, &MediaItem> = old.map(|o| o.media.iter().map(|m| (m.id, m)).collect()).unwrap_or_default();
    for m in &new.media {
        if old_media.get(&m.id).is_none_or(|o| !media_eq(o, m)) {
            p.media.insert(m.id, Some(m.clone()));
        }
    }
    if let Some(o) = old {
        let now: BTreeSet<MediaId> = new.media.iter().map(|m| m.id).collect();
        for m in &o.media {
            if !now.contains(&m.id) {
                p.media.insert(m.id, None);
            }
        }
    }
    p
}

/// Ids in `order` that exist (first occurrence only), then the remaining ones lowest id first.
fn ordered<I: Copy + Ord>(order: &[I], present: &BTreeSet<I>) -> Vec<I> {
    let mut seen = BTreeSet::new();
    let mut out: Vec<I> = order.iter().copied().filter(|i| present.contains(i) && seen.insert(*i)).collect();
    out.extend(present.iter().copied().filter(|i| !seen.contains(i)));
    out
}

/// `doc` with `p` applied. Slides the patch doesn't touch stay the same `Arc`s.
pub fn apply(doc: &Presentation, p: &Patch) -> Presentation {
    let mut out = match &p.meta {
        Some(meta) => Presentation { slides: doc.slides.clone(), media: doc.media.clone(), next_id: doc.next_id, ..(**meta).clone() },
        None => doc.clone(),
    };
    for (id, item) in &p.media {
        let at = out.media.iter().position(|m| m.id == *id);
        match (item, at) {
            (Some(item), Some(i)) => {
                if let Some(slot) = out.media.get_mut(i) {
                    *slot = item.clone();
                }
            }
            (Some(item), None) => out.media.push(item.clone()),
            (None, Some(i)) => {
                out.media.remove(i);
            }
            (None, None) => {}
        }
    }
    let touched: BTreeSet<SlideId> = p.slides.keys().copied().chain(p.shapes.keys().map(|(s, _)| *s)).collect();
    let mut added = false;
    for sid in touched {
        let at = out.slides.iter().position(|s| s.id == sid);
        let head = p.slides.get(&sid);
        if let Some(None) = head {
            if let Some(i) = at {
                out.slides.remove(i);
            }
            continue;
        }
        let existing = at.and_then(|i| out.slides.get(i)).cloned();
        let (mut slide, order): (Slide, Vec<ShapeId>) = match (head, &existing) {
            (Some(Some(h)), Some(e)) => (Slide { shapes: e.shapes.clone(), ..h.slide.clone() }, h.shape_order.clone()),
            (Some(Some(h)), None) => (h.slide.clone(), h.shape_order.clone()),
            (_, Some(e)) => ((**e).clone(), e.shapes.iter().map(|s| s.id).collect()),
            // Shapes for a slide that doesn't exist here: nothing to put them on.
            (_, None) => continue,
        };
        slide.id = sid;
        let mut by_id: BTreeMap<ShapeId, Shape> = std::mem::take(&mut slide.shapes).into_iter().map(|s| (s.id, s)).collect();
        for ((_, sh), shape) in p.shapes.range((sid, ShapeId(0))..=(sid, ShapeId(u32::MAX))) {
            match shape {
                Some(shape) => {
                    by_id.insert(*sh, shape.clone());
                }
                None => {
                    by_id.remove(sh);
                }
            }
        }
        let present: BTreeSet<ShapeId> = by_id.keys().copied().collect();
        slide.shapes = ordered(&order, &present).into_iter().filter_map(|id| by_id.remove(&id)).collect();
        match at {
            Some(i) => {
                if let Some(slot) = out.slides.get_mut(i) {
                    *slot = Arc::new(slide);
                }
            }
            None => {
                out.slides.push(Arc::new(slide));
                added = true;
            }
        }
    }
    if p.order.is_some() || added {
        let current: Vec<SlideId> = out.slides.iter().map(|s| s.id).collect();
        let order = p.order.as_deref().unwrap_or(&current);
        let present: BTreeSet<SlideId> = current.iter().copied().collect();
        // Without a new order, newly added slides were appended above in id order.
        let mut by_id: HashMap<SlideId, Arc<Slide>> = std::mem::take(&mut out.slides).into_iter().map(|s| (s.id, s)).collect();
        out.slides = ordered(order, &present).into_iter().filter_map(|id| by_id.remove(&id)).collect();
    }
    out
}

/// An empty presentation to materialise a snapshot onto.
pub fn empty() -> Presentation {
    Presentation { slides: vec![], media: vec![], ..Presentation::default() }
}
