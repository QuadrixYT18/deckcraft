//! Live sessions in the app: several people edit one presentation at once, in the desktop app
//! and in the browser alike.
//!
//! This module drives a [`deckcraft_collab::client::Client`] each frame: it applies other people's
//! changes to the shared presentation through [`deckcraft_engine::Session::apply_remote`] (so Undo
//! stays the user's own) and sends the user's changes. The network is the shell's
//! ([`LiveNet`]): threads on the desktop, the browser's WebSocket and fetch on the web.
//!
//! Review › Live Session opens the dialog: start a session with the open presentation (the host,
//! on the server's computer) and see its access code, or join one with a code.

use std::sync::Arc;

use deckcraft_collab::client::{Client, Event};
use deckcraft_collab::patch::{Patch, apply, diff};
use deckcraft_collab::proto::Role;
use deckcraft_engine::DocState;
use egui::{Color32, RichText, Ui, vec2};
use serde_json::Value;
use web_time::{Duration, Instant};

use crate::SlideApp;
use crate::dialogs::Dialog;
use crate::theme::Tokens;

/// What the shell's socket reports.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveSocketEvent {
    Open,
    Text(String),
    /// The socket closed or couldn't open.
    Closed(String),
}

/// The network a live session needs. One HTTP request and one socket at a time.
pub trait LiveNet {
    /// Start `POST url` with `body`; the answer arrives through [`Self::poll_http`].
    fn post(&mut self, url: &str, content_type: &str, body: Vec<u8>);
    /// The answer to the last [`Self::post`]: HTTP status and body, or why it failed.
    fn poll_http(&mut self) -> Option<Result<(u16, String), String>>;
    /// Open a WebSocket to `url` (closing any open one).
    fn connect(&mut self, url: &str);
    fn send(&mut self, text: String);
    fn poll(&mut self) -> Vec<LiveSocketEvent>;
    fn close(&mut self);
    /// The server this page came from (web), as the dialog's default.
    fn page_origin(&self) -> Option<String> {
        None
    }
}

/// How the live session stands, for the status bar and the dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveStatus {
    /// Asking the server to create or join.
    Requesting,
    Connecting,
    Live,
    /// The connection dropped; trying again at the given time. Changes are kept and sent then.
    Reconnecting(Instant),
    /// It ended (removed by the host, session ended, refused); nothing more happens.
    Ended(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Request {
    Create,
    Join,
}

/// The app's live session.
pub struct Live {
    pub server: String,
    pub status: LiveStatus,
    /// Shown to the host: the access code and invitation link.
    pub code: Option<String>,
    pub invite: Option<String>,
    pub dashboard: Option<String>,
    pub client: Option<Client>,
    /// The shared presentation (a document uid).
    pub uid: Option<u64>,
    /// The host shares this open presentation: the server's copy is rebased onto it.
    bind: Option<u64>,
    request: Option<Request>,
    ws_url: String,
    last_local: Instant,
    last_slide: Option<usize>,
    backoff: Duration,
    pub error: Option<String>,
}

/// How often the user's changes are looked for and sent.
const SEND_EVERY: Duration = Duration::from_millis(80);

impl Live {
    fn new(server: &str) -> Live {
        Live {
            server: server.trim().trim_end_matches('/').to_string(),
            status: LiveStatus::Requesting,
            code: None,
            invite: None,
            dashboard: None,
            client: None,
            uid: None,
            bind: None,
            request: None,
            ws_url: String::new(),
            last_local: Instant::now(),
            last_slide: None,
            backoff: Duration::from_secs(1),
            error: None,
        }
    }

    /// Look for the user's changes on the next frame instead of waiting for the send interval.
    pub fn force_send_now(&mut self) {
        self.last_local = Instant::now().checked_sub(SEND_EVERY).unwrap_or_else(Instant::now);
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.status, LiveStatus::Ended(_))
    }

    pub fn online(&self) -> usize {
        self.client.as_ref().map(|c| c.peers.iter().filter(|p| p.online).count()).unwrap_or(0)
    }

    pub fn role(&self) -> Option<Role> {
        self.client.as_ref().and_then(|c| c.you.as_ref()).map(|p| p.role)
    }

    /// A short status for the status bar.
    pub fn badge(&self) -> (String, bool) {
        match &self.status {
            LiveStatus::Requesting | LiveStatus::Connecting => ("Live: connecting…".into(), false),
            LiveStatus::Live => {
                let n = self.online();
                let pending = self.client.as_ref().map(|c| c.unconfirmed()).unwrap_or(0);
                let viewer = if self.role() == Some(Role::Viewer) { " · view only" } else { "" };
                let sync = if pending > 0 { " · syncing" } else { "" };
                (format!("Live · {n} {}{viewer}{sync}", if n == 1 { "person" } else { "people" }), true)
            }
            LiveStatus::Reconnecting(_) => ("Live: offline, reconnecting…".into(), false),
            LiveStatus::Ended(r) => (format!("Live session ended ({r})"), false),
        }
    }
}

fn client_id(token: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    format!("c{:016x}", h.finish())
}

fn ws_url(server: &str, path: &str) -> String {
    let base = if let Some(rest) = server.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = server.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{server}")
    };
    format!("{base}{path}")
}

fn url_part(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Start a session on `server` with the open presentation; the host is `name`.
pub fn start(app: &mut SlideApp, server: &str, name: &str) -> Result<(), String> {
    let st = app.session.active().ok_or("Open a presentation first")?;
    let bytes = deckcraft_format::save(&st.doc).map_err(|e| e.to_string())?;
    let title = st.title();
    let uid = st.uid;
    let net = app.services.live_net.as_mut().ok_or("Live sessions aren't available in this build")?;
    let mut live = Live::new(server);
    net.post(&format!("{}/api/sessions?name={}&host={}", live.server, url_part(&title), url_part(name)), "application/octet-stream", bytes);
    live.request = Some(Request::Create);
    live.bind = Some(uid);
    app.live = Some(live);
    Ok(())
}

/// Join the session with `code` on `server` as `name`.
pub fn join(app: &mut SlideApp, server: &str, name: &str, code: &str) -> Result<(), String> {
    let net = app.services.live_net.as_mut().ok_or("Live sessions aren't available in this build")?;
    let mut live = Live::new(server);
    let body = serde_json::json!({"name": name, "code": code}).to_string().into_bytes();
    net.post(&format!("{}/api/join", live.server), "application/json", body);
    live.request = Some(Request::Join);
    app.live = Some(live);
    Ok(())
}

/// Join with a token the server already gave (the web join page passes it to the editor).
pub fn join_with_token(app: &mut SlideApp, server: &str, token: &str) {
    let mut live = Live::new(server);
    live.ws_url = ws_url(&live.server, "/ws");
    live.client = Some(Client::new(client_id(token), token.to_string()));
    live.status = LiveStatus::Connecting;
    if let Some(net) = app.services.live_net.as_mut() {
        net.connect(&live.ws_url);
    }
    app.live = Some(live);
}

/// `live.status`: how the session stands, for automation and tests.
pub fn status_json(app: &SlideApp) -> Value {
    let Some(live) = app.live.as_ref() else { return serde_json::json!({"status": "none"}) };
    let status = match &live.status {
        LiveStatus::Requesting => "requesting".to_string(),
        LiveStatus::Connecting => "connecting".to_string(),
        LiveStatus::Live => "live".to_string(),
        LiveStatus::Reconnecting(_) => "reconnecting".to_string(),
        LiveStatus::Ended(r) => format!("ended: {r}"),
    };
    let people: Vec<Value> = live
        .client
        .as_ref()
        .map(|c| c.peers.iter().map(|p| serde_json::json!({"name": p.name, "role": p.role.as_str(), "online": p.online})).collect())
        .unwrap_or_default();
    serde_json::json!({
        "status": status,
        "code": live.code,
        "invite": live.invite,
        "role": live.role().map(Role::as_str),
        "people": people,
        "unconfirmed": live.client.as_ref().map(|c| c.unconfirmed()).unwrap_or(0),
        "error": live.error,
    })
}

/// Leave the session. The shared presentation stays open as an ordinary copy.
pub fn leave(app: &mut SlideApp) {
    if let Some(net) = app.services.live_net.as_mut() {
        net.close();
    }
    if let Some(live) = app.live.as_mut() {
        live.status = LiveStatus::Ended("you left".into());
    }
}

fn handle_http(app: &mut SlideApp, result: Result<(u16, String), String>) {
    let Some(live) = app.live.as_mut() else { return };
    let request = live.request.take();
    let answer: Value = match &result {
        Ok((_, body)) => serde_json::from_str(body).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let token = answer.get("token").and_then(Value::as_str).map(str::to_string);
    let token = match (result, token) {
        (Ok((200, _)), Some(t)) => t,
        (Ok((status, _)), _) => {
            let msg = answer.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("the server answered {status}"));
            live.status = LiveStatus::Ended(msg.clone());
            live.error = Some(msg.clone());
            app.notify(format!("Live session: {msg}"));
            return;
        }
        (Err(e), _) => {
            let msg = format!("Can't reach {}: {e}", live.server);
            live.status = LiveStatus::Ended("not connected".into());
            live.error = Some(msg);
            return;
        }
    };
    if request == Some(Request::Create) {
        live.code = answer.get("code").and_then(Value::as_str).map(str::to_string);
        live.invite = answer.get("inviteUrl").and_then(Value::as_str).map(str::to_string);
        live.dashboard = answer.get("dashboardUrl").and_then(Value::as_str).map(str::to_string);
    }
    let path = answer.get("wsPath").and_then(Value::as_str).filter(|p| p.starts_with('/')).unwrap_or("/ws");
    live.ws_url = ws_url(&live.server, path);
    live.client = Some(Client::new(client_id(&token), token));
    live.status = LiveStatus::Connecting;
    let url = live.ws_url.clone();
    if let Some(net) = app.services.live_net.as_mut() {
        net.connect(&url);
    }
}

/// Apply a patch from the session to the shared presentation and tell the client.
fn apply_patch(app: &mut SlideApp, uid: u64, patch: &Patch) {
    if patch.is_empty() {
        return;
    }
    app.session.apply_remote(uid, &|p| apply(p, patch));
    let doc = app.session.documents().iter().find(|d| d.uid == uid).map(|d| d.doc.clone());
    if let (Some(doc), Some(client)) = (doc, app.live.as_mut().and_then(|l| l.client.as_mut())) {
        client.adopt(doc);
    }
}

fn shared_doc(app: &SlideApp, uid: Option<u64>) -> Option<Arc<deckcraft_model::Presentation>> {
    let uid = uid?;
    app.session.documents().iter().find(|d| d.uid == uid).map(|d| d.doc.clone())
}

fn handle_events(app: &mut SlideApp, events: Vec<Event>) {
    for ev in events {
        match ev {
            Event::Joined(doc) => {
                let bind = app.live.as_ref().and_then(|l| l.bind).filter(|u| app.session.documents().iter().any(|d| d.uid == *u));
                let uid = match bind {
                    // The host's open presentation becomes the shared one: same file, same history.
                    Some(uid) => {
                        if let Some(cur) = shared_doc(app, Some(uid)) {
                            let change = diff(Some(&cur), &doc);
                            app.session.apply_remote(uid, &|p| apply(p, &change));
                        }
                        uid
                    }
                    None => {
                        let title =
                            app.live.as_ref().and_then(|l| l.client.as_ref()).map(|c| c.title.clone()).unwrap_or_else(|| "Live presentation".into());
                        let i = app.session.add_document(DocState::new(*doc, None, title));
                        app.session.documents().get(i).map(|d| d.uid).unwrap_or_default()
                    }
                };
                if let Some(live) = app.live.as_mut() {
                    live.uid = Some(uid);
                }
                if let Some(i) = app.session.documents().iter().position(|d| d.uid == uid) {
                    app.session.set_active(i);
                }
                keep_ids_in_block(app);
                let doc = shared_doc(app, Some(uid));
                if let (Some(doc), Some(client)) = (doc, app.live.as_mut().and_then(|l| l.client.as_mut())) {
                    client.adopt(doc);
                }
                app.set_status("Live session: connected");
            }
            Event::Apply(patch) => {
                if let Some(uid) = app.live.as_ref().and_then(|l| l.uid) {
                    apply_patch(app, uid, &patch);
                }
            }
            Event::Presence => {}
            Event::Role(role) => app.set_status(match role {
                Role::Viewer => "Live session: you can now only view",
                _ => "Live session: you can edit again",
            }),
            Event::Rejected(reason) => app.set_status(format!("Live session: a change was refused ({reason}) and undone")),
            Event::Closed(reason) => {
                let msg = match reason.as_str() {
                    "kicked" => "The host removed you from the session".to_string(),
                    "ended" => "The host ended the session".to_string(),
                    "unauthorized" => "The invitation is no longer valid".to_string(),
                    "replaced" => "This session was opened elsewhere".to_string(),
                    other => other.to_string(),
                };
                if let Some(net) = app.services.live_net.as_mut() {
                    net.close();
                }
                if let Some(live) = app.live.as_mut() {
                    live.status = LiveStatus::Ended(msg.clone());
                }
                app.notify(format!("Live session: {msg}. Your copy stays open."));
            }
        }
    }
}

/// New slides and shapes take ids from this connection's block, so they never clash with ids
/// other people create at the same moment.
fn keep_ids_in_block(app: &mut SlideApp) {
    let Some(live) = app.live.as_ref() else { return };
    let (Some(uid), Some(ids)) = (live.uid, live.client.as_ref().and_then(|c| c.ids)) else { return };
    let out = app.session.documents().iter().find(|d| d.uid == uid).is_some_and(|d| !ids.contains(d.doc.next_id.saturating_add(1)));
    if out {
        // The highest id this block already holds in the document (after a reconnect).
        let used = shared_doc(app, Some(uid)).map(|d| max_id_in(&d, ids)).unwrap_or(ids.start);
        app.session.adjust_document(uid, |p| p.next_id = used.max(ids.start));
    }
}

fn max_id_in(p: &deckcraft_model::Presentation, ids: deckcraft_collab::proto::IdBlock) -> u32 {
    let mut m = ids.start;
    for s in &p.slides {
        if ids.contains(s.id.0) {
            m = m.max(s.id.0);
        }
        deckcraft_model::walk(&s.shapes, &mut |sh, _| {
            if ids.contains(sh.id.0) {
                m = m.max(sh.id.0);
            }
        });
    }
    for md in &p.media {
        if ids.contains(md.id.0) {
            m = m.max(md.id.0);
        }
    }
    m
}

/// Each frame: network results, other people's changes, then the user's own.
pub fn tick(app: &mut SlideApp, ctx: &egui::Context) {
    let Some(live) = app.live.as_ref() else { return };
    if !live.is_active() {
        return;
    }
    let Some(net) = app.services.live_net.as_mut() else { return };
    let http = net.poll_http();
    let events = net.poll();
    if let Some(r) = http {
        handle_http(app, r);
    }
    for ev in events {
        match ev {
            LiveSocketEvent::Open => {
                let mut first = false;
                if let Some(live) = app.live.as_mut() {
                    live.backoff = Duration::from_secs(1);
                    if let Some(c) = live.client.as_mut() {
                        c.connected();
                    }
                    first = live.uid.is_none();
                }
                if first {
                    // The whole presentation (pictures included) comes next; on a slow network or
                    // with many photos that takes a while, and there is only the start screen.
                    app.notify("Live session: connected. Loading the presentation…");
                }
            }
            LiveSocketEvent::Text(text) => {
                let uid = app.live.as_ref().and_then(|l| l.uid);
                let doc = shared_doc(app, uid);
                let Some(client) = app.live.as_mut().and_then(|l| l.client.as_mut()) else { continue };
                match client.receive(&text, doc.as_ref()) {
                    Ok(events) => {
                        if client.is_live()
                            && let Some(l) = app.live.as_mut()
                        {
                            l.status = LiveStatus::Live;
                        }
                        handle_events(app, events);
                    }
                    Err(e) => log::warn!("live session: {e}"),
                }
            }
            LiveSocketEvent::Closed(why) => {
                if let Some(live) = app.live.as_mut()
                    && live.is_active()
                {
                    if let Some(c) = live.client.as_mut() {
                        c.disconnected();
                    }
                    log::info!("live session: connection closed ({why}); retrying in {:?}", live.backoff);
                    live.status = LiveStatus::Reconnecting(Instant::now() + live.backoff);
                    live.backoff = (live.backoff * 2).min(Duration::from_secs(10));
                    // Before the presentation arrived there is only the start screen: say why.
                    if live.uid.is_none() {
                        let server = live.server.clone();
                        app.notify(format!("Live session: can't connect to {server} yet ({why}). Retrying… Check that this device is on the same network as the host and that the server is running."));
                    }
                }
            }
        }
    }
    // Reconnect when it's time.
    if let Some(live) = app.live.as_mut()
        && let LiveStatus::Reconnecting(at) = live.status
        && Instant::now() >= at
    {
        live.status = LiveStatus::Connecting;
        let url = live.ws_url.clone();
        if let Some(net) = app.services.live_net.as_mut() {
            net.connect(&url);
        }
    }
    // The user's own changes.
    let due = app.live.as_ref().is_some_and(|l| l.last_local.elapsed() >= SEND_EVERY && l.uid.is_some());
    if due {
        keep_ids_in_block(app);
        let uid = app.live.as_ref().and_then(|l| l.uid);
        // A gesture in progress is sent as it goes; an input method's composition when it ends.
        let composing = uid.and_then(|u| app.session.documents().iter().find(|d| d.uid == u)).is_some_and(|d| d.composition.is_some());
        if let (Some(uid), Some(doc), false) = (uid, shared_doc(app, uid), composing) {
            let revert = app.live.as_mut().and_then(|l| l.client.as_mut()).and_then(|c| c.local(&doc));
            if let Some(revert) = revert {
                apply_patch(app, uid, &revert);
                app.set_status("This live session is view only");
            }
        }
        let slide = uid.and_then(|u| app.session.documents().iter().find(|d| d.uid == u)).map(|d| d.selection.slide);
        if let Some(live) = app.live.as_mut() {
            live.last_local = Instant::now();
            if slide != live.last_slide
                && let (Some(s), Some(c)) = (slide, live.client.as_mut())
            {
                c.presence(s);
                live.last_slide = slide;
            }
        }
    }
    // Send.
    let out = app.live.as_mut().and_then(|l| l.client.as_mut()).map(|c| c.take_outbox()).unwrap_or_default();
    if let Some(net) = app.services.live_net.as_mut() {
        for m in out {
            net.send(m);
        }
    }
    // Keep listening while idle.
    ctx.request_repaint_after(Duration::from_millis(120));
}

/// The Live Session dialog. Returns `true` to close it.
pub fn dialog(app: &mut SlideApp, ui: &mut Ui, d: &mut Dialog) -> bool {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(420.0);
    if app.services.live_net.is_none() {
        ui.label("Live sessions aren't available in this build.");
        return ui.button("Close").clicked();
    }
    let active = app.live.as_ref().is_some_and(Live::is_active);
    if active {
        return session_view(app, ui, t);
    }
    if let Some(err) = app.live.as_ref().and_then(|l| l.error.clone()) {
        ui.label(RichText::new(err).color(Color32::from_rgb(200, 50, 40)));
        ui.add_space(6.0);
    }
    let default_server = app.services.live_net.as_ref().and_then(|n| n.page_origin()).unwrap_or_else(|| "http://localhost:8080".into());
    let mut server = d.fields.entry("server".into()).or_insert(default_server).clone();
    let mut name = d.fields.entry("name".into()).or_insert_with(|| app.session.prefs.author.clone()).clone();
    let mut code = d.fields.get("code").cloned().unwrap_or_default();
    egui::Grid::new("live_fields").num_columns(2).spacing(vec2(8.0, 8.0)).show(ui, |ui| {
        let l = ui.label("Server");
        ui.add(egui::TextEdit::singleline(&mut server).desired_width(280.0)).labelled_by(l.id);
        ui.end_row();
        let l = ui.label("Your name");
        ui.add(egui::TextEdit::singleline(&mut name).desired_width(280.0)).labelled_by(l.id);
        ui.end_row();
    });
    ui.add_space(10.0);
    let mut close = false;
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Share this presentation").strong());
        ui.label(
            RichText::new("Starts a session on the server with the open presentation. You get an access code to give to others.").color(t.text_dim),
        );
        if ui.add_enabled(app.session.active().is_some(), egui::Button::new("Start live session").min_size(vec2(160.0, 30.0))).clicked() {
            match start(app, &server, &name) {
                Ok(()) => {}
                Err(e) => app.set_status(e),
            }
        }
    });
    ui.add_space(8.0);
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Join a session").strong());
        ui.horizontal(|ui| {
            let l = ui.label("Access code");
            ui.add(egui::TextEdit::singleline(&mut code).desired_width(160.0).hint_text("ABCDE-FGHJK")).labelled_by(l.id);
            if ui.add_enabled(!code.trim().is_empty() && !name.trim().is_empty(), egui::Button::new("Join").min_size(vec2(80.0, 30.0))).clicked()
                && let Err(e) = join(app, &server, &name, &code)
            {
                app.set_status(e);
            }
        });
    });
    d.fields.insert("server".into(), server);
    d.fields.insert("name".into(), name);
    d.fields.insert("code".into(), code);
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    close
}

fn session_view(app: &mut SlideApp, ui: &mut Ui, t: Tokens) -> bool {
    let mut close = false;
    let mut leave_now = false;
    if let Some(live) = app.live.as_ref() {
        let (badge, ok) = live.badge();
        ui.label(RichText::new(badge).color(if ok { t.accent } else { t.text_dim }).strong());
        if let Some(c) = &live.client
            && !c.title.is_empty()
        {
            ui.label(format!("Session: {}", c.title));
        }
        ui.add_space(6.0);
        if let Some(code) = &live.code {
            ui.label("Access code");
            ui.add(egui::Label::new(RichText::new(code).size(30.0).monospace().strong()).selectable(true));
            ui.horizontal(|ui| {
                if ui.button("Copy code").clicked() {
                    ui.ctx().copy_text(code.clone());
                }
                if let Some(link) = &live.invite
                    && ui.button("Copy invitation link").clicked()
                {
                    ui.ctx().copy_text(link.clone());
                }
                if let Some(dash) = &live.dashboard
                    && ui.button("Open host dashboard").clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(dash.clone()));
                }
            });
            if let Some(link) = &live.invite {
                ui.add(egui::Label::new(RichText::new(link).monospace().size(11.0).color(t.text_dim)).selectable(true));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    qr_code(ui, link, 180.0);
                    ui.label(RichText::new("Scan with a phone or tablet\nto join this session.").color(t.text_dim));
                });
            }
            ui.add_space(6.0);
        }
        if let Some(c) = &live.client {
            ui.label(RichText::new("People").strong());
            for p in &c.peers {
                ui.horizontal(|ui| {
                    let colour = if p.online { Color32::from_rgb(40, 160, 80) } else { t.text_dim };
                    ui.label(RichText::new("●").color(colour));
                    let you = c.you.as_ref().is_some_and(|y| y.id == p.id);
                    ui.label(format!("{}{}", p.name, if you { " (you)" } else { "" }));
                    ui.label(RichText::new(p.role.as_str()).color(t.text_dim));
                    if let Some(s) = p.slide {
                        ui.label(RichText::new(format!("slide {}", s.saturating_add(1))).color(t.text_dim));
                    }
                });
            }
        }
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Leave session").clicked() {
            leave_now = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if leave_now {
        leave(app);
    }
    close
}

/// `text` as a QR code `size` points wide, dark on a white quiet zone (readable in dark mode too).
fn qr_code(ui: &mut Ui, text: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else { return };
    let width = code.width();
    let colors = code.to_colors();
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::WHITE);
    // Four modules of quiet zone on each side.
    let cells = width + 8;
    let m = size / cells as f32;
    for (i, c) in colors.iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = ((i % width + 4) as f32, (i / width + 4) as f32);
            let r = egui::Rect::from_min_size(rect.min + vec2(x * m, y * m), vec2(m + 0.3, m + 0.3));
            painter.rect_filled(r, 0.0, Color32::BLACK);
        }
    }
}
