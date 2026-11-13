//! Sessions, participants and their files.
//!
//! Each session lives in `<data>/sessions/<id>/`:
//! - `session.json`: name, access code, participants (written on every change of them);
//! - `shared.json`: the shared document ([`Shared`]), written at most every two seconds while it
//!   changes and when the server stops, always to a temporary file first and then renamed, so a
//!   crash leaves the previous complete file;
//! - `backups/shared-<time>.json`: a copy every ten minutes while it changes, the last 20 kept.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use deckcraft_collab::proto::{Peer, Role, ServerMsg};
use deckcraft_collab::server::Shared;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

pub const BACKUPS_KEPT: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// Removed by the host: their tokens no longer work.
    #[serde(default)]
    pub removed: bool,
    /// Unix seconds of the last connection.
    #[serde(default)]
    pub last_seen: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub id: String,
    pub title: String,
    pub code: String,
    pub created: i64,
    #[serde(default)]
    pub ended: bool,
    /// New participants join as editors (`false`: as viewers).
    #[serde(default = "yes")]
    pub join_as_editor: bool,
    pub participants: BTreeMap<String, Participant>,
}

fn yes() -> bool {
    true
}

/// One open connection.
pub struct Conn {
    pub pid: String,
    pub client_id: String,
    pub slide: Option<u32>,
    pub tx: mpsc::UnboundedSender<String>,
}

pub struct Session {
    pub dir: PathBuf,
    pub info: Mutex<Info>,
    pub shared: Mutex<Shared>,
    pub conns: Mutex<HashMap<u64, Conn>>,
    /// The document changed since it was last written.
    pub dirty: AtomicBool,
    /// The document changed since the last backup.
    pub backup_due: AtomicBool,
    pub saved_seq: AtomicU64,
}

impl Session {
    pub fn new(dir: PathBuf, info: Info, shared: Shared) -> Session {
        let seq = shared.seq;
        Session {
            dir,
            info: Mutex::new(info),
            shared: Mutex::new(shared),
            conns: Mutex::new(HashMap::new()),
            dirty: AtomicBool::new(false),
            backup_due: AtomicBool::new(false),
            saved_seq: AtomicU64::new(seq),
        }
    }

    /// Everyone in the session, with who is connected.
    pub async fn peers(&self) -> Vec<Peer> {
        let info = self.info.lock().await;
        let conns = self.conns.lock().await;
        info.participants
            .values()
            .filter(|p| !p.removed)
            .map(|p| {
                let conn = conns.values().find(|c| c.pid == p.id);
                Peer { id: p.id.clone(), name: p.name.clone(), role: p.role, online: conn.is_some(), slide: conn.and_then(|c| c.slide) }
            })
            .collect()
    }

    pub async fn broadcast_presence(&self) {
        let msg = ServerMsg::Presence { peers: self.peers().await }.to_json();
        for c in self.conns.lock().await.values() {
            let _ = c.tx.send(msg.clone());
        }
    }

    /// Close every connection of participant `pid` (or of everyone) with `reason`.
    pub async fn close(&self, pid: Option<&str>, reason: &str) {
        let msg = ServerMsg::Closed { reason: reason.into() }.to_json();
        let mut conns = self.conns.lock().await;
        conns.retain(|_, c| {
            if pid.is_none_or(|p| p == c.pid) {
                let _ = c.tx.send(msg.clone());
                false
            } else {
                true
            }
        });
    }

    pub async fn save_info(&self) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(&*self.info.lock().await).map_err(std::io::Error::other)?;
        write_atomic(&self.dir.join("session.json"), &json)
    }

    /// Write the document if it changed. Serialised under the lock, written outside it.
    pub async fn save_shared(&self) -> std::io::Result<()> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let (json, seq) = {
            let s = self.shared.lock().await;
            (serde_json::to_vec(&*s).map_err(std::io::Error::other)?, s.seq)
        };
        let dir = self.dir.clone();
        let r = tokio::task::spawn_blocking(move || write_atomic(&dir.join("shared.json"), &json)).await.map_err(std::io::Error::other)?;
        if r.is_err() {
            // Try again next round.
            self.dirty.store(true, Ordering::Release);
        } else {
            self.saved_seq.store(seq, Ordering::Release);
        }
        r
    }

    /// Copy the last written document into `backups/` and keep the newest few.
    pub async fn backup(&self) -> std::io::Result<()> {
        if !self.backup_due.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || {
            let backups = dir.join("backups");
            std::fs::create_dir_all(&backups)?;
            let name = format!("shared-{}.json", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
            std::fs::copy(dir.join("shared.json"), backups.join(name))?;
            let mut all: Vec<_> = std::fs::read_dir(&backups)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            all.sort();
            let excess = all.len().saturating_sub(BACKUPS_KEPT);
            for old in all.into_iter().take(excess) {
                let _ = std::fs::remove_file(old);
            }
            Ok(())
        })
        .await
        .map_err(std::io::Error::other)?
    }
}

/// Write `bytes` to `path` so that a crash leaves either the old or the new file: a temporary
/// file in the same folder, flushed to disk, then renamed over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Read every session in `<data>/sessions`. A damaged session is skipped with a message (its
/// files are left as they are for inspection) instead of stopping the server.
pub fn load_all(data: &Path) -> Vec<Arc<Session>> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(data.join("sessions")) else { return out };
    for e in entries.flatten() {
        let dir = e.path();
        let info: Option<Info> = std::fs::read(dir.join("session.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let shared: Option<Shared> = std::fs::read(dir.join("shared.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        match (info, shared) {
            (Some(info), Some(shared)) => out.push(Arc::new(Session::new(dir, info, shared))),
            _ => eprintln!("deckcraft-server: skipping damaged session folder {}", dir.display()),
        }
    }
    out
}
