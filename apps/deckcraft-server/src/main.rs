//! DeckCraft live sessions: one server for the collaboration socket, the host dashboard, the join
//! page and the browser editor's files.
//!
//! - `/`: join page (name + access code), `/join` (form post).
//! - `/host`: the host dashboard: create sessions, see the access code, invite link and
//!   participants, change roles, remove people, end a session, download the presentation.
//! - `/app/`: the browser editor (the `deckcraft-web` build, `DECKCRAFT_WEB_DIR`).
//! - `/api/sessions` (host), `/api/join`: the JSON API the desktop and web apps use.
//! - `/ws`: the live session socket (`deckcraft_collab::proto`).
//!
//! Configuration is read from the environment; see `docs/collaboration.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod auth;
mod live;
mod pages;
mod store;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use deckcraft_collab::proto::{MAX_MESSAGE_BYTES, Role};
use deckcraft_collab::server::Shared;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::store::{Info, Participant, Session};

/// Largest presentation a host may upload.
pub const MAX_UPLOAD_BYTES: usize = 512 << 20;
/// How long participant tokens last.
const TOKEN_DAYS: i64 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Development,
    Production,
}

pub struct Config {
    pub mode: Mode,
    pub bind: SocketAddr,
    pub data: PathBuf,
    pub web_dir: PathBuf,
    pub admin_password: Option<String>,
    pub public_url: Option<String>,
    pub allowed_origins: Vec<String>,
    /// Requests from this computer (to `localhost`) may use the host dashboard without a password.
    pub trust_loopback: bool,
}

pub struct App {
    pub cfg: Config,
    pub secret: Vec<u8>,
    pub sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    pub limiter: Mutex<auth::Limiter>,
    pub next_conn: AtomicU64,
}

impl App {
    pub async fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.get(id).cloned()
    }

    /// The dashboard's CSRF value (a page on another site can't read it).
    pub fn csrf(&self) -> String {
        auth::tag(&self.secret, "csrf")
    }

    /// Where people on the network reach this server.
    pub fn base_url(&self) -> String {
        if let Some(u) = &self.cfg.public_url {
            return u.trim_end_matches('/').to_string();
        }
        let ip = if self.cfg.bind.ip().is_unspecified() { lan_ip().unwrap_or(IpAddr::from([127, 0, 0, 1])) } else { self.cfg.bind.ip() };
        format!("http://{}", SocketAddr::new(ip, self.cfg.bind.port()))
    }

    pub fn token_for(&self, sid: &str, pid: &str) -> String {
        auth::sign(&self.secret, &auth::Claims { sid: sid.into(), pid: pid.into(), exp: auth::now() + TOKEN_DAYS * 86_400 })
    }
}

/// This computer's address on the local network (no packet is sent).
pub fn lan_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    Some(s.local_addr().ok()?.ip())
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn config() -> Result<Config, String> {
    let mode = match env("DECKCRAFT_MODE").as_deref() {
        None | Some("development") | Some("dev") => Mode::Development,
        Some("production") | Some("prod") => Mode::Production,
        Some(other) => return Err(format!("DECKCRAFT_MODE must be development or production, not {other}")),
    };
    let bind: SocketAddr = env("DECKCRAFT_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()).parse().map_err(|e| format!("DECKCRAFT_BIND: {e}"))?;
    let admin_password = env("DECKCRAFT_ADMIN_PASSWORD");
    if mode == Mode::Production && admin_password.as_ref().is_none_or(|p| p.chars().count() < 12) {
        return Err("production mode needs DECKCRAFT_ADMIN_PASSWORD with at least 12 characters".into());
    }
    let public_url = env("DECKCRAFT_PUBLIC_URL");
    if mode == Mode::Production && !public_url.as_deref().is_some_and(|u| u.starts_with("https://")) {
        return Err("production mode needs DECKCRAFT_PUBLIC_URL=https://… (put the server behind a TLS proxy; the socket is then wss://)".into());
    }
    Ok(Config {
        mode,
        bind,
        data: env("DECKCRAFT_DATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("deckcraft-data")),
        web_dir: env("DECKCRAFT_WEB_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("dist/web")),
        admin_password,
        public_url,
        allowed_origins: env("DECKCRAFT_ALLOWED_ORIGINS")
            .map(|v| v.split(',').map(|s| s.trim().trim_end_matches('/').to_string()).collect())
            .unwrap_or_default(),
        trust_loopback: match env("DECKCRAFT_TRUST_LOOPBACK").as_deref() {
            Some("0") | Some("false") => false,
            Some(_) => true,
            None => mode == Mode::Development,
        },
    })
}

/// The signing secret: `DECKCRAFT_JWT_SECRET`, or a random one kept in `<data>/secret.key`.
fn secret(cfg: &Config) -> Result<Vec<u8>, String> {
    if let Some(s) = env("DECKCRAFT_JWT_SECRET") {
        if s.len() < 32 {
            return Err("DECKCRAFT_JWT_SECRET must have at least 32 characters".into());
        }
        return Ok(s.into_bytes());
    }
    let path = cfg.data.join("secret.key");
    if let Ok(b) = std::fs::read(&path)
        && b.len() >= 32
    {
        return Ok(b);
    }
    let key = auth::random_bytes::<32>().to_vec();
    store::write_atomic(&path, &key).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{}", include_str!("help.txt"));
        return;
    }
    let cfg = match config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("deckcraft-server: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(cfg.data.join("sessions")) {
        eprintln!("deckcraft-server: can't create the data folder {}: {e}", cfg.data.display());
        std::process::exit(2);
    }
    let secret = match secret(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("deckcraft-server: {e}");
            std::process::exit(2);
        }
    };
    let sessions: BTreeMap<String, Arc<Session>> =
        store::load_all(&cfg.data).into_iter().filter_map(|s| s.info.try_lock().ok().map(|i| i.id.clone()).map(|id| (id, s.clone()))).collect();
    let bind = cfg.bind;
    let app = Arc::new(App { cfg, secret, sessions: Mutex::new(sessions), limiter: Mutex::default(), next_conn: AtomicU64::new(1) });
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("deckcraft-server: can't listen on {bind}: {e} (is another server running? set DECKCRAFT_BIND)");
            std::process::exit(2);
        }
    };
    banner(&app);
    tokio::spawn(saver(app.clone()));
    let router = router(app.clone());
    let served = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    if let Err(e) = served.await {
        eprintln!("deckcraft-server: {e}");
    }
    save_all(&app).await;
    println!("deckcraft-server: stopped, everything saved.");
}

fn banner(app: &App) {
    let base = app.base_url();
    println!("DeckCraft live sessions {}", env!("CARGO_PKG_VERSION"));
    println!("  mode:       {}", if app.cfg.mode == Mode::Production { "production" } else { "development (for a trusted local network only)" });
    println!("  data:       {}", app.cfg.data.display());
    println!("  host:       http://localhost:{}/host   (on this computer)", app.cfg.bind.port());
    println!("  join:       {base}/");
    if !app.cfg.web_dir.join("index.html").exists() {
        println!("  web editor: not found in {} (build it: cd apps/deckcraft-web && trunk build --release)", app.cfg.web_dir.display());
    }
}

/// Write changed documents every two seconds and back them up every ten minutes.
async fn saver(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut n: u64 = 0;
    loop {
        tick.tick().await;
        n = n.wrapping_add(1);
        let sessions: Vec<Arc<Session>> = app.sessions.lock().await.values().cloned().collect();
        for s in sessions {
            if let Err(e) = s.save_shared().await {
                eprintln!("deckcraft-server: saving {} failed: {e}", s.dir.display());
            }
            if n.is_multiple_of(300)
                && let Err(e) = s.backup().await
            {
                eprintln!("deckcraft-server: backup of {} failed: {e}", s.dir.display());
            }
        }
    }
}

async fn save_all(app: &App) {
    for s in app.sessions.lock().await.values() {
        let _ = s.save_shared().await;
        let _ = s.save_info().await;
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(pages::landing))
        .route("/join", post(join_form))
        .route("/healthz", get(|| async { "ok" }))
        .route("/host", get(pages::dashboard))
        .route("/host/login", get(pages::login).post(login))
        .route("/host/create", post(create_form).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
        .route("/host/s/{id}", get(pages::session_page))
        .route("/host/s/{id}/action", post(session_action))
        .route("/host/s/{id}/download/{file}", get(download))
        .route("/api/join", post(api_join))
        .route("/api/sessions", post(api_create).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
        .route("/ws", get(ws))
        .route("/app", get(|| async { Redirect::permanent("/app/") }))
        .route("/app/", get(static_file))
        .route("/app/{*path}", get(static_file))
        .layer(DefaultBodyLimit::max(64 << 10))
        .with_state(app)
}

type AppState = State<Arc<App>>;

/// The request comes from this computer to `localhost` (not through a name that merely points
/// here, which a web page could use to reach the dashboard).
fn is_loopback(addr: &SocketAddr, headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    addr.ip().is_loopback() && matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

/// May this request use the host dashboard and API?
pub fn is_host(app: &App, addr: &SocketAddr, headers: &HeaderMap) -> bool {
    if app.cfg.trust_loopback && is_loopback(addr, headers) {
        return true;
    }
    if let Some(t) = cookie(headers, "dc_admin")
        && auth::verify(&app.secret, t).is_some_and(|c| c.sid == "*")
    {
        return true;
    }
    if let (Some(pw), Some(given)) =
        (&app.cfg.admin_password, headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer ")))
    {
        return auth::same(pw, given);
    }
    false
}

/// A browser request from another site: refused unless that origin is allowed.
fn foreign_origin(app: &App, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) else { return false };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let own = origin.split_once("://").map(|(_, h)| h) == Some(host);
    !own && !app.cfg.allowed_origins.iter().any(|o| o == origin) && app.cfg.public_url.as_deref().map(|u| u.trim_end_matches('/')) != Some(origin)
}

pub fn error_page(status: StatusCode, title: &str, msg: &str) -> Response {
    (status, Html(pages::page(title, &format!("<h1>{}</h1><p class=err>{}</p><p><a href='/'>Back</a></p>", auth::html(title), auth::html(msg)))))
        .into_response()
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// Add a participant to the session with `code`. `Err` says why (shown to the person).
async fn join_session(app: &App, ip: IpAddr, name: &str, code: &str) -> Result<(Arc<Session>, String, Role), (StatusCode, String)> {
    if !app.limiter.lock().await.allowed(ip) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Too many wrong codes from this device. Wait ten minutes and try again.".into()));
    }
    let Some(name) = auth::clean_name(name) else {
        return Err((StatusCode::BAD_REQUEST, "Please enter your name.".into()));
    };
    let wanted = auth::normalize_code(code);
    let mut found = None;
    for s in app.sessions.lock().await.values() {
        let info = s.info.lock().await;
        if !info.ended && auth::same(&auth::normalize_code(&info.code), &wanted) {
            found = Some(s.clone());
        }
    }
    let Some(session) = found else {
        app.limiter.lock().await.failed(ip);
        return Err((StatusCode::FORBIDDEN, "That access code doesn't match an open session. Check it with the host.".into()));
    };
    let pid = auth::random_id();
    let role = {
        let mut info = session.info.lock().await;
        if info.participants.len() >= 500 {
            return Err((StatusCode::FORBIDDEN, "This session is full.".into()));
        }
        let role = if info.join_as_editor { Role::Editor } else { Role::Viewer };
        info.participants.insert(pid.clone(), Participant { id: pid.clone(), name, role, removed: false, last_seen: 0 });
        role
    };
    let _ = session.save_info().await;
    session.broadcast_presence().await;
    Ok((session, pid, role))
}

#[derive(Deserialize)]
struct JoinForm {
    name: String,
    code: String,
}

async fn join_form(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::Form(f): axum::Form<JoinForm>,
) -> Response {
    if foreign_origin(&app, &headers) {
        return error_page(StatusCode::FORBIDDEN, "Not allowed", "This form can only be sent from this server's own page.");
    }
    match join_session(&app, addr.ip(), &f.name, &f.code).await {
        Ok((session, pid, _)) => {
            let sid = session.info.lock().await.id.clone();
            let token = app.token_for(&sid, &pid);
            // The token goes in the fragment: browsers never send it to a server or log it.
            Redirect::to(&format!("/app/#token={token}")).into_response()
        }
        Err((status, msg)) => (status, Html(pages::landing_html(&app, Some(&msg), &f.name, &f.code))).into_response(),
    }
}

#[derive(Deserialize)]
struct ApiJoin {
    name: String,
    code: String,
}

async fn api_join(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::Json(j): axum::Json<ApiJoin>,
) -> Response {
    if foreign_origin(&app, &headers) {
        return json_error(StatusCode::FORBIDDEN, "origin not allowed");
    }
    match join_session(&app, addr.ip(), &j.name, &j.code).await {
        Ok((session, pid, role)) => {
            let info = session.info.lock().await;
            axum::Json(serde_json::json!({
                "token": app.token_for(&info.id, &pid),
                "sessionId": info.id,
                "title": info.title,
                "role": role.as_str(),
                "wsPath": "/ws",
            }))
            .into_response()
        }
        Err((status, msg)) => json_error(status, &msg),
    }
}

/// Make a session from an uploaded presentation (or a new one) with its host.
async fn create_session(app: &App, title: &str, host_name: &str, bytes: &[u8]) -> Result<(Arc<Session>, String), String> {
    let doc = if bytes.is_empty() {
        let mut p = deckcraft_model::defaults::new_presentation(None);
        let layout = deckcraft_model::defaults::layout_of_kind(&p, deckcraft_model::LayoutType::Title)
            .or_else(|| p.masters.first().and_then(|m| m.layouts.first()).map(|l| l.id))
            .unwrap_or_default();
        if let Some(s) = deckcraft_model::defaults::new_slide(&mut p, layout) {
            p.slides.push(Arc::new(s));
        }
        p
    } else if deckcraft_format::sniff(bytes) {
        deckcraft_format::load(bytes).map_err(|e| format!("that DeckCraft file can't be opened: {e}"))?
    } else if deckcraft_pptx::sniff(bytes) {
        if deckcraft_pptx::is_encrypted(bytes) {
            return Err("that PowerPoint file is password protected".into());
        }
        deckcraft_pptx::import(bytes).map_err(|e| format!("that PowerPoint file can't be opened: {e}"))?
    } else {
        return Err("upload a .pptx or .deckcraft file (or nothing for a new presentation)".into());
    };
    let shared = Shared::from_presentation(&doc).map_err(|e| e.to_string())?;
    let id = auth::random_id();
    let host_pid = auth::random_id();
    let mut participants = BTreeMap::new();
    let host_name = auth::clean_name(host_name).unwrap_or_else(|| "Host".into());
    participants.insert(host_pid.clone(), Participant { id: host_pid.clone(), name: host_name, role: Role::Host, removed: false, last_seen: 0 });
    let title = auth::clean_name(title).unwrap_or_else(|| "Presentation".into());
    // Codes are unique among open sessions.
    let mut code = auth::new_code();
    {
        let sessions = app.sessions.lock().await;
        let mut taken = Vec::new();
        for s in sessions.values() {
            taken.push(auth::normalize_code(&s.info.lock().await.code));
        }
        while taken.contains(&auth::normalize_code(&code)) {
            code = auth::new_code();
        }
    }
    let info = Info { id: id.clone(), title, code, created: auth::now(), ended: false, join_as_editor: true, participants };
    let session = Arc::new(Session::new(app.cfg.data.join("sessions").join(&id), info, shared));
    session.dirty.store(true, std::sync::atomic::Ordering::Release);
    session.save_info().await.map_err(|e| format!("can't save the session: {e}"))?;
    session.save_shared().await.map_err(|e| format!("can't save the session: {e}"))?;
    app.sessions.lock().await.insert(id, session.clone());
    Ok((session, host_pid))
}

#[derive(Deserialize)]
struct CreateQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    host: String,
}

/// `POST /api/sessions?name=…&host=…` with an optional .deckcraft/.pptx body (the desktop app
/// shares the open presentation this way).
async fn api_create(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<CreateQuery>,
    body: axum::body::Bytes,
) -> Response {
    if !is_host(&app, &addr, &headers) || foreign_origin(&app, &headers) {
        return json_error(
            StatusCode::FORBIDDEN,
            "only the host may create sessions: run DeckCraft on the server's computer, or send the admin password",
        );
    }
    match create_session(&app, &q.name, &q.host, &body).await {
        Ok((session, pid)) => {
            let info = session.info.lock().await;
            axum::Json(serde_json::json!({
                "token": app.token_for(&info.id, &pid),
                "sessionId": info.id,
                "title": info.title,
                "code": info.code,
                "role": "host",
                "wsPath": "/ws",
                "inviteUrl": format!("{}/?code={}", app.base_url(), info.code),
                "dashboardUrl": format!("{}/host/s/{}", app.base_url(), info.id),
            }))
            .into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, &e),
    }
}

async fn create_form(State(app): AppState, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, mut form: Multipart) -> Response {
    if !is_host(&app, &addr, &headers) {
        return Redirect::to("/host/login").into_response();
    }
    let (mut title, mut host, mut csrf, mut bytes) = (String::new(), String::new(), String::new(), Vec::new());
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return error_page(StatusCode::BAD_REQUEST, "Upload failed", &e.to_string()),
        };
        let name = field.name().unwrap_or("").to_string();
        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => return error_page(StatusCode::BAD_REQUEST, "Upload failed", &e.to_string()),
        };
        match name.as_str() {
            "title" => title = String::from_utf8_lossy(&data).into_owned(),
            "host" => host = String::from_utf8_lossy(&data).into_owned(),
            "csrf" => csrf = String::from_utf8_lossy(&data).into_owned(),
            "file" => bytes = data.to_vec(),
            _ => {}
        }
    }
    if !auth::same(&csrf, &app.csrf()) {
        return error_page(StatusCode::FORBIDDEN, "Not allowed", "The form expired. Reload the dashboard and try again.");
    }
    match create_session(&app, &title, &host, &bytes).await {
        Ok((session, _)) => {
            let id = session.info.lock().await.id.clone();
            Redirect::to(&format!("/host/s/{id}")).into_response()
        }
        Err(e) => error_page(StatusCode::BAD_REQUEST, "Couldn't create the session", &e),
    }
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

async fn login(State(app): AppState, ConnectInfo(addr): ConnectInfo<SocketAddr>, axum::Form(f): axum::Form<LoginForm>) -> Response {
    if !app.limiter.lock().await.allowed(addr.ip()) {
        return error_page(StatusCode::TOO_MANY_REQUESTS, "Too many attempts", "Wait ten minutes and try again.");
    }
    let ok = app.cfg.admin_password.as_ref().is_some_and(|pw| auth::same(pw, &f.password));
    if !ok {
        app.limiter.lock().await.failed(addr.ip());
        return error_page(StatusCode::FORBIDDEN, "Wrong password", "The admin password is set on the server (DECKCRAFT_ADMIN_PASSWORD).");
    }
    let token = auth::sign(&app.secret, &auth::Claims { sid: "*".into(), pid: "admin".into(), exp: auth::now() + 12 * 3600 });
    let secure = if app.cfg.mode == Mode::Production { "; Secure" } else { "" };
    let mut r = Redirect::to("/host").into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("dc_admin={token}; HttpOnly; SameSite=Strict; Path=/{secure}")) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

#[derive(Deserialize)]
struct ActionForm {
    csrf: String,
    action: String,
    #[serde(default)]
    pid: String,
    #[serde(default)]
    role: String,
}

async fn session_action(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Form(f): axum::Form<ActionForm>,
) -> Response {
    if !is_host(&app, &addr, &headers) {
        return Redirect::to("/host/login").into_response();
    }
    if !auth::same(&f.csrf, &app.csrf()) || foreign_origin(&app, &headers) {
        return error_page(StatusCode::FORBIDDEN, "Not allowed", "The form expired. Reload the dashboard and try again.");
    }
    let Some(session) = app.session(&id).await else {
        return error_page(StatusCode::NOT_FOUND, "No such session", "It may have been deleted.");
    };
    match f.action.as_str() {
        "role" => {
            let Some(role) = Role::parse(&f.role).filter(|r| *r != Role::Host) else {
                return error_page(StatusCode::BAD_REQUEST, "Unknown role", &f.role);
            };
            let changed = {
                let mut info = session.info.lock().await;
                match info.participants.get_mut(&f.pid) {
                    Some(p) if p.role != Role::Host => {
                        p.role = role;
                        true
                    }
                    _ => false,
                }
            };
            if changed {
                let msg = deckcraft_collab::proto::ServerMsg::Role { role }.to_json();
                for c in session.conns.lock().await.values().filter(|c| c.pid == f.pid) {
                    let _ = c.tx.send(msg.clone());
                }
            }
        }
        "remove" => {
            let removed = {
                let mut info = session.info.lock().await;
                match info.participants.get_mut(&f.pid) {
                    Some(p) if p.role != Role::Host => {
                        p.removed = true;
                        true
                    }
                    _ => false,
                }
            };
            if removed {
                session.close(Some(&f.pid), "kicked").await;
            }
        }
        "joinmode" => session.info.lock().await.join_as_editor = f.role == "editor",
        "newcode" => {
            session.info.lock().await.code = auth::new_code();
        }
        "end" => {
            session.info.lock().await.ended = true;
            session.close(None, "ended").await;
        }
        "reopen" => session.info.lock().await.ended = false,
        "delete" => {
            session.close(None, "ended").await;
            app.sessions.lock().await.remove(&id);
            let trash = app.cfg.data.join("deleted");
            let _ = std::fs::create_dir_all(&trash);
            // Moved aside rather than erased: a mistaken click can be undone by hand.
            let _ = std::fs::rename(&session.dir, trash.join(format!("{id}-{}", auth::now())));
            return Redirect::to("/host").into_response();
        }
        other => return error_page(StatusCode::BAD_REQUEST, "Unknown action", other),
    }
    if let Err(e) = session.save_info().await {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't save", &e.to_string());
    }
    session.broadcast_presence().await;
    Redirect::to(&format!("/host/s/{id}")).into_response()
}

async fn download(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    if !is_host(&app, &addr, &headers) {
        return Redirect::to("/host/login").into_response();
    }
    let Some(session) = app.session(&id).await else {
        return error_page(StatusCode::NOT_FOUND, "No such session", "");
    };
    let doc = match session.shared.lock().await.materialize() {
        Ok(d) => d,
        Err(e) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "The presentation can't be read", &e.to_string()),
    };
    let title = session.info.lock().await.title.clone();
    let safe: String = title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let (bytes, mime, ext) = match file.as_str() {
        "deckcraft" => (deckcraft_format::save(&doc).map_err(|e| e.to_string()), "application/zip", "deckcraft"),
        "pptx" => (
            deckcraft_pptx::export(&doc).map_err(|e| e.to_string()),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "pptx",
        ),
        _ => return error_page(StatusCode::NOT_FOUND, "Unknown format", &file),
    };
    match bytes {
        Ok(b) => {
            let disposition = format!("attachment; filename=\"{}.{ext}\"", safe.trim());
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CONTENT_DISPOSITION, disposition)], b).into_response()
        }
        Err(e) => error_page(StatusCode::INTERNAL_SERVER_ERROR, "Export failed", &e),
    }
}

async fn ws(State(app): AppState, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if foreign_origin(&app, &headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    upgrade.max_message_size(MAX_MESSAGE_BYTES).max_frame_size(MAX_MESSAGE_BYTES).on_upgrade(move |socket| live::run(app, socket))
}

/// The browser editor's files, from `DECKCRAFT_WEB_DIR`.
async fn static_file(State(app): AppState, path: Option<Path<String>>) -> Response {
    let rel = path.map(|Path(p)| p).unwrap_or_default();
    let rel = if rel.is_empty() { "index.html".to_string() } else { rel };
    // Only plain names below the folder: no `..`, no absolute paths, no hidden files.
    if rel.split('/').any(|c| c.is_empty() || c.starts_with('.') || c.contains('\\')) || rel.len() > 200 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let file = app.cfg.web_dir.join(&rel);
    let Ok(bytes) = tokio::fs::read(&file).await else {
        if rel == "index.html" {
            return error_page(
                StatusCode::NOT_FOUND,
                "Web editor not built",
                "Build it with `trunk build --release` in apps/deckcraft-web, or set DECKCRAFT_WEB_DIR.",
            );
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    let ext = rel.rsplit('.').next().unwrap_or("");
    let mime = match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "ico" => "image/x-icon",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "json" | "webmanifest" => "application/json",
        _ => "application/octet-stream",
    };
    // Hashed file names never change; the page itself always revalidates.
    let cache = if ext == "html" { "no-cache" } else { "public, max-age=31536000, immutable" };
    ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)], bytes).into_response()
}

#[cfg(test)]
mod tests;
