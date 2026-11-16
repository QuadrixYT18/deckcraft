//! The live-session driver with a scripted network: join, receive the document, send the user's
//! edits, apply other people's, and stay read-only as a viewer.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_collab::patch::{Entry, Patch, diff};
use deckcraft_collab::proto::{ClientMsg, IdBlock, Peer, Role, ServerMsg};
use deckcraft_collab::server::Shared;
use deckcraft_engine::Session;
use serde_json::json;

use crate::live::{LiveNet, LiveSocketEvent, LiveStatus};
use crate::{Services, SlideApp};

#[derive(Default)]
struct Wire {
    posts: Vec<(String, Vec<u8>)>,
    http: Option<Result<(u16, String), String>>,
    connects: Vec<String>,
    sent: Vec<String>,
    incoming: Vec<LiveSocketEvent>,
}

struct FakeNet(Rc<RefCell<Wire>>);

impl LiveNet for FakeNet {
    fn post(&mut self, url: &str, _content_type: &str, body: Vec<u8>) {
        self.0.borrow_mut().posts.push((url.into(), body));
    }
    fn poll_http(&mut self) -> Option<Result<(u16, String), String>> {
        self.0.borrow_mut().http.take()
    }
    fn connect(&mut self, url: &str) {
        self.0.borrow_mut().connects.push(url.into());
    }
    fn send(&mut self, text: String) {
        self.0.borrow_mut().sent.push(text);
    }
    fn poll(&mut self) -> Vec<LiveSocketEvent> {
        std::mem::take(&mut self.0.borrow_mut().incoming)
    }
    fn close(&mut self) {}
}

fn app() -> (SlideApp, Rc<RefCell<Wire>>) {
    let wire = Rc::new(RefCell::new(Wire::default()));
    let services = Services { live_net: Some(Box::new(FakeNet(wire.clone()))), ..Default::default() };
    (SlideApp::new(Session::with_new(), services), wire)
}

fn tick(app: &mut SlideApp) {
    crate::live::tick(app, &egui::Context::default());
}

fn welcome(shared: &Shared, role: Role) -> String {
    ServerMsg::Welcome {
        title: "Team deck".into(),
        you: Peer { id: "p1".into(), name: "Ann".into(), role, online: true, slide: None },
        ids: IdBlock::nth(0).unwrap(),
        seq: shared.seq,
        applied_op: 0,
        entries: shared.snapshot(),
    }
    .to_json()
}

fn ops(wire: &Rc<RefCell<Wire>>) -> Vec<Vec<Entry>> {
    wire.borrow()
        .sent
        .iter()
        .filter_map(|m| match serde_json::from_str::<ClientMsg>(m).ok()? {
            ClientMsg::Op { entries, .. } => Some(entries),
            _ => None,
        })
        .collect()
}

/// Join with a code, get the document, and edit it together.
#[test]
fn join_receive_send_and_apply() {
    let (mut app, wire) = app();
    crate::live::join(&mut app, "http://server:8080/", "Ann", "ABCDE-FGHJK").unwrap();
    assert_eq!(wire.borrow().posts[0].0, "http://server:8080/api/join");
    wire.borrow_mut().http = Some(Ok((200, json!({"token": "t", "wsPath": "/ws", "role": "editor"}).to_string())));
    tick(&mut app);
    assert_eq!(wire.borrow().connects, vec!["ws://server:8080/ws".to_string()]);
    // The socket opens: hello with the token.
    wire.borrow_mut().incoming.push(LiveSocketEvent::Open);
    tick(&mut app);
    assert!(wire.borrow().sent.iter().any(|m| m.contains(r#""t":"hello""#) && m.contains(r#""token":"t""#)));
    // The server's document opens as a new presentation.
    let shared_doc = deckcraft_model::defaults::new_presentation(None);
    let mut shared_doc = shared_doc;
    let layout = shared_doc.masters[0].layouts[0].id;
    let slide = deckcraft_model::defaults::new_slide(&mut shared_doc, layout).unwrap();
    shared_doc.slides.push(std::sync::Arc::new(slide));
    let shared = Shared::from_presentation(&shared_doc).unwrap();
    let docs_before = app.session.documents().len();
    wire.borrow_mut().incoming.push(LiveSocketEvent::Text(welcome(&shared, Role::Editor)));
    tick(&mut app);
    assert_eq!(app.session.documents().len(), docs_before + 1);
    assert_eq!(app.live.as_ref().unwrap().status, LiveStatus::Live);
    assert_eq!(app.session.doc().unwrap().title, "Team deck");
    // A new shape takes an id from this connection's block and is sent.
    let r = app.run("shape.insert", json!({"preset": "rect", "rect": [10, 10, 100, 50]})).unwrap();
    let id = r["id"].as_u64().unwrap() as u32;
    assert!(IdBlock::nth(0).unwrap().contains(id), "id {id} is in the block");
    app.live.as_mut().unwrap().force_send_now();
    tick(&mut app);
    let sent = ops(&wire);
    assert!(sent.iter().flatten().any(|e| e.k.ends_with(&format!("/{id}"))), "the rectangle was sent: {sent:?}");
    // Someone else adds a shape: it appears, and Undo still takes back only the rectangle.
    let mut theirs = (*app.session.doc().unwrap().doc).clone();
    std::sync::Arc::make_mut(&mut theirs.slides[0]).shapes.push(deckcraft_model::Shape {
        id: deckcraft_model::ShapeId(17_000_000),
        name: "Theirs".into(),
        ..Default::default()
    });
    let entries: Vec<Entry> = diff(Some(&app.session.doc().unwrap().doc), &theirs).to_entries().unwrap();
    wire.borrow_mut().incoming.push(LiveSocketEvent::Text(ServerMsg::Op { seq: 5, from: "other".into(), entries }.to_json()));
    tick(&mut app);
    let has = |app: &SlideApp, id: u32| app.session.doc().unwrap().doc.slides[0].shapes.iter().any(|s| s.id.0 == id);
    assert!(has(&app, 17_000_000) && has(&app, id));
    app.run("edit.undo", json!({})).unwrap();
    assert!(!has(&app, id) && has(&app, 17_000_000));
    // A dropped connection reconnects.
    wire.borrow_mut().incoming.push(LiveSocketEvent::Closed("network".into()));
    tick(&mut app);
    assert!(matches!(app.live.as_ref().unwrap().status, LiveStatus::Reconnecting(_)));
}

/// A wrong code ends with the server's message, shown in the dialog.
#[test]
fn wrong_code_shows_the_servers_message() {
    let (mut app, wire) = app();
    crate::live::join(&mut app, "http://server:8080", "Ann", "NOPE").unwrap();
    wire.borrow_mut().http = Some(Ok((403, json!({"error": "That access code doesn't match an open session."}).to_string())));
    tick(&mut app);
    let live = app.live.as_ref().unwrap();
    assert!(!live.is_active());
    assert!(live.error.as_deref().unwrap().contains("doesn't match"));
    assert!(wire.borrow().connects.is_empty());
}

/// A viewer's edits are undone and nothing is sent.
#[test]
fn viewer_edits_are_undone() {
    let (mut app, wire) = app();
    crate::live::join_with_token(&mut app, "http://server:8080", "t");
    wire.borrow_mut().incoming.push(LiveSocketEvent::Open);
    tick(&mut app);
    let shared = Shared::from_presentation(&app.session.doc().unwrap().doc).unwrap();
    wire.borrow_mut().incoming.push(LiveSocketEvent::Text(welcome(&shared, Role::Viewer)));
    tick(&mut app);
    let before = app.session.doc().unwrap().doc.slides[0].shapes.len();
    let _ = app.run("shape.insert", json!({"preset": "rect", "rect": [10, 10, 100, 50]}));
    app.live.as_mut().unwrap().force_send_now();
    tick(&mut app);
    assert_eq!(app.session.doc().unwrap().doc.slides[0].shapes.len(), before);
    assert!(ops(&wire).is_empty());
    let _ = Patch::default();
}
