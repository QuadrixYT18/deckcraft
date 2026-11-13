//! The browser's network for live sessions: `fetch` for the join/create API and a `WebSocket`
//! for the session, both feeding queues the app polls each frame.

use std::cell::RefCell;
use std::rc::Rc;

use deckcraft_ui_egui::live::{LiveNet, LiveSocketEvent};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

type Queue<T> = Rc<RefCell<Vec<T>>>;

pub struct WebNet {
    ctx: egui::Context,
    http: Rc<RefCell<Option<Result<(u16, String), String>>>>,
    events: Queue<LiveSocketEvent>,
    socket: Option<web_sys::WebSocket>,
    /// Bumped by every connect and close: events of an older socket are dropped.
    generation: Rc<RefCell<u64>>,
    /// The socket's handlers, kept alive as long as it is.
    _handlers: Vec<Closure<dyn FnMut(web_sys::Event)>>,
}

impl WebNet {
    pub fn new(ctx: egui::Context) -> Self {
        WebNet { ctx, http: Rc::default(), events: Rc::default(), socket: None, generation: Rc::default(), _handlers: vec![] }
    }
}

fn js(e: wasm_bindgen::JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

async fn fetch(url: String, content_type: String, body: Vec<u8>) -> Result<(u16, String), String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    let bytes = js_sys::Uint8Array::from(body.as_slice());
    init.set_body(&bytes);
    let headers = web_sys::Headers::new().map_err(js)?;
    headers.set("Content-Type", &content_type).map_err(js)?;
    init.set_headers(&headers);
    let request = web_sys::Request::new_with_str_and_init(&url, &init).map_err(js)?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request)).await.map_err(|e| format!("network error ({})", js(e)))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a response")?;
    let status = resp.status();
    let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(js)?).await.map_err(js)?;
    Ok((status, text.as_string().unwrap_or_default()))
}

impl LiveNet for WebNet {
    fn post(&mut self, url: &str, content_type: &str, body: Vec<u8>) {
        let slot = self.http.clone();
        *slot.borrow_mut() = None;
        let ctx = self.ctx.clone();
        let (url, content_type) = (url.to_string(), content_type.to_string());
        wasm_bindgen_futures::spawn_local(async move {
            let r = fetch(url, content_type, body).await;
            *slot.borrow_mut() = Some(r);
            ctx.request_repaint();
        });
    }

    fn poll_http(&mut self) -> Option<Result<(u16, String), String>> {
        self.http.borrow_mut().take()
    }

    fn connect(&mut self, url: &str) {
        self.close();
        let generation = *self.generation.borrow();
        let ws = match web_sys::WebSocket::new(url) {
            Ok(ws) => ws,
            Err(e) => {
                self.events.borrow_mut().push(LiveSocketEvent::Closed(js(e)));
                return;
            }
        };
        let push = {
            let (events, current, ctx) = (self.events.clone(), self.generation.clone(), self.ctx.clone());
            move |e: LiveSocketEvent| {
                if *current.borrow() == generation {
                    events.borrow_mut().push(e);
                    ctx.request_repaint();
                }
            }
        };
        let on_open = {
            let push = push.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| push(LiveSocketEvent::Open))
        };
        let on_message = {
            let push = push.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
                if let Some(text) = e.dyn_ref::<web_sys::MessageEvent>().and_then(|m| m.data().as_string()) {
                    push(LiveSocketEvent::Text(text));
                }
            })
        };
        let on_close = {
            let push = push.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
                let why = e.dyn_ref::<web_sys::CloseEvent>().map(|c| format!("closed ({})", c.code())).unwrap_or_else(|| "closed".into());
                push(LiveSocketEvent::Closed(why));
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        self._handlers = vec![on_open, on_message, on_close];
        self.socket = Some(ws);
    }

    fn send(&mut self, text: String) {
        if let Some(ws) = &self.socket
            && ws.ready_state() == web_sys::WebSocket::OPEN
            && let Err(e) = ws.send_with_str(&text)
        {
            log::warn!("live session: send failed: {}", js(e));
        }
    }

    fn poll(&mut self) -> Vec<LiveSocketEvent> {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        // The server ended this person's session: a reload must not try to join it again.
        if events.iter().any(|e| matches!(e, LiveSocketEvent::Text(t) if t.starts_with(r#"{"t":"closed""#))) {
            forget_token();
        }
        events
    }

    fn close(&mut self) {
        *self.generation.borrow_mut() += 1;
        if let Some(ws) = self.socket.take() {
            ws.set_onopen(None);
            ws.set_onmessage(None);
            ws.set_onclose(None);
            let _ = ws.close();
        }
        self._handlers.clear();
    }

    fn page_origin(&self) -> Option<String> {
        web_sys::window()?.location().origin().ok().filter(|o| o.starts_with("http"))
    }
}

/// The tab's storage key for the live-session token (cleared when the tab closes).
const TOKEN_KEY: &str = "deckcraft.live.token";

/// `#token=…` from the server's join page: join that session, and take the token out of the
/// address bar so it isn't bookmarked or shared by accident. The tab keeps it (sessionStorage),
/// so a reload, including one the browser does by itself after reclaiming memory, joins again.
pub fn token_from_fragment() -> Option<String> {
    let window = web_sys::window()?;
    let storage = window.session_storage().ok().flatten();
    let hash = window.location().hash().ok().unwrap_or_default();
    let fresh = hash.strip_prefix("#token=").and_then(|t| t.split('&').next()).map(str::to_string).filter(|t| !t.is_empty() && t.len() < 4096);
    if let Some(token) = &fresh {
        if let Some(s) = &storage {
            let _ = s.set_item(TOKEN_KEY, token);
        }
        if let (Ok(history), Ok(path), Ok(search)) = (window.history(), window.location().pathname(), window.location().search()) {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&format!("{path}{search}")));
        }
        return fresh;
    }
    storage.and_then(|s| s.get_item(TOKEN_KEY).ok().flatten()).filter(|t| !t.is_empty() && t.len() < 4096)
}

/// The editor served by a DeckCraft server (`/app/`) without a session: send the visitor to the
/// join page instead of showing the sample presentation. Returns `true` when it redirected.
pub fn redirect_to_join_page() -> bool {
    let Some(window) = web_sys::window() else { return false };
    let path = window.location().pathname().unwrap_or_default();
    if !path.starts_with("/app") {
        return false;
    }
    window.location().set_href("/").is_ok()
}

/// Forget the tab's session (the host ended it or removed this person).
pub fn forget_token() {
    if let Some(s) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = s.remove_item(TOKEN_KEY);
    }
}
