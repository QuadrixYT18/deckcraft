//! The server's pages: plain HTML forms, no scripts. They work on phones, tablets and desktops.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use deckcraft_collab::proto::Role;
use serde::Deserialize;

use crate::auth::html;
use crate::{App, Mode, error_page, is_host};

const STYLE: &str = r#"
:root { --bg:#f5f5f7; --card:#fff; --text:#1d1d1f; --dim:#5f6368; --accent:#c43e1c; --line:#d9d9de; --ok:#1b7f3b; --err:#b3261e; }
@media (prefers-color-scheme: dark) { :root { --bg:#1e1e20; --card:#2a2a2d; --text:#ececee; --dim:#a1a1a6; --accent:#ff8a65; --line:#3c3c40; --ok:#6fcf8f; --err:#ff8a80; } }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--text); font:16px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif; }
main { max-width: 860px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 1.6rem; margin: .2em 0 .6em; } h2 { font-size: 1.15rem; margin: 1.4em 0 .5em; }
.card { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:18px; margin: 14px 0; }
label { display:block; font-weight:600; margin: 10px 0 4px; }
input[type=text], input[type=password], input[type=file], select { width:100%; font:inherit; padding:12px; border:1px solid var(--line); border-radius:8px; background:var(--bg); color:var(--text); }
button, .button { display:inline-block; font:inherit; font-weight:600; padding:11px 18px; border-radius:8px; border:0; background:var(--accent); color:#fff; cursor:pointer; text-decoration:none; min-height:44px; }
button.quiet, .button.quiet { background:transparent; color:var(--accent); border:1px solid var(--line); }
button.danger { background:var(--err); }
.code { font: 700 clamp(2rem, 9vw, 3.6rem)/1.1 ui-monospace,"SF Mono",Menlo,monospace; letter-spacing:.08em; user-select:all; -webkit-user-select:all; padding: 8px 0; }
.copy { width:100%; font:15px ui-monospace,Menlo,monospace; padding:10px; border:1px solid var(--line); border-radius:8px; background:var(--bg); color:var(--text); }
.dim { color:var(--dim); } .err { color:var(--err); font-weight:600; } .ok { color:var(--ok); font-weight:600; }
table { width:100%; border-collapse:collapse; } td, th { text-align:left; padding:8px 6px; border-bottom:1px solid var(--line); vertical-align:middle; }
form.inline { display:inline; } .row { display:flex; flex-wrap:wrap; gap:8px; align-items:center; }
.dot { display:inline-block; width:10px; height:10px; border-radius:50%; background:var(--dim); margin-right:6px; } .dot.on { background:var(--ok); }
.warn { border-left: 4px solid var(--accent); padding-left: 12px; }
"#;

pub fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width, initial-scale=1'>\
         <meta name=referrer content=same-origin><title>{} · DeckCraft</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>",
        html(title)
    )
}

#[derive(Deserialize, Default)]
pub struct LandingQuery {
    #[serde(default)]
    code: String,
}

pub async fn landing(State(app): State<Arc<App>>, Query(q): Query<LandingQuery>) -> Html<String> {
    Html(landing_html(&app, None, "", &q.code))
}

pub fn landing_html(_app: &App, error: Option<&str>, name: &str, code: &str) -> String {
    let err = error.map(|e| format!("<p class=err role=alert>{}</p>", html(e))).unwrap_or_default();
    page(
        "Join a presentation",
        &format!(
            "<h1>Join a presentation</h1>\
             <div class=card><form method=post action=/join>{err}\
             <label for=name>Your name</label><input type=text id=name name=name autocomplete=name maxlength=60 required value=\"{}\">\
             <label for=code>Access code</label><input type=text id=code name=code autocomplete=off autocapitalize=characters spellcheck=false maxlength=20 required placeholder='ABCDE-FGHJK' value=\"{}\">\
             <p><button type=submit>Join</button></p></form></div>\
             <p class=dim>The host sees the access code in DeckCraft or on the host dashboard. \
             In the DeckCraft app, use <b>Review › Live Session</b> instead.</p>\
             <p class=dim><a href=/host>Host dashboard</a></p>",
            html(name),
            html(code)
        ),
    )
}

pub async fn login(State(app): State<Arc<App>>) -> Html<String> {
    let note = if app.cfg.admin_password.is_none() {
        "<p class=warn>No admin password is set, so the dashboard only opens on the server's own computer at <code>http://localhost</code>. Set <code>DECKCRAFT_ADMIN_PASSWORD</code> to host from elsewhere.</p>"
    } else {
        ""
    };
    Html(page(
        "Host sign-in",
        &format!(
            "<h1>Host sign-in</h1>{note}<div class=card><form method=post action=/host/login>\
             <label for=pw>Admin password</label><input type=password id=pw name=password autocomplete=current-password required>\
             <p><button type=submit>Sign in</button></p></form></div>"
        ),
    ))
}

pub async fn dashboard(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if !is_host(&app, &addr, &headers) {
        return Redirect::to("/host/login").into_response();
    }
    let mut rows = String::new();
    let sessions: Vec<_> = app.sessions.lock().await.values().cloned().collect();
    let mut list = Vec::new();
    for s in sessions {
        let info = s.info.lock().await.clone();
        let online = s.conns.lock().await.len();
        list.push((info, online));
    }
    list.sort_by_key(|e| std::cmp::Reverse(e.0.created));
    for (info, online) in &list {
        rows.push_str(&format!(
            "<tr><td><a href='/host/s/{}'>{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html(&info.id),
            html(&info.title),
            if info.ended { "<span class=dim>ended</span>".to_string() } else { format!("<b>{}</b>", html(&info.code)) },
            online,
            when(info.created)
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td colspan=4 class=dim>No sessions yet.</td></tr>".into();
    }
    let mode = if app.cfg.mode == Mode::Development {
        "<p class='warn dim'>Development mode: for a trusted local network. See docs/collaboration.md before exposing this server to the internet.</p>"
    } else {
        ""
    };
    Html(page(
        "Host dashboard",
        &format!(
            "<h1>Host dashboard</h1>{mode}\
             <div class=card><h2 style='margin-top:0'>Start a session</h2>\
             <form method=post action=/host/create enctype=multipart/form-data>\
             <input type=hidden name=csrf value=\"{}\">\
             <label for=title>Session name</label><input type=text id=title name=title maxlength=60 required placeholder='Quarterly review'>\
             <label for=host>Your name</label><input type=text id=host name=host maxlength=60 placeholder='Host'>\
             <label for=file>Presentation (.pptx or .deckcraft, optional)</label><input type=file id=file name=file accept='.pptx,.deckcraft'>\
             <p><button type=submit>Create session</button></p></form></div>\
             <h2>Sessions</h2><div class=card><table><tr><th>Name</th><th>Access code</th><th>Online</th><th>Created</th></tr>{rows}</table></div>",
            html(&app.csrf())
        ),
    ))
    .into_response()
}

fn when(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default()
}

#[derive(Deserialize, Default)]
pub struct SessionQuery {
    #[serde(default)]
    live: Option<String>,
}

pub async fn session_page(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SessionQuery>,
) -> Response {
    if !is_host(&app, &addr, &headers) {
        return Redirect::to("/host/login").into_response();
    }
    let Some(session) = app.session(&id).await else {
        return error_page(StatusCode::NOT_FOUND, "No such session", "It may have been deleted.");
    };
    let info = session.info.lock().await.clone();
    let peers = session.peers().await;
    let (seq, saved) = (session.shared.lock().await.seq, session.saved_seq.load(std::sync::atomic::Ordering::Acquire));
    let csrf = html(&app.csrf());
    let base = app.base_url();
    let invite = format!("{base}/?code={}", info.code);
    let invite_plain = format!("{base}/");
    let host_pid = info.participants.values().find(|p| p.role == Role::Host).map(|p| p.id.clone()).unwrap_or_default();
    let editor = format!("/app/#token={}", app.token_for(&info.id, &host_pid));
    let mut people = String::new();
    for p in &peers {
        let role_form = if p.role == Role::Host {
            "host".to_string()
        } else {
            format!(
                "<form class=inline method=post action='/host/s/{id}/action'><input type=hidden name=csrf value=\"{csrf}\"><input type=hidden name=action value=role><input type=hidden name=pid value=\"{pid}\">\
                 <select name=role aria-label='Role of {name}'><option value=editor{e}>editor</option><option value=viewer{v}>viewer</option></select> <button class=quiet type=submit>Set</button></form>",
                id = html(&info.id),
                pid = html(&p.id),
                name = html(&p.name),
                e = if p.role == Role::Editor { " selected" } else { "" },
                v = if p.role == Role::Viewer { " selected" } else { "" },
            )
        };
        let remove = if p.role == Role::Host {
            String::new()
        } else {
            format!(
                "<form class=inline method=post action='/host/s/{}/action'><input type=hidden name=csrf value=\"{csrf}\"><input type=hidden name=action value=remove><input type=hidden name=pid value=\"{}\"><button class=danger type=submit>Remove</button></form>",
                html(&info.id),
                html(&p.id)
            )
        };
        people.push_str(&format!(
            "<tr><td><span class='dot{}'></span>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            if p.online { " on" } else { "" },
            html(&p.name),
            role_form,
            p.slide.map(|s| format!("slide {}", s.saturating_add(1))).unwrap_or_default(),
            remove
        ));
    }
    let sync = if saved >= seq {
        format!("<span class=ok>All changes saved</span> <span class=dim>(version {seq})</span>")
    } else {
        format!("<span class=dim>Saving… (version {seq}, saved {saved})</span>")
    };
    let refresh = if q.live.is_some() { "<meta http-equiv=refresh content=5>" } else { "" };
    let status = if info.ended { "<p class=err>This session has ended. People can't join or edit.</p>" } else { "" };
    let action = |name: &str, label: &str, class: &str, extra: &str| {
        format!(
            "<form class=inline method=post action='/host/s/{}/action'><input type=hidden name=csrf value=\"{csrf}\"><input type=hidden name=action value={name}>{extra}<button class='{class}' type=submit>{label}</button></form>",
            html(&info.id)
        )
    };
    let lifecycle = if info.ended { action("reopen", "Reopen session", "", "") } else { action("end", "End session", "danger", "") };
    let joinmode = if info.join_as_editor {
        action("joinmode", "New people join as viewers", "quiet", "<input type=hidden name=role value=viewer>")
    } else {
        action("joinmode", "New people join as editors", "quiet", "<input type=hidden name=role value=editor>")
    };
    let body = format!(
        "{refresh}<p><a href=/host>‹ All sessions</a></p><h1>{title}</h1>{status}\
         <div class=card><div class=dim>Access code</div><div class=code aria-label='Access code'>{code}</div>\
         <label for=inv>Invitation link (includes the code)</label><input class=copy id=inv type=text readonly value=\"{invite}\">\
         <label for=inv2>Join page (people type the code)</label><input class=copy id=inv2 type=text readonly value=\"{invite_plain}\">\
         <p class=dim>Select a field and copy it. Anyone with the code can join while the session is open.</p>\
         <div class=row>{newcode}{joinmode}</div></div>\
         <div class=card><div class=row><a class=button href=\"{editor}\">Open in browser editor</a>\
         <a class='button quiet' href='/host/s/{id}/download/pptx'>Download .pptx</a>\
         <a class='button quiet' href='/host/s/{id}/download/deckcraft'>Download .deckcraft</a></div>\
         <p>{sync}</p>\
         <form method=post action='/host/s/{id}/replace' enctype=multipart/form-data><input type=hidden name=csrf value=\"{csrf}\">\
         <label for=rep>Replace the shared presentation with a file (.pptx or .deckcraft)</label>\
         <input type=file id=rep name=file accept='.pptx,.deckcraft' required>\
         <p><button class=quiet type=submit>Replace presentation for everyone</button></p></form></div>\
         <h2>People ({n})</h2><div class=card><table><tr><th>Name</th><th>Role</th><th>Where</th><th></th></tr>{people}</table>\
         <p class=dim><a href='?live=1'>Refresh automatically</a> · <a href='?'>Stop refreshing</a></p></div>\
         <div class=row>{lifecycle}{delete}</div>",
        title = html(&info.title),
        code = html(&info.code),
        invite = html(&invite),
        invite_plain = html(&invite_plain),
        editor = html(&editor),
        id = html(&info.id),
        n = peers.len(),
        newcode = action("newcode", "New access code", "quiet", ""),
        delete = action("delete", "Delete session", "quiet", ""),
    );
    Html(page(&info.title, &body)).into_response()
}
