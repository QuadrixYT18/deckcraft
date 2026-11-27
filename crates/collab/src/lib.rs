//! Live collaboration for DeckCraft.
//!
//! Several people edit one presentation at once. Each client keeps its own full
//! [`Presentation`](deckcraft_model::Presentation) and the [`Engine`](../deckcraft_engine) edits it
//! exactly as in single-user mode (commands, pointer gestures, keys, undo). The sync layer only
//! looks at what changed:
//!
//! - [`patch`]: the document as a set of keyed values (`meta`, `order`, `slide/<id>`,
//!   `shape/<slide>/<shape>`, `media/<id>`), the [`diff`](patch::diff) between two presentations
//!   and how a [`Patch`](patch::Patch) is [applied](patch::apply). Unchanged slides are found by
//!   `Arc` identity, so a diff after one edit looks at one slide.
//! - [`proto`]: the JSON messages on the WebSocket.
//! - [`client`]: the IO-free client state machine: pending operations, acknowledgements,
//!   reconnection and the rule that keeps concurrent edits convergent.
//! - [`server`]: the server's authoritative key/value state, sequencing and materialisation.
//!
//! # Conflict strategy
//!
//! The server orders every operation (a total order, `seq`). Each key is a last-writer-wins
//! register in that order. A client applies a remote operation at once, except for keys it has
//! itself changed and not yet seen acknowledged: the server will order its own write after the
//! remote one, so the local value is already the final one. Two people editing different shapes,
//! even on the same slide, never lose each other's work; two people changing the same shape at
//! the same moment end with the later write. Ordering lists (`order`, a slide's shape order) that
//! lose a race never lose an item: a slide or shape whose key exists but is missing from the
//! winning list is appended (lowest id first), the same way on every client and the server.
//!
//! Shape and slide ids come from `Presentation::alloc_id`. To keep concurrent inserts from
//! picking the same id, the server gives every connection its own id block ([`proto::IdBlock`]).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod client;
pub mod patch;
pub mod proto;
pub mod server;

#[cfg(test)]
mod tests;

/// Errors decoding or applying collaboration data. Every value from the network is hostile.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CollabError {
    #[error("bad message: {0}")]
    BadMessage(String),
    #[error("bad key: {0}")]
    BadKey(String),
    #[error("bad value for {0}: {1}")]
    BadValue(String, String),
    #[error("too large: {0}")]
    TooLarge(String),
}

pub type Result<T> = std::result::Result<T, CollabError>;
