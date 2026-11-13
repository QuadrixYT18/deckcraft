//! The desktop's network for live sessions: HTTP and the WebSocket on worker threads, results
//! collected for the UI thread to poll each frame.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deckcraft_ui_egui::live::{LiveNet, LiveSocketEvent};

/// The largest answer read from the server's HTTP API (only small JSON is expected).
const MAX_HTTP_ANSWER: u64 = 1 << 20;

#[derive(Default)]
pub struct NativeNet {
    http: Arc<Mutex<Option<Result<(u16, String), String>>>>,
    events: Arc<Mutex<Vec<LiveSocketEvent>>>,
    outgoing: Option<Sender<String>>,
    /// Bumped by every connect and close: a socket thread of an older generation stops and
    /// reports nothing more.
    generation: Arc<AtomicU64>,
    ctx: Option<egui::Context>,
}

impl NativeNet {
    pub fn new(ctx: Option<egui::Context>) -> Self {
        NativeNet { ctx, ..Default::default() }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LiveNet for NativeNet {
    fn post(&mut self, url: &str, content_type: &str, body: Vec<u8>) {
        let slot = self.http.clone();
        *lock(&slot) = None;
        let (url, content_type, ctx) = (url.to_string(), content_type.to_string(), self.ctx.clone());
        std::thread::spawn(move || {
            let agent: ureq::Agent =
                ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(120))).build().into();
            let result = agent.post(&url).header("Content-Type", &content_type).send(&body[..]).map_err(|e| e.to_string()).and_then(|mut r| {
                let status = r.status().as_u16();
                r.body_mut().with_config().limit(MAX_HTTP_ANSWER).read_to_string().map(|b| (status, b)).map_err(|e| e.to_string())
            });
            *lock(&slot) = Some(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn poll_http(&mut self) -> Option<Result<(u16, String), String>> {
        lock(&self.http).take()
    }

    fn connect(&mut self, url: &str) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let (tx, rx) = channel::<String>();
        self.outgoing = Some(tx);
        let (url, events, current, ctx) = (url.to_string(), self.events.clone(), self.generation.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let report = |e: LiveSocketEvent| {
                if current.load(Ordering::Acquire) == generation {
                    lock(&events).push(e);
                    if let Some(ctx) = &ctx {
                        ctx.request_repaint();
                    }
                }
            };
            let reason = socket_loop(&url, &rx, &current, generation, &report);
            report(LiveSocketEvent::Closed(reason));
        });
    }

    fn send(&mut self, text: String) {
        if let Some(tx) = &self.outgoing {
            let _ = tx.send(text);
        }
    }

    fn poll(&mut self) -> Vec<LiveSocketEvent> {
        std::mem::take(&mut *lock(&self.events))
    }

    fn close(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.outgoing = None;
    }
}

/// Run one connection until it ends; returns why.
fn socket_loop(url: &str, rx: &Receiver<String>, current: &AtomicU64, generation: u64, report: &dyn Fn(LiveSocketEvent)) -> String {
    use tungstenite::Message;
    use tungstenite::stream::MaybeTlsStream;
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(deckcraft_collab::proto::MAX_MESSAGE_BYTES);
    config.max_frame_size = Some(deckcraft_collab::proto::MAX_MESSAGE_BYTES);
    let (mut ws, _) = match tungstenite::client::connect_with_config(url, Some(config), 3) {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    // Short reads so outgoing messages go out promptly.
    let timeout = Some(Duration::from_millis(20));
    let _ = match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(s) => s.get_mut().set_read_timeout(timeout),
        _ => Ok(()),
    };
    report(LiveSocketEvent::Open);
    loop {
        if current.load(Ordering::Acquire) != generation {
            let _ = ws.close(None);
            let _ = ws.flush();
            return "closed".into();
        }
        loop {
            match rx.try_recv() {
                Ok(text) => {
                    if let Err(e) = ws.send(Message::text(text)) {
                        return e.to_string();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    return "closed".into();
                }
            }
        }
        match ws.read() {
            Ok(Message::Text(t)) => report(LiveSocketEvent::Text(t.as_str().to_string())),
            Ok(Message::Close(_)) => return "the server closed the connection".into(),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return e.to_string(),
        }
    }
}
