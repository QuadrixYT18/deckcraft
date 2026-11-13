//! One WebSocket connection: authenticate with the first message, welcome, then relay ops.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use deckcraft_collab::proto::{ClientMsg, Peer, ServerMsg};
use tokio::sync::mpsc;

use crate::App;
use crate::store::Conn;

/// How long a new socket may take to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const PING_EVERY: Duration = Duration::from_secs(25);

async fn send_closed(socket: &mut WebSocket, reason: &str) {
    let _ = socket.send(Message::Text(ServerMsg::Closed { reason: reason.into() }.to_json().into())).await;
    let _ = socket.send(Message::Close(None)).await;
}

pub async fn run(app: Arc<App>, mut socket: WebSocket) {
    // 1. Hello with a valid token for a live session and a participant who wasn't removed.
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMsg>(t.as_str()).ok(),
        _ => None,
    };
    let Some(ClientMsg::Hello { client_id, token }) = hello else {
        return send_closed(&mut socket, "expected hello").await;
    };
    let client_id: String = client_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect();
    let Some(claims) = crate::auth::verify(&app.secret, &token) else {
        return send_closed(&mut socket, "unauthorized").await;
    };
    let Some(session) = app.session(&claims.sid).await else {
        return send_closed(&mut socket, "ended").await;
    };
    let (title, me) = {
        let mut info = session.info.lock().await;
        if info.ended {
            drop(info);
            return send_closed(&mut socket, "ended").await;
        }
        let title = info.title.clone();
        match info.participants.get_mut(&claims.pid) {
            Some(p) if !p.removed => {
                p.last_seen = crate::auth::now();
                (title, Peer { id: p.id.clone(), name: p.name.clone(), role: p.role, online: true, slide: None })
            }
            _ => {
                drop(info);
                return send_closed(&mut socket, "kicked").await;
            }
        }
    };
    let _ = session.save_info().await;

    // 2. Welcome and register under the document lock, so no op falls between the snapshot and
    //    this connection hearing about the next one.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let conn_id = app.next_conn.fetch_add(1, Ordering::Relaxed);
    // Lock order everywhere: info, then shared, then conns.
    let ids = {
        let mut shared = session.shared.lock().await;
        let Some(ids) = shared.next_block() else {
            drop(shared);
            return send_closed(&mut socket, "no ids left in this session").await;
        };
        session.dirty.store(true, Ordering::Release);
        let welcome = ServerMsg::Welcome {
            title,
            you: me.clone(),
            ids,
            seq: shared.seq,
            applied_op: shared.applied_op(&client_id),
            entries: shared.snapshot(),
        };
        let _ = tx.send(welcome.to_json());
        let mut conns = session.conns.lock().await;
        // The same client reconnecting: its old socket is gone or about to be.
        conns.retain(|_, c| c.client_id != client_id);
        conns.insert(conn_id, Conn { pid: me.id.clone(), client_id: client_id.clone(), slide: None, tx: tx.clone() });
        ids
    };
    drop(tx);
    session.broadcast_presence().await;

    // 3. Relay.
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
                    Ok(ClientMsg::Op { op, entries }) => {
                        let can_edit = session.info.lock().await.participants.get(&me.id).is_some_and(|p| !p.removed && p.role.can_edit());
                        let mut shared = session.shared.lock().await;
                        let conns = session.conns.lock().await;
                        let Some(mine) = conns.get(&conn_id) else { break };
                        if !can_edit {
                            let _ = mine.tx.send(ServerMsg::Rejected { op, reason: "viewers can't change the presentation".into() }.to_json());
                            continue;
                        }
                        match shared.apply_op(&client_id, op, &entries) {
                            Ok(seq) => {
                                session.dirty.store(true, Ordering::Release);
                                session.backup_due.store(true, Ordering::Release);
                                let _ = mine.tx.send(ServerMsg::Ack { seq, op }.to_json());
                                let msg = ServerMsg::Op { seq, from: client_id.clone(), entries }.to_json();
                                for (id, c) in conns.iter() {
                                    if *id != conn_id {
                                        let _ = c.tx.send(msg.clone());
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = mine.tx.send(ServerMsg::Rejected { op, reason: e.to_string() }.to_json());
                            }
                        }
                    }
                    Ok(ClientMsg::Presence { slide }) => {
                        if let Some(c) = session.conns.lock().await.get_mut(&conn_id) {
                            c.slide = Some(slide);
                        }
                        session.broadcast_presence().await;
                    }
                    Ok(ClientMsg::Hello { .. }) => {
                        // A resync request (after a rejected op): a fresh welcome.
                        let (role, title) = {
                            let info = session.info.lock().await;
                            (info.participants.get(&me.id).map(|p| p.role).unwrap_or(me.role), info.title.clone())
                        };
                        let shared = session.shared.lock().await;
                        if let Some(c) = session.conns.lock().await.get(&conn_id) {
                            let w = ServerMsg::Welcome {
                                title,
                                you: Peer { role, ..me.clone() },
                                ids,
                                seq: shared.seq,
                                applied_op: shared.applied_op(&client_id),
                                entries: shared.snapshot(),
                            };
                            let _ = c.tx.send(w.to_json());
                        }
                    }
                    Err(_) => {
                        let _ = socket.send(Message::Text(ServerMsg::Closed { reason: "bad message".into() }.to_json().into())).await;
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
    session.conns.lock().await.remove(&conn_id);
    session.broadcast_presence().await;
}
