//! Bounded WebSocket transport for the shared Rust remote owner. Only encoded
//! protocol frames reach the socket; local resource bytes have no route here.
use client::{network::NetworkConfig, remote::RemoteSession, view::ClientView};
use net::{PlayerId, SecretToken};
use std::{
    cell::{Ref, RefCell, RefMut},
    rc::Rc,
};
use wasm_bindgen::{JsCast, prelude::*};
use web_sys::{BinaryType, Event, MessageEvent, WebSocket};
const SOCKET_BACKLOG: u32 = 2 * 1024 * 1024;
struct Inbox {
    fault: Option<String>,
    admitted_frames: u64,
    rejected_frames: u64,
    open: bool,
    closed: bool,
}
pub struct BrowserSocket {
    socket: WebSocket,
    session: Rc<RefCell<RemoteSession>>,
    inbox: Rc<RefCell<Inbox>>,
    pending: Option<Vec<u8>>,
    identity: PlayerId,
    saved_credential: Option<[u8; 32]>,
    storage_key: String,
    started_ms: u64,
    _listeners: [Closure<dyn FnMut(Event)>; 4],
}
fn error(value: impl ToString) -> JsValue {
    JsValue::from_str(&value.to_string())
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[usize::from(byte >> 4)] as char);
        result.push(DIGITS[usize::from(byte & 15)] as char);
    }
    result
}
fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut bytes = [0; N];
    for (i, pair) in text.as_bytes().chunks_exact(2).enumerate() {
        let nibble = |c| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        bytes[i] = nibble(pair[0])? * 16 + nibble(pair[1])?;
    }
    Some(bytes)
}
pub fn socket_url(address: &str) -> Result<String, JsValue> {
    if address.is_empty() || address.len() > 256 || address.chars().any(char::is_control) {
        return Err(error("Некорректный адрес мира"));
    }
    let window = web_sys::window().ok_or_else(|| error("Window unavailable"))?;
    let scheme = if window.location().protocol()? == "https:" {
        "wss://"
    } else {
        "ws://"
    };
    let absolute = if address.starts_with("ws://") || address.starts_with("wss://") {
        address.to_owned()
    } else {
        format!("{scheme}{}", address.trim())
    };
    let url = web_sys::Url::new(&absolute)?;
    if !matches!(url.protocol().as_str(), "ws:" | "wss:")
        || !url.username().is_empty()
        || !url.password().is_empty()
        || !url.search().is_empty()
        || !url.hash().is_empty()
    {
        return Err(error("Некорректный адрес мира"));
    }
    Ok(url.href())
}
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
fn identity(key: &str) -> Result<(PlayerId, Option<SecretToken>), JsValue> {
    if let Some(text) = storage().and_then(|storage| storage.get_item(key).ok().flatten())
        && text.len() <= 100
    {
        let mut fields = text.split(':');
        if let Some(id) = fields.next().and_then(unhex::<16>) {
            let credential = fields.next().and_then(unhex::<32>).map(SecretToken::new);
            if fields.next().is_none() && credential.is_some() {
                return Ok((PlayerId(id), credential));
            }
        }
    }
    // The authority creates new identities. A nonzero ID without its credential
    // would request an unauthorized resume rather than a new player.
    Ok((PlayerId::default(), None))
}
impl BrowserSocket {
    pub fn connect(address: &str, name: &str, now_ms: u64) -> Result<Self, JsValue> {
        let url = socket_url(address)?;
        let storage_key = format!("cubi.identity.{url}");
        let (identity, credential) = identity(&storage_key)?;
        let mut session = RemoteSession::new(NetworkConfig::remote_scene()).map_err(error)?;
        session
            .connection_mut()
            .hello(name.to_owned(), identity, credential)
            .map_err(|_| error("Имя или данные входа отклонены"))?;
        let session = Rc::new(RefCell::new(session));
        let socket = WebSocket::new(&url)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let inbox = Rc::new(RefCell::new(Inbox {
            fault: None,
            admitted_frames: 0,
            rejected_frames: 0,
            open: false,
            closed: false,
        }));
        let messages = inbox.clone();
        let receiving = session.clone();
        let message = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(event) = event.dyn_ref::<MessageEvent>() else {
                return;
            };
            let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                messages.borrow_mut().fault = Some("Мир прислал неподдерживаемый кадр".into());
                return;
            };
            let length = buffer.byte_length() as usize;
            if length > net::MAX_FRAME_BYTES {
                messages.borrow_mut().fault = Some("Кадр мира превышает допустимый размер".into());
                return;
            }
            let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
            // Browser WebSocket has no TCP pause API. Admit each bounded frame
            // directly into the shared Rust caches, which coalesce RAW/LIGHT
            // versions by page. Scene imports, prediction and GPU work remain
            // in the bounded owner pump; no arbitrary raw event backlog grows.
            let received = receiving
                .borrow_mut()
                .receive_frame(&bytes, super::clock_ms().max(0.) as u64);
            if let Err(error) = received {
                let mut inbox = messages.borrow_mut();
                inbox.rejected_frames = inbox.rejected_frames.saturating_add(1);
                inbox.fault = Some(error);
            } else {
                let mut inbox = messages.borrow_mut();
                inbox.admitted_frames = inbox.admitted_frames.saturating_add(1);
            }
        });
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        let opened = inbox.clone();
        let open = Closure::<dyn FnMut(Event)>::new(move |_| opened.borrow_mut().open = true);
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        let closed = inbox.clone();
        let close = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut inbox = closed.borrow_mut();
            inbox.closed = true;
            inbox.open = false;
        });
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        let failed = inbox.clone();
        let failure = Closure::<dyn FnMut(Event)>::new(move |_| {
            failed.borrow_mut().fault = Some("Не удалось подключиться к миру".into())
        });
        socket.set_onerror(Some(failure.as_ref().unchecked_ref()));
        Ok(Self {
            socket,
            session,
            inbox,
            pending: None,
            identity,
            saved_credential: credential.map(|value| *value.as_bytes()),
            storage_key,
            started_ms: now_ms,
            _listeners: [message, open, close, failure],
        })
    }
    pub fn session(&self) -> Ref<'_, RemoteSession> {
        self.session.borrow()
    }
    pub fn receive_counts(&self) -> [u64; 2] {
        let inbox = self.inbox.borrow();
        [inbox.admitted_frames, inbox.rejected_frames]
    }
    pub fn session_mut(&mut self) -> RefMut<'_, RemoteSession> {
        self.session.borrow_mut()
    }
    pub fn pump(&mut self, view: &mut ClientView, now_ms: u64) -> Result<(), String> {
        if let Some(fault) = self.inbox.borrow_mut().fault.take() {
            return Err(fault);
        }
        if self.inbox.borrow().closed {
            return Err("Соединение с миром закрыто".into());
        }
        let mut session = self.session.borrow_mut();
        if session.connection().welcome().is_none()
            && now_ms.saturating_sub(self.started_ms) > 15_000
        {
            return Err("Мир не ответил вовремя".into());
        }
        session.pump(view, now_ms)?;
        if let Some(welcome) = session.connection().welcome() {
            self.identity = welcome.identity;
        }
        if let Some(credential) = session.connection().credential() {
            let bytes = *credential.as_bytes();
            if self.saved_credential != Some(bytes) {
                if let Some(storage) = storage() {
                    let _ = storage.set_item(
                        &self.storage_key,
                        &format!("{}:{}", hex(&self.identity.0), hex(&bytes)),
                    );
                }
                self.saved_credential = Some(bytes);
            }
        }
        if !self.inbox.borrow().open {
            return Ok(());
        }
        for _ in 0..16 {
            if self.socket.buffered_amount() > SOCKET_BACKLOG {
                break;
            }
            if self.pending.is_none() {
                self.pending = session.take_outgoing();
            }
            let Some(bytes) = self.pending.as_ref() else {
                break;
            };
            self.socket
                .send_with_u8_array(bytes)
                .map_err(|_| "Не удалось отправить кадр миру".to_owned())?;
            self.pending = None;
        }
        Ok(())
    }
}
impl Drop for BrowserSocket {
    fn drop(&mut self) {
        self.socket.set_onmessage(None);
        self.socket.set_onopen(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        let _ = self.socket.close();
    }
}
