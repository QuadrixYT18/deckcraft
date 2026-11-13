//! The real server on a local port, driven over HTTP and WebSocket by the real sync client.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use deckcraft_collab::client::{Client, Event};
use deckcraft_collab::patch::apply;
use deckcraft_model::{Presentation, Shape, ShapeId};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::*;

fn start(dir: &std::path::Path) -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let cfg = Config {
        mode: Mode::Development,
        bind: addr,
        data: dir.to_path_buf(),
        web_dir: dir.join("web"),
        admin_password: None,
        public_url: None,
        allowed_origins: vec![],
        trust_loopback: true,
        open_dashboard: false,
    };
    let app = Arc::new(App { cfg, secret: vec![9; 32], sessions: Mutex::default(), limiter: Mutex::default(), next_conn: AtomicU64::new(1) });
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            tokio::spawn(saver(app.clone()));
            let l = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(l, router(app).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
    });
    addr
}

fn post_json(url: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut r = agent.post(url).send_json(body).unwrap();
    let status = r.status().as_u16();
    (status, r.body_mut().read_json().unwrap_or(serde_json::Value::Null))
}

struct Peer {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    client: Client,
    doc: Option<Arc<Presentation>>,
    closed: Option<String>,
}

impl Peer {
    fn connect(addr: SocketAddr, token: &str, id: &str) -> Peer {
        let (ws, _) = tungstenite::connect(format!("ws://{addr}/ws")).unwrap();
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        }
        let mut p = Peer { ws, client: Client::new(id.into(), token.into()), doc: None, closed: None };
        p.client.connected();
        p.flush();
        p
    }

    fn flush(&mut self) {
        if let Some(d) = self.doc.clone() {
            self.client.local(&d);
        }
        for m in self.client.take_outbox() {
            self.ws.send(Message::text(m)).unwrap();
        }
    }

    /// Read and apply messages until `done` or a timeout.
    fn pump_until(&mut self, done: impl Fn(&Peer) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            match self.ws.read() {
                Ok(Message::Text(t)) => {
                    for ev in self.client.receive(t.as_str(), self.doc.as_ref()).unwrap() {
                        match ev {
                            Event::Joined(p) => {
                                let d = Arc::new(*p);
                                self.client.adopt(d.clone());
                                self.doc = Some(d);
                            }
                            Event::Apply(patch) => {
                                if let Some(d) = &self.doc {
                                    let n = Arc::new(apply(d, &patch));
                                    self.client.adopt(n.clone());
                                    self.doc = Some(n);
                                }
                            }
                            Event::Closed(r) => self.closed = Some(r),
                            _ => {}
                        }
                    }
                    self.flush();
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => {
                    if self.closed.is_none() {
                        self.closed = Some("socket closed".into());
                    }
                }
            }
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Presentation)) {
        let mut p = (**self.doc.as_ref().unwrap()).clone();
        f(&mut p);
        self.doc = Some(Arc::new(p));
        self.flush();
    }

    fn shape(&self, id: ShapeId) -> Option<Shape> {
        self.doc.as_ref()?.slides.first()?.shapes.iter().find(|s| s.id == id).cloned()
    }
}

#[test]
fn host_creates_people_join_and_edits_sync_both_ways() {
    let dir = std::env::temp_dir().join(format!("deckcraft-server-test-{}", auth::random_id()));
    let addr = start(&dir);
    let base = format!("http://{addr}");
    // Wait until it listens.
    let t = Instant::now();
    while ureq::get(&format!("{base}/healthz")).call().is_err() {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    // 26/27: the host creates a session (from this computer) and gets the code.
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut r = agent.post(&format!("{base}/api/sessions?name=Review&host=Ann")).send(&[][..]).unwrap();
    assert_eq!(r.status(), 200);
    let created: serde_json::Value = r.body_mut().read_json().unwrap();
    let code = created["code"].as_str().unwrap().to_string();
    let host_token = created["token"].as_str().unwrap().to_string();
    // The dashboard page shows it.
    let page = agent.get(&format!("{base}/host/s/{}", created["sessionId"].as_str().unwrap())).call().unwrap().body_mut().read_to_string().unwrap();
    assert!(page.contains(&code), "the dashboard shows the access code");

    // 29: a wrong code is refused with a clear message.
    let (status, body) = post_json(&format!("{base}/api/join"), serde_json::json!({"name": "Bob", "code": "AAAAA-AAAAA"}));
    assert_eq!(status, 403);
    assert!(body["error"].as_str().unwrap().contains("doesn't match"));
    // 28: the right one, typed sloppily, works.
    let (status, joined) = post_json(&format!("{base}/api/join"), serde_json::json!({"name": "Bob", "code": code.to_lowercase().replace('-', " ")}));
    assert_eq!(status, 200, "{joined}");
    let bob_token = joined["token"].as_str().unwrap().to_string();
    // A forged token gets nowhere.
    let mut mallory = Peer::connect(addr, "e30.e30.e30", "m");
    mallory.pump_until(|p| p.closed.is_some());
    assert_eq!(mallory.closed.as_deref(), Some("unauthorized"));

    // 30: two clients connect.
    let mut a = Peer::connect(addr, &host_token, "a");
    let mut b = Peer::connect(addr, &bob_token, "b");
    a.pump_until(|p| p.doc.is_some());
    b.pump_until(|p| p.doc.is_some());
    let ids = a.client.ids.unwrap();
    assert_ne!(Some(ids), b.client.ids, "each connection has its own id block");

    // 31: A inserts a rectangle; B sees it without reloading.
    let rect = ShapeId(ids.start + 1);
    a.edit(|p| Arc::make_mut(&mut p.slides[0]).shapes.push(Shape { id: rect, name: "Rectangle 1".into(), ..Default::default() }));
    b.pump_until(|p| p.shape(rect).is_some());
    // B changes it; A sees the change.
    b.edit(|p| {
        if let Some(s) = Arc::make_mut(&mut p.slides[0]).shapes.iter_mut().find(|s| s.id == rect) {
            s.name = "Rectangle 1 (red)".into();
        }
    });
    a.pump_until(|p| p.shape(rect).is_some_and(|s| s.name.ends_with("(red)")));

    // 37/38: a third client joining later (a reload) gets the current document.
    let mut c = Peer::connect(addr, &bob_token, "c");
    c.pump_until(|p| p.shape(rect).is_some_and(|s| s.name.ends_with("(red)")));

    // 46: a viewer can't change anything.
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let bob_pid = auth::verify(&[9; 32], &bob_token).unwrap().pid;
    let csrf = auth::tag(&[9; 32], "csrf");
    let r = agent
        .post(&format!("{base}/host/s/{sid}/action"))
        .send_form([("csrf", csrf.as_str()), ("action", "role"), ("pid", bob_pid.as_str()), ("role", "viewer")])
        .unwrap();
    assert!(r.status().is_success() || r.status().is_redirection());
    b.pump_until(|p| !p.client.can_edit());
    // The host removes Bob: his connections close and his token stops working.
    agent.post(&format!("{base}/host/s/{sid}/action")).send_form([("csrf", csrf.as_str()), ("action", "remove"), ("pid", bob_pid.as_str())]).unwrap();
    b.pump_until(|p| p.closed.is_some());
    assert_eq!(b.closed.as_deref(), Some("kicked"));
    let mut again = Peer::connect(addr, &bob_token, "b2");
    again.pump_until(|p| p.closed.is_some());
    assert_eq!(again.closed.as_deref(), Some("kicked"));
    // A dashboard form without the CSRF value is refused.
    let r = agent.post(&format!("{base}/host/s/{sid}/action")).send_form([("csrf", "x"), ("action", "end")]).unwrap();
    assert_eq!(r.status(), 403);

    // The document is saved to disk.
    let t = Instant::now();
    loop {
        let saved = std::fs::read_to_string(dir.join("sessions").join(&sid).join("shared.json")).unwrap_or_default();
        if saved.contains("Rectangle 1 (red)") {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "saved within a few seconds");
        std::thread::sleep(Duration::from_millis(200));
    }
    // And can be downloaded as PowerPoint.
    let mut r = agent.get(&format!("{base}/host/s/{sid}/download/pptx")).call().unwrap();
    assert_eq!(r.status(), 200);
    let pptx = r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap();
    let back = deckcraft_pptx::import(&pptx).unwrap();
    assert!(back.slides.iter().any(|s| s.shapes.iter().any(|x| x.name == "Rectangle 1 (red)")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn join_attempts_are_rate_limited() {
    let dir = std::env::temp_dir().join(format!("deckcraft-server-test-{}", auth::random_id()));
    let addr = start(&dir);
    let base = format!("http://{addr}");
    let t = Instant::now();
    while ureq::get(&format!("{base}/healthz")).call().is_err() {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    for _ in 0..auth::MAX_FAILURES {
        assert_eq!(post_json(&format!("{base}/api/join"), serde_json::json!({"name": "x", "code": "WRONG-WRONG"})).0, 403);
    }
    assert_eq!(post_json(&format!("{base}/api/join"), serde_json::json!({"name": "x", "code": "WRONG-WRONG"})).0, 429);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dashboard_needs_a_password_from_elsewhere() {
    // Through a name other than localhost (DNS rebinding) the loopback exception doesn't apply.
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("evil.example:8080"));
    assert!(!is_loopback(&"127.0.0.1:5000".parse().unwrap(), &h));
    h.insert(header::HOST, HeaderValue::from_static("localhost:8080"));
    assert!(is_loopback(&"127.0.0.1:5000".parse().unwrap(), &h));
    assert!(!is_loopback(&"192.168.1.20:5000".parse().unwrap(), &h));
}
