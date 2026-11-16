//! The client side of a live session, without any IO: the application feeds it the messages it
//! receives and the document after each frame; it answers with patches to apply and messages to
//! send.
//!
//! The application calls, in this order each frame:
//! 1. [`Client::receive`] for every message that arrived, applying each [`Event::Apply`] patch
//!    to the shared document (and its undo history) and then calling [`Client::adopt`];
//! 2. [`Client::local`] with the shared document (at most every 100 ms is plenty);
//! 3. [`Client::take_outbox`], sending each message if the socket is open.

use std::collections::BTreeSet;
use std::sync::Arc;

use deckcraft_model::Presentation;

use crate::patch::{Patch, apply, diff, empty};
use crate::proto::{ClientMsg, IdBlock, Peer, Role, ServerMsg};
use crate::{CollabError, Result};

/// What the application must do after [`Client::receive`].
#[derive(Debug)]
pub enum Event {
    /// The first welcome: open this presentation as the shared document. Its undo history starts
    /// here. Set its id counter into [`Client::ids`].
    Joined(Box<Presentation>),
    /// Apply this patch to the shared document, its undo/redo steps and any gesture in progress,
    /// then call [`Client::adopt`].
    Apply(Patch),
    /// The participant list changed.
    Presence,
    /// This user's role changed.
    Role(Role),
    /// The server refused one of this user's changes; it has been undone.
    Rejected(String),
    /// The server ended the connection (`kicked`, `ended`, `replaced`). Don't reconnect.
    Closed(String),
}

#[derive(Debug)]
struct Pending {
    op: u64,
    keys: BTreeSet<String>,
    patch: Patch,
    sent: bool,
}

/// The client state of one live session.
#[derive(Debug)]
pub struct Client {
    pub client_id: String,
    token: String,
    pub title: String,
    pub you: Option<Peer>,
    pub ids: Option<IdBlock>,
    pub peers: Vec<Peer>,
    /// The last sequence number seen.
    pub seq: u64,
    connected: bool,
    next_op: u64,
    pending: Vec<Pending>,
    /// The document as last diffed: the shared state plus this client's pending changes.
    synced: Option<Arc<Presentation>>,
    outbox: Vec<String>,
    /// The last change that couldn't be encoded, for the application to show.
    pub last_error: Option<String>,
}

impl Client {
    pub fn new(client_id: String, token: String) -> Self {
        Client {
            client_id,
            token,
            title: String::new(),
            you: None,
            ids: None,
            peers: vec![],
            seq: 0,
            connected: false,
            next_op: 1,
            pending: vec![],
            synced: None,
            outbox: vec![],
            last_error: None,
        }
    }

    /// The socket opened: say hello. Nothing else is sent until the welcome.
    pub fn connected(&mut self) {
        self.connected = false;
        self.outbox.clear();
        self.outbox.push(ClientMsg::Hello { client_id: self.client_id.clone(), token: self.token.clone() }.to_json());
    }

    /// The socket closed. Changes keep collecting and are sent after the next welcome.
    pub fn disconnected(&mut self) {
        self.connected = false;
        self.outbox.clear();
    }

    /// Welcomed and connected.
    pub fn is_live(&self) -> bool {
        self.connected
    }

    pub fn has_joined(&self) -> bool {
        self.synced.is_some()
    }

    /// This user's changes that the server hasn't confirmed yet.
    pub fn unconfirmed(&self) -> usize {
        self.pending.len()
    }

    pub fn can_edit(&self) -> bool {
        self.you.as_ref().is_some_and(|p| p.role.can_edit())
    }

    pub fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    /// The shared document after applying the last [`Event::Apply`] (or [`Event::Joined`]): the
    /// next [`Self::local`] compares against it.
    pub fn adopt(&mut self, doc: Arc<Presentation>) {
        self.synced = Some(doc);
    }

    /// Look for this user's changes in `doc` and queue them. For a viewer, returns the patch that
    /// undoes them (a viewer can't change the shared presentation).
    pub fn local(&mut self, doc: &Arc<Presentation>) -> Option<Patch> {
        let synced = self.synced.as_ref()?;
        if Arc::ptr_eq(synced, doc) {
            return None;
        }
        if !self.can_edit() {
            let revert = diff(Some(doc), synced);
            return (!revert.is_empty()).then_some(revert);
        }
        let changes = diff(Some(synced), doc);
        self.synced = Some(doc.clone());
        if changes.is_empty() {
            return None;
        }
        self.queue(changes);
        None
    }

    fn queue(&mut self, changes: Patch) {
        if !self.connected
            && let Some(last) = self.pending.last_mut()
            && !last.sent
        {
            last.keys.extend(changes.keys());
            last.patch.merge(changes);
            return;
        }
        let op = self.next_op;
        self.next_op = self.next_op.saturating_add(1);
        let mut p = Pending { op, keys: changes.keys().into_iter().collect(), patch: changes, sent: false };
        if self.connected {
            self.send(&mut p);
        }
        self.pending.push(p);
    }

    fn send(&mut self, p: &mut Pending) {
        match p.patch.to_entries() {
            Ok(entries) => {
                self.outbox.push(ClientMsg::Op { op: p.op, entries }.to_json());
                p.sent = true;
            }
            // The op stays pending and unsent; the next rejoin tries again.
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// Tell the others which slide this user is on.
    pub fn presence(&mut self, slide: usize) {
        if self.connected {
            self.outbox.push(ClientMsg::Presence { slide: u32::try_from(slide).unwrap_or(u32::MAX) }.to_json());
        }
    }

    /// Handle one message from the server. `doc` is the shared document as it is now.
    pub fn receive(&mut self, text: &str, doc: Option<&Arc<Presentation>>) -> Result<Vec<Event>> {
        let msg: ServerMsg = serde_json::from_str(text).map_err(|e| CollabError::BadMessage(e.to_string()))?;
        let mut events = vec![];
        match msg {
            ServerMsg::Welcome { title, you, ids, seq, applied_op, entries } => {
                let base = apply(&empty(), &Patch::from_entries(&entries)?);
                self.title = title;
                self.you = Some(you);
                self.ids = Some(ids);
                self.seq = seq;
                self.pending.retain(|p| p.op > applied_op);
                match (self.synced.clone(), doc) {
                    (Some(synced), Some(doc)) => {
                        // Rejoin: the server's document plus what this user did meanwhile.
                        if self.can_edit() {
                            let unsent = diff(Some(&synced), doc);
                            if !unsent.is_empty() {
                                self.queue(unsent);
                            }
                        } else {
                            self.pending.clear();
                        }
                        let mut target = base;
                        for p in &self.pending {
                            target = apply(&target, &p.patch);
                        }
                        self.connected = true;
                        let mut pending = std::mem::take(&mut self.pending);
                        for p in &mut pending {
                            self.send(p);
                        }
                        self.pending = pending;
                        let change = diff(Some(doc), &target);
                        self.synced = Some(Arc::new(target));
                        events.push(Event::Apply(change));
                    }
                    _ => {
                        self.pending.clear();
                        self.connected = true;
                        self.synced = Some(Arc::new(base.clone()));
                        events.push(Event::Joined(Box::new(base)));
                    }
                }
            }
            ServerMsg::Op { seq, from: _, entries } => {
                if let Some(doc) = doc {
                    // This user's own changes go first: the server orders them after this op.
                    if let Some(revert) = self.local(doc) {
                        events.push(Event::Apply(revert));
                    }
                }
                self.seq = self.seq.max(seq);
                let skip: BTreeSet<String> = self.pending.iter().flat_map(|p| p.keys.iter().cloned()).collect();
                let patch = Patch::from_entries(&entries)?.without_keys(&skip);
                if let Some(synced) = &self.synced {
                    self.synced = Some(Arc::new(apply(synced, &patch)));
                }
                if !patch.is_empty() {
                    events.push(Event::Apply(patch));
                }
            }
            ServerMsg::Ack { seq, op } => {
                self.seq = self.seq.max(seq);
                self.pending.retain(|p| p.op != op);
            }
            ServerMsg::Rejected { op, reason } => {
                // Drop it and ask for the document again: the rejoin puts back the server's state.
                self.pending.retain(|p| p.op != op);
                self.outbox.push(ClientMsg::Hello { client_id: self.client_id.clone(), token: self.token.clone() }.to_json());
                self.connected = false;
                events.push(Event::Rejected(reason));
            }
            ServerMsg::Presence { peers } => {
                self.peers = peers;
                events.push(Event::Presence);
            }
            ServerMsg::Role { role } => {
                if let Some(you) = self.you.as_mut() {
                    you.role = role;
                }
                events.push(Event::Role(role));
            }
            ServerMsg::Closed { reason } => {
                self.connected = false;
                events.push(Event::Closed(reason));
            }
        }
        Ok(events)
    }
}
