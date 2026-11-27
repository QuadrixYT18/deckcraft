//! `Session::apply_remote`: another person's edit in a live session.

use std::sync::Arc;

use serde_json::json;

use crate::{Presentation, Session};
use deckcraft_model::{Shape, ShapeId};

fn add_remote_shape(p: &Presentation) -> Presentation {
    let mut p = p.clone();
    if let Some(s) = p.slides.first_mut() {
        Arc::make_mut(s).shapes.push(Shape { id: ShapeId(50_000), name: "Remote".into(), ..Default::default() });
    }
    p
}

fn has(s: &Session, id: ShapeId) -> bool {
    s.doc().unwrap().doc.slides[0].shapes.iter().any(|x| x.id == id)
}

#[test]
fn undo_takes_back_only_the_users_own_edit() {
    let mut s = Session::with_new();
    let mine = s.execute("shape.insert", &json!({"preset": "rect", "rect": [10, 10, 100, 50]})).unwrap();
    let mine = ShapeId(mine["id"].as_u64().unwrap() as u32);
    let uid = s.doc().unwrap().uid;
    assert!(s.apply_remote(uid, &add_remote_shape));
    assert!(has(&s, mine) && has(&s, ShapeId(50_000)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!has(&s, mine), "undo removes my rectangle");
    assert!(has(&s, ShapeId(50_000)), "undo keeps the other person's shape");
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(has(&s, mine) && has(&s, ShapeId(50_000)));
    // A remote edit is not an undo step of its own.
    assert_eq!(s.doc().unwrap().history.undo.len(), 1);
}

#[test]
fn current_slide_is_kept_when_a_slide_is_inserted_before_it() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({})).unwrap();
    let st = s.doc().unwrap();
    let current = st.doc.slides[st.selection.slide].id;
    let uid = st.uid;
    s.apply_remote(uid, &|p: &Presentation| {
        let mut p = p.clone();
        let mut extra = (*p.slides[0]).clone();
        extra.id = deckcraft_model::SlideId(60_000);
        p.slides.insert(0, Arc::new(extra));
        p
    });
    let st = s.doc().unwrap();
    assert_eq!(st.doc.slides[st.selection.slide].id, current);
}

#[test]
fn caret_in_text_another_person_shortened_moves_to_its_end() {
    let mut s = Session::with_new();
    let title = s.doc().unwrap().current_slide().unwrap().shapes[0].id;
    s.execute("text.edit", &json!({"id": title.0})).unwrap();
    s.execute("text.insert", &json!({"text": "Hello world"})).unwrap();
    let uid = s.doc().unwrap().uid;
    s.apply_remote(uid, &|p: &Presentation| {
        let mut p = p.clone();
        if let Some(sh) = Arc::make_mut(&mut p.slides[0]).shapes.iter_mut().find(|x| x.id == title) {
            sh.text = Some(deckcraft_model::text::TextBody::from_text("Hi"));
        }
        p
    });
    let t = s.doc().unwrap().selection.text.clone().unwrap();
    assert_eq!(t.caret, (0, 2));
    // Typing goes on without trouble.
    s.execute("text.insert", &json!({"text": "!"})).unwrap();
    assert_eq!(s.execute("text.get", &json!({"id": title.0})).unwrap()["text"], "Hi!");
}

#[test]
fn deleted_selected_shape_is_deselected() {
    let mut s = Session::with_new();
    let r = s.execute("shape.insert", &json!({"preset": "ellipse", "rect": [10, 10, 100, 50]})).unwrap();
    let id = ShapeId(r["id"].as_u64().unwrap() as u32);
    assert_eq!(s.doc().unwrap().selection.shapes, vec![id]);
    let uid = s.doc().unwrap().uid;
    s.apply_remote(uid, &|p: &Presentation| {
        let mut p = p.clone();
        Arc::make_mut(&mut p.slides[0]).shapes.retain(|x| x.id != id);
        p
    });
    assert!(s.doc().unwrap().selection.shapes.is_empty());
    assert!(!s.apply_remote(uid.wrapping_add(1), &|p: &Presentation| p.clone()), "unknown document");
}
