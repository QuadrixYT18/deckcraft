//! One WebSocket connection: authenticate with the first message, welcome, then relay ops.
//!
//! Lock order everywhere: a session's `info`, then `shared`, then `conns`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use deckcraft_collab::patch::Entry;
use deckcraft_collab::proto::{ClientMsg, IdBlock, Peer, ServerMsg};
use tokio::sync::mpsc;

use crate::App;
use crate::store::{Conn, Session};

/// How long a new socket may take to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const PING_EVERY: Duration = Duration::from_secs(25);

async fn send_closed(socket: &mut WebSocket, reason: &str) {
    let _ = socket.send(Message::Text(ServerMsg::Closed { reason: reason.into() }.to_json().into())).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// An authenticated connection.
struct Me {
    session: Arc<Session>,
    peer: Peer,
    client_id: String,
    conn_id: u64,
    ids: IdBlock,
}

/// Hello with a valid token for an open session and a participant who wasn't removed; `Err` is
/// the reason the connection is refused.
async fn authenticate(app: &App, socket: &mut WebSocket) -> Result<(Arc<Session>, Peer, String, String), &'static str> {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMsg>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMsg::Hello { client_id, token }) = hello else { return Err("expected hello") };
    let client_id: String = client_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect();
    let claims = crate::auth::verify(&app.secret, &token).ok_or("unauthorized")?;
    let session = app.session(&claims.sid).await.ok_or("ended")?;
    let (title, peer) = {
        let mut info = session.info.lock().await;
        if info.ended {
            return Err("ended");
        }
        let title = info.title.clone();
        let p = info.participants.get_mut(&claims.pid).filter(|p| !p.removed).ok_or("kicked")?;
        p.last_seen = crate::auth::now();
        (title, Peer { id: p.id.clone(), name: p.name.clone(), role: p.role, online: true, slide: None })
    };
    let _ = session.save_info().await;
    Ok((session, peer, client_id, title))
}

/// Welcome and register under the document lock, so no op falls between the snapshot and this
/// connection hearing about the next one.
async fn register(
    app: &App,
    session: &Arc<Session>,
    peer: &Peer,
    client_id: &str,
    title: &str,
    tx: &mpsc::UnboundedSender<String>,
) -> Option<(u64, IdBlock)> {
    let conn_id = app.next_conn.fetch_add(1, Ordering::Relaxed);
    let mut shared = session.shared.lock().await;
    let ids = shared.next_block()?;
    session.dirty.store(true, Ordering::Release);
    let welcome = ServerMsg::Welcome {
        title: title.to_string(),
        you: peer.clone(),
        ids,
        seq: shared.seq,
        applied_op: shared.applied_op(client_id),
        entries: shared.snapshot(),
    };
    let _ = tx.send(welcome.to_json());
    let mut conns = session.conns.lock().await;
    // The same client reconnecting: its old socket is gone or about to be.
    conns.retain(|_, c| c.client_id != client_id);
    conns.insert(conn_id, Conn { pid: peer.id.clone(), client_id: client_id.to_string(), slide: None, tx: tx.clone() });
    Some((conn_id, ids))
}

/// Apply one op (if this participant may edit), acknowledge it and pass it to everyone else.
async fn handle_op(me: &Me, op: u64, entries: Vec<Entry>) {
    let s = &me.session;
    let can_edit = s.info.lock().await.participants.get(&me.peer.id).is_some_and(|p| !p.removed && p.role.can_edit());
    let mut shared = s.shared.lock().await;
    let conns = s.conns.lock().await;
    let Some(mine) = conns.get(&me.conn_id) else { return };
    if !can_edit {
        let _ = mine.tx.send(ServerMsg::Rejected { op, reason: "viewers can't change the presentation".into() }.to_json());
        return;
    }
    match shared.apply_op(&me.client_id, op, &entries) {
        Ok(seq) => {
            s.dirty.store(true, Ordering::Release);
            s.backup_due.store(true, Ordering::Release);
            let _ = mine.tx.send(ServerMsg::Ack { seq, op }.to_json());
            let msg = ServerMsg::Op { seq, from: me.client_id.clone(), entries }.to_json();
            for (id, c) in conns.iter() {
                if *id != me.conn_id {
                    let _ = c.tx.send(msg.clone());
                }
            }
        }
        Err(e) => {
            let _ = mine.tx.send(ServerMsg::Rejected { op, reason: e.to_string() }.to_json());
        }
    }
}

/// A fresh welcome on request (the client resyncs after a refused op).
async fn resync(me: &Me) {
    let s = &me.session;
    let (role, title) = {
        let info = s.info.lock().await;
        (info.participants.get(&me.peer.id).map(|p| p.role).unwrap_or(me.peer.role), info.title.clone())
    };
    let shared = s.shared.lock().await;
    if let Some(c) = s.conns.lock().await.get(&me.conn_id) {
        let w = ServerMsg::Welcome {
            title,
            you: Peer { role, ..me.peer.clone() },
            ids: me.ids,
            seq: shared.seq,
            applied_op: shared.applied_op(&me.client_id),
            entries: shared.snapshot(),
        };
        let _ = c.tx.send(w.to_json());
    }
}

async fn presence(me: &Me, slide: u32) {
    if let Some(c) = me.session.conns.lock().await.get_mut(&me.conn_id) {
        c.slide = Some(slide);
    }
    me.session.broadcast_presence().await;
}

pub async fn run(app: Arc<App>, mut socket: WebSocket) {
    let (session, peer, client_id, title) = match authenticate(&app, &mut socket).await {
        Ok(a) => a,
        Err(reason) => return send_closed(&mut socket, reason).await,
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let Some((conn_id, ids)) = register(&app, &session, &peer, &client_id, &title, &tx).await else {
        return send_closed(&mut socket, "no ids left in this session").await;
    };
    drop(tx);
    let me = Me { session, peer, client_id, conn_id, ids };
    me.session.broadcast_presence().await;
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let text = match incoming {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                match serde_json::from_str::<ClientMsg>(text.as_str()) {
                    Ok(ClientMsg::Op { op, entries }) => handle_op(&me, op, entries).await,
                    Ok(ClientMsg::Presence { slide }) => presence(&me, slide).await,
                    Ok(ClientMsg::Hello { .. }) => resync(&me).await,
                    Err(_) => {
                        send_closed(&mut socket, "bad message").await;
                        break;
                    }
                }
            }
            outgoing = rx.recv() => {
                let Some(text) = outgoing else { break };
                let closing = text.starts_with(r#"{"t":"closed""#);
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
                if closing {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    me.session.conns.lock().await.remove(&me.conn_id);
    me.session.broadcast_presence().await;
}
