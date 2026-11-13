//! The WebSocket messages: JSON text frames, tagged by `t`.
//!
//! A client opens the socket, sends [`ClientMsg::Hello`] with its participant token, and gets [`ServerMsg::Welcome`] with the whole document.
//! From then on it sends [`ClientMsg::Op`] for its own changes and receives every other client's
//! [`ServerMsg::Op`], an [`ServerMsg::Ack`] for each of its own, and [`ServerMsg::Presence`].
//! After a dropped connection it sends `Hello` again with the same `clientId`; the welcome's
//! `appliedOp` says which of its operations the server already has.

use serde::{Deserialize, Serialize};

use crate::patch::Entry;

/// The largest message a server accepts (a picture is carried base64 encoded in one op).
pub const MAX_MESSAGE_BYTES: usize = 96 << 20;

/// Who may change the presentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Created the session: may also remove people and end it.
    Host,
    #[default]
    Editor,
    /// Sees changes live but can't make any.
    Viewer,
}

impl Role {
    pub fn can_edit(self) -> bool {
        !matches!(self, Role::Viewer)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "host" => Some(Role::Host),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

/// A person in the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// The participant id (stable across reconnects of the same join).
    pub id: String,
    pub name: String,
    pub role: Role,
    /// Connected right now.
    pub online: bool,
    /// The slide index they are on, when they said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
}

/// The ids a connection allocates new slides and shapes from: `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdBlock {
    pub start: u32,
    pub end: u32,
}

/// Ids below this are left to documents as they were opened; blocks start here.
pub const ID_BLOCKS_FROM: u32 = 1 << 24;
/// Ids per block.
pub const ID_BLOCK_SIZE: u32 = 1 << 20;

impl IdBlock {
    /// The `n`th block, or `None` once the id space is used up.
    pub fn nth(n: u32) -> Option<IdBlock> {
        let start = ID_BLOCKS_FROM.checked_add(n.checked_mul(ID_BLOCK_SIZE)?)?;
        let end = start.checked_add(ID_BLOCK_SIZE)?;
        Some(IdBlock { start, end })
    }
    pub fn contains(&self, id: u32) -> bool {
        (self.start..self.end).contains(&id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientMsg {
    #[serde(rename_all = "camelCase")]
    Hello {
        /// Random per page load; identifies this client's operations across reconnects.
        client_id: String,
        /// The participant's token from joining (the socket itself carries no credentials, so
        /// browsers and native clients authenticate the same way).
        token: String,
    },
    /// The client's changes since its previous op. `op` counts up from 1 per client.
    Op { op: u64, entries: Vec<Entry> },
    /// Which slide the user is looking at.
    Presence { slide: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ServerMsg {
    #[serde(rename_all = "camelCase")]
    Welcome {
        /// The session's name.
        title: String,
        you: Peer,
        ids: IdBlock,
        /// The sequence number of the last operation in `entries`.
        seq: u64,
        /// The last of this client's ops the server has applied (0: none).
        applied_op: u64,
        /// The whole document.
        entries: Vec<Entry>,
    },
    /// Another client's operation.
    Op {
        seq: u64,
        from: String,
        entries: Vec<Entry>,
    },
    /// The server applied this client's `op` as `seq`.
    Ack {
        seq: u64,
        op: u64,
    },
    /// The server refused this client's `op` (it changed nothing).
    Rejected {
        op: u64,
        reason: String,
    },
    Presence {
        peers: Vec<Peer>,
    },
    /// The participant's role changed.
    Role {
        role: Role,
    },
    /// The connection ends: `kicked`, `ended`, `replaced` (the same join connected elsewhere).
    Closed {
        reason: String,
    },
}

impl ClientMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl ServerMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
