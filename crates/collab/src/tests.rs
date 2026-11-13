use std::sync::Arc;

use deckcraft_model::{Presentation, Shape, ShapeId, Slide, SlideId, defaults};

use crate::client::{Client, Event};
use crate::patch::{Patch, apply, diff, empty, parse_key};
use crate::proto::{ClientMsg, IdBlock, Peer, Role, ServerMsg};
use crate::server::Shared;

fn deck() -> Presentation {
    let mut p = defaults::new_presentation(None);
    let layout = p.masters.first().and_then(|m| m.layouts.first()).map(|l| l.id).unwrap_or_default();
    for _ in 0..3 {
        if let Some(s) = defaults::new_slide(&mut p, layout) {
            p.slides.push(Arc::new(s));
        }
    }
    p
}

fn shape(id: u32, name: &str) -> Shape {
    Shape { id: ShapeId(id), name: name.into(), ..Default::default() }
}

fn edit_slide(p: &mut Presentation, i: usize, f: impl FnOnce(&mut Slide)) {
    if let Some(s) = p.slides.get_mut(i) {
        f(Arc::make_mut(s));
    }
}

#[test]
fn snapshot_round_trips() {
    let p = deck();
    let snap = diff(None, &p).to_entries().unwrap();
    let back = apply(&empty(), &Patch::from_entries(&snap).unwrap());
    assert!(diff(Some(&p), &back).is_empty(), "{:?}", diff(Some(&p), &back).keys());
}

#[test]
fn diff_of_one_shape_is_one_key() {
    let a = deck();
    let mut b = a.clone();
    let sid = b.slides[1].id;
    edit_slide(&mut b, 1, |s| s.shapes.push(shape(900, "Rect")));
    let d = diff(Some(&a), &b);
    let mut keys = d.keys();
    keys.sort();
    assert_eq!(keys, vec![format!("shape/{}/900", sid.0), format!("slide/{}", sid.0)]);
    let mut c = b.clone();
    edit_slide(&mut c, 1, |s| {
        if let Some(sh) = s.shapes.iter_mut().find(|x| x.id == ShapeId(900)) {
            sh.name = "Renamed".into();
        }
    });
    assert_eq!(diff(Some(&b), &c).keys(), vec![format!("shape/{}/900", sid.0)]);
    // Applying the diff to the old document gives the new one.
    let wire = Patch::from_entries(&diff(Some(&a), &c).to_entries().unwrap()).unwrap();
    assert!(diff(Some(&apply(&a, &wire)), &c).is_empty());
}

#[test]
fn slide_insert_delete_and_reorder() {
    let a = deck();
    let mut b = a.clone();
    b.slides.swap(0, 2);
    b.slides.remove(1);
    let mut extra = (*a.slides[0]).clone();
    extra.id = SlideId(777);
    b.slides.insert(0, Arc::new(extra));
    let p = Patch::from_entries(&diff(Some(&a), &b).to_entries().unwrap()).unwrap();
    let c = apply(&a, &p);
    let ids = |d: &Presentation| d.slides.iter().map(|s| s.id).collect::<Vec<_>>();
    assert_eq!(ids(&c), ids(&b));
}

#[test]
fn hostile_entries_are_refused() {
    assert!(parse_key("shape/1").is_err());
    assert!(parse_key("slide/x").is_err());
    assert!(parse_key("../etc").is_err());
    let bad = vec![crate::patch::Entry { k: "slide/3".into(), v: Some(serde_json::json!({"hidden": "yes"})) }];
    assert!(Patch::from_entries(&bad).is_err());
    let mut s = Shared::default();
    assert!(s.apply_op("c", 1, &bad).is_err());
    assert_eq!(s.seq, 0, "a refused op changes nothing");
    let no_meta = vec![crate::patch::Entry { k: "meta".into(), v: None }];
    assert!(Patch::from_entries(&no_meta).is_err());
}

#[test]
fn id_blocks_are_disjoint_and_end() {
    let a = IdBlock::nth(0).unwrap();
    let b = IdBlock::nth(1).unwrap();
    assert_eq!(a.end, b.start);
    assert!(IdBlock::nth(u32::MAX).is_none());
    assert!(IdBlock::nth(4078).is_some());
    assert!(IdBlock::nth(4079).is_none(), "its end would be 2^32");
}

/// A test harness: one server state and clients that exchange messages through it.
struct Net {
    shared: Shared,
    clients: Vec<(Client, Option<Arc<Presentation>>, Vec<String>)>,
}

impl Net {
    fn new(doc: &Presentation, n: usize) -> Net {
        let mut net = Net { shared: Shared::from_presentation(doc).unwrap(), clients: vec![] };
        for i in 0..n {
            let mut c = Client::new(format!("c{i}"), String::new());
            c.connected();
            net.clients.push((c, None, vec![]));
            net.join(i);
        }
        net
    }

    fn welcome(&mut self, i: usize) -> String {
        let id = self.clients[i].0.client_id.clone();
        let ids = self.shared.next_block().unwrap();
        ServerMsg::Welcome {
            title: "t".into(),
            you: Peer { id: id.clone(), name: id.clone(), role: Role::Editor, online: true, slide: None },
            ids,
            seq: self.shared.seq,
            applied_op: self.shared.applied_op(&id),
            entries: self.shared.snapshot(),
        }
        .to_json()
    }

    fn join(&mut self, i: usize) {
        let _hello = self.clients[i].0.take_outbox();
        let w = self.welcome(i);
        self.deliver(i, &w);
        self.pump();
    }

    fn deliver(&mut self, i: usize, msg: &str) {
        let (c, doc, _) = &mut self.clients[i];
        for ev in c.receive(msg, doc.as_ref()).unwrap() {
            match ev {
                Event::Joined(p) => {
                    let d = Arc::new(*p);
                    c.adopt(d.clone());
                    *doc = Some(d);
                }
                Event::Apply(p) => {
                    if let Some(d) = doc.as_mut() {
                        let n = Arc::new(apply(d, &p));
                        *d = n.clone();
                        c.adopt(n);
                    }
                }
                _ => {}
            }
        }
    }

    /// Local edit on client `i` (not yet sent).
    fn edit(&mut self, i: usize, f: impl FnOnce(&mut Presentation)) {
        let doc = self.clients[i].1.as_mut().unwrap();
        let mut p = (**doc).clone();
        f(&mut p);
        *doc = Arc::new(p);
    }

    /// Client `i` notices its changes and puts them in its outbox.
    fn flush(&mut self, i: usize) {
        let (c, doc, out) = &mut self.clients[i];
        if let Some(d) = doc.clone() {
            assert!(c.local(&d).is_none());
        }
        out.extend(c.take_outbox());
    }

    /// Deliver every queued client message to the server and the server's answers back.
    fn pump(&mut self) {
        loop {
            let mut any = false;
            for i in 0..self.clients.len() {
                let out = std::mem::take(&mut self.clients[i].2);
                for m in out {
                    any = true;
                    let id = self.clients[i].0.client_id.clone();
                    match serde_json::from_str::<ClientMsg>(&m).unwrap() {
                        ClientMsg::Op { op, entries } => {
                            let seq = self.shared.apply_op(&id, op, &entries).unwrap();
                            self.deliver(i, &ServerMsg::Ack { seq, op }.to_json());
                            let msg = ServerMsg::Op { seq, from: id.clone(), entries }.to_json();
                            for j in 0..self.clients.len() {
                                if j != i {
                                    self.deliver(j, &msg);
                                    self.flush(j);
                                }
                            }
                        }
                        ClientMsg::Hello { .. } | ClientMsg::Presence { .. } => {}
                    }
                }
            }
            if !any {
                break;
            }
        }
    }

    fn assert_converged(&self) {
        let server = self.shared.materialize().unwrap();
        for (i, (c, doc, _)) in self.clients.iter().enumerate() {
            let d = doc.as_ref().unwrap();
            let keys = diff(Some(&server), d).keys();
            assert!(keys.is_empty(), "client {i} differs from the server in {keys:?}");
            assert_eq!(c.unconfirmed(), 0);
        }
    }
}

fn slide_id(net: &Net, i: usize) -> SlideId {
    net.clients[0].1.as_ref().unwrap().slides[i].id
}

#[test]
fn a_inserts_b_recolours_a_sees_it() {
    // The acceptance test of the brief, on the document level.
    let mut net = Net::new(&deck(), 2);
    let sid = slide_id(&net, 0);
    net.edit(0, |p| edit_slide(p, 0, |s| s.shapes.push(shape(16_777_300, "Rectangle 1"))));
    net.flush(0);
    net.pump();
    let b = net.clients[1].1.as_ref().unwrap();
    assert!(b.slides[0].shapes.iter().any(|s| s.id == ShapeId(16_777_300)), "B sees A's rectangle");
    net.edit(1, |p| {
        edit_slide(p, 0, |s| {
            if let Some(sh) = s.shapes.iter_mut().find(|s| s.id == ShapeId(16_777_300)) {
                sh.name = "red".into();
            }
        })
    });
    net.flush(1);
    net.pump();
    let a = net.clients[0].1.as_ref().unwrap();
    let sh = a.slides.iter().find(|s| s.id == sid).and_then(|s| s.shapes.iter().find(|s| s.id == ShapeId(16_777_300)));
    assert_eq!(sh.map(|s| s.name.as_str()), Some("red"), "A sees B's change");
    net.assert_converged();
}

#[test]
fn four_clients_concurrent_edits_converge() {
    let mut net = Net::new(&deck(), 4);
    // Everyone edits at once before any message is delivered: each adds a shape to the same
    // slide, client 0 also deletes slide 2 while client 3 edits a shape on it, and clients 1 and
    // 2 both rename the same shape.
    for i in 0..4u32 {
        net.edit(i as usize, |p| edit_slide(p, 0, |s| s.shapes.push(shape(16_777_216 + i * (1 << 20) + 5, &format!("from {i}")))));
    }
    net.edit(0, |p| {
        p.slides.remove(2);
    });
    net.edit(3, |p| edit_slide(p, 2, |s| s.shapes.push(shape(16_777_216 + 3 * (1 << 20) + 6, "on deleted slide"))));
    net.edit(1, |p| edit_slide(p, 1, |s| s.shapes.push(shape(16_777_216 + (1 << 20) + 9, "shared"))));
    net.flush(1);
    net.pump();
    net.edit(1, |p| edit_slide(p, 1, |s| s.shapes.iter_mut().for_each(|x| x.name = "one".into())));
    net.edit(2, |p| edit_slide(p, 1, |s| s.shapes.iter_mut().for_each(|x| x.name = "two".into())));
    for i in 0..4 {
        net.flush(i);
    }
    net.pump();
    net.assert_converged();
    let doc = net.clients[0].1.as_ref().unwrap();
    // No concurrent insert was lost.
    let names: Vec<String> = doc.slides[0].shapes.iter().map(|s| s.name.clone()).collect();
    for i in 0..4 {
        assert!(names.contains(&format!("from {i}")), "{names:?}");
    }
}

#[test]
fn reconnect_resends_unconfirmed_and_offline_changes() {
    let mut net = Net::new(&deck(), 2);
    // Client 0 makes a change whose message is lost with the connection.
    net.edit(0, |p| edit_slide(p, 0, |s| s.shapes.push(shape(16_777_216 + 1, "lost in transit"))));
    net.flush(0);
    net.clients[0].2.clear();
    net.clients[0].0.disconnected();
    // Edits while offline.
    net.edit(0, |p| edit_slide(p, 1, |s| s.shapes.push(shape(16_777_216 + 2, "offline"))));
    net.flush(0);
    assert!(net.clients[0].2.is_empty(), "nothing is sent while offline");
    // Meanwhile client 1 edits.
    net.edit(1, |p| edit_slide(p, 2, |s| s.shapes.push(shape(16_777_216 + (1 << 20) * 2 + 1, "other"))));
    net.flush(1);
    net.pump();
    // Reconnect.
    net.clients[0].0.connected();
    net.join(0);
    let out = net.clients[0].0.take_outbox();
    net.clients[0].2.extend(out);
    net.pump();
    net.assert_converged();
    let doc = net.clients[1].1.as_ref().unwrap();
    let all: Vec<String> = doc.slides.iter().flat_map(|s| s.shapes.iter().map(|x| x.name.clone())).collect();
    for n in ["lost in transit", "offline", "other"] {
        assert!(all.contains(&n.to_string()), "{n} missing from {all:?}");
    }
}

#[test]
fn resent_op_is_applied_once() {
    let mut s = Shared::from_presentation(&deck()).unwrap();
    let e = vec![crate::patch::Entry { k: "order".into(), v: Some(serde_json::json!([])) }];
    let a = s.apply_op("x", 1, &e).unwrap();
    let b = s.apply_op("x", 1, &e).unwrap();
    assert_eq!(a, b);
    assert_eq!(s.seq, 1);
}

#[test]
fn viewer_changes_are_reverted_not_sent() {
    let mut c = Client::new("v".into(), String::new());
    c.connected();
    let shared = Shared::from_presentation(&deck()).unwrap();
    let w = ServerMsg::Welcome {
        title: "t".into(),
        you: Peer { id: "v".into(), name: "v".into(), role: Role::Viewer, online: true, slide: None },
        ids: IdBlock::nth(0).unwrap(),
        seq: 0,
        applied_op: 0,
        entries: shared.snapshot(),
    };
    let mut doc = None;
    for ev in c.receive(&w.to_json(), None).unwrap() {
        if let Event::Joined(p) = ev {
            let d = Arc::new(*p);
            c.adopt(d.clone());
            doc = Some(d);
        }
    }
    let d = doc.unwrap();
    let mut changed = (*d).clone();
    edit_slide(&mut changed, 0, |s| s.shapes.push(shape(99, "nope")));
    let changed = Arc::new(changed);
    let revert = c.local(&changed).expect("a viewer's change is undone");
    assert!(diff(Some(&apply(&changed, &revert)), &d).is_empty());
    let _ = c.take_outbox();
    assert!(c.take_outbox().is_empty());
}
