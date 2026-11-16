//! The server's authoritative state of one session: every key's current value, the sequence
//! number, the last op applied per client and the id blocks handed out. IO-free: the server
//! application persists it as JSON (serde).

use std::collections::BTreeMap;

use deckcraft_model::Presentation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::patch::{Entry, Patch, apply, diff, empty};
use crate::proto::IdBlock;
use crate::{CollabError, Result};

/// The most keys one operation may write (a deck of 1 000 slides with 100 shapes each is
/// 101 000 keys, and only a snapshot writes them all at once).
pub const MAX_ENTRIES_PER_OP: usize = 250_000;

/// The shared document of one session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    pub seq: u64,
    /// Every key's value (media included, base64).
    pub values: BTreeMap<String, Value>,
    /// The last op applied for each client id.
    pub applied: BTreeMap<String, u64>,
    /// Id blocks handed out so far.
    pub blocks: u32,
}

impl Shared {
    /// A session starting from `doc`.
    pub fn from_presentation(doc: &Presentation) -> Result<Shared> {
        let mut s = Shared::default();
        for e in diff(None, doc).to_entries()? {
            if let Some(v) = e.v {
                s.values.insert(e.k, v);
            }
        }
        Ok(s)
    }

    /// Check and apply one client's operation; returns its sequence number. Nothing is applied
    /// when any entry is invalid. A repeated op (the client resent it after a reconnect) is
    /// acknowledged again without being applied twice.
    pub fn apply_op(&mut self, client: &str, op: u64, entries: &[Entry]) -> Result<u64> {
        if entries.len() > MAX_ENTRIES_PER_OP {
            return Err(CollabError::TooLarge(format!("{} entries", entries.len())));
        }
        if self.applied.get(client).is_some_and(|last| op <= *last) {
            return Ok(self.seq);
        }
        // Decoding checks every value against the model types.
        Patch::from_entries(entries)?;
        for e in entries {
            match &e.v {
                Some(v) => {
                    self.values.insert(e.k.clone(), v.clone());
                }
                None => {
                    self.values.remove(&e.k);
                }
            }
        }
        self.seq = self.seq.saturating_add(1);
        self.applied.insert(client.to_string(), op);
        Ok(self.seq)
    }

    pub fn applied_op(&self, client: &str) -> u64 {
        self.applied.get(client).copied().unwrap_or(0)
    }

    /// The next free id block.
    pub fn next_block(&mut self) -> Option<IdBlock> {
        let b = IdBlock::nth(self.blocks)?;
        self.blocks = self.blocks.saturating_add(1);
        Some(b)
    }

    /// Every key, for a welcome.
    pub fn snapshot(&self) -> Vec<Entry> {
        self.values.iter().map(|(k, v)| Entry { k: k.clone(), v: Some(v.clone()) }).collect()
    }

    /// The presentation as it stands (for saving and export).
    pub fn materialize(&self) -> Result<Presentation> {
        let p = Patch::from_entries(&self.snapshot())?;
        let mut doc = apply(&empty(), &p);
        doc.fix_next_id();
        Ok(doc)
    }
}
