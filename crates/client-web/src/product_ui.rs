//! Product presentation glue. Layout/actions stay in shared Rust; the platform
//! side supplies a canvas, local browser storage and native file selection.
use super::{browser_network::BrowserSocket, js_error};
use audio::{
    EmitterId,
    browser::{BrowserImports, ImportEvent, take_import_event},
    scene::{PreparedSounds, WebScene},
};
use client::{network::Connection, ui_bridge::UiBridge, view::ClientView};
use ui::{
    app::{Focus, Intent, Modifiers, Screen, Snapshot},
    layer::UiLayer,
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::HtmlCanvasElement;
#[derive(Debug)]
pub enum Command {
    Connect { address: String, name: String },
    Disconnect,
    RebuildResources,
    BugReport,
    Quit,
}
pub struct ProductUi {
    pub layer: UiLayer,
    pub audio: WebScene,
    imports: BrowserImports,
    bridge: UiBridge,
    command: Option<Command>,
    ready_world: bool,
    sequence: u64,
}
fn preferences_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
impl ProductUi {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self, JsValue> {
        let mut layer = UiLayer::prepare(device, queue, format, true).map_err(js_error)?;
        if let Some(text) = preferences_storage()
            .and_then(|storage| storage.get_item("cubi.ui.preferences").ok().flatten())
            && text.len() <= 16384
            && text.len() % 2 == 0
        {
            let bytes: Option<Vec<_>> = text
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let digit = |value| match value {
                        b'0'..=b'9' => Some(value - b'0'),
                        b'a'..=b'f' => Some(value - b'a' + 10),
                        _ => None,
                    };
                    Some(digit(pair[0])? * 16 + digit(pair[1])?)
                })
                .collect();
            if let Some(bytes) = bytes {
                let _ = layer.app.load_preferences(&bytes);
            }
        }
        let prepared = PreparedSounds::prepare(None, None).map_err(js_error)?;
        let audio = WebScene::open(prepared)?;
        let mut result = Self {
            layer,
            audio,
            imports: BrowserImports::prepare()?,
            bridge: UiBridge::default(),
            command: None,
            ready_world: false,
            sequence: 0,
        };
        result.apply_sound()?;
        Ok(result)
    }
    pub fn save_preferences(&self) {
        use std::fmt::Write;
        let bytes = self.layer.app.encode_preferences();
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        if let Some(storage) = preferences_storage() {
            let _ = storage.set_item("cubi.ui.preferences", &text);
        }
    }
    pub fn apply_sound(&mut self) -> Result<(), JsValue> {
        let (master, levels) = client::ui_bridge::sound_levels(&self.layer.app.settings);
        self.audio.output().volume(None, master)?;
        for (category, gain) in levels {
            self.audio.output().volume(Some(category), gain)?;
        }
        Ok(())
    }
    pub fn gesture(&mut self) {
        if let Ok(promise) = self.audio.output().resume_request() {
            spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
    }
    pub fn click_sound(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        let _ = self
            .audio
            .event("ui.click", EmitterId(u64::MAX), None, self.sequence);
    }
    pub fn keyboard(
        &mut self,
        view: &mut ClientView,
        code: &str,
        key: &str,
        pressed: bool,
        modifiers: Modifiers,
    ) -> bool {
        let text_focus = matches!(
            self.layer.app.focus,
            Focus::Name | Focus::Address | Focus::Search | Focus::Chat
        );
        let captured =
            client::ui_bridge::apply_key(&mut self.layer.app, view, code, pressed, modifiers);
        if pressed && text_focus && !modifiers.ctrl && key.chars().count() == 1 {
            self.layer.app.text(key);
        }
        captured
    }
    pub fn sync(&mut self, network: &Connection, view: &ClientView) -> Result<(), JsValue> {
        self.bridge
            .sync_authority(&mut self.layer.app, network)
            .map_err(js_error)?;
        if network.player().is_some() && view.is_ready() && !self.ready_world {
            self.ready_world = true;
            self.layer.app.set_status("");
            if self.layer.app.screen == Screen::Loading {
                self.layer.app.open(Screen::Game);
            }
        }
        Ok(())
    }
    pub fn connecting(&mut self) {
        self.ready_world = false;
        self.layer.app.set_status("Ожидание ответа мира…");
        self.layer.app.open(Screen::Loading);
    }
    pub fn disconnected(&mut self, message: &str) {
        self.ready_world = false;
        self.bridge = UiBridge::default();
        let _ = self.layer.app.set_snapshot(Snapshot::default());
        self.layer.app.open(Screen::Welcome);
        self.layer.app.set_status(message);
    }
    pub fn poll_imports(&mut self) {
        while let Some(event) = take_import_event() {
            match event {
                ImportEvent::Loading => self
                    .layer
                    .app
                    .set_status("Загрузка выбранных локальных файлов…"),
                ImportEvent::Ready(_) => {
                    self.layer.app.set_status("Подготовка локальных ресурсов…");
                    self.command = Some(Command::RebuildResources);
                }
                ImportEvent::Rejected => self
                    .layer
                    .app
                    .set_status("Набор отклонён; прежние ресурсы сохранены"),
            }
        }
    }
    pub fn take_command(&mut self) -> Option<Command> {
        self.command.take()
    }
    pub fn drain(
        &mut self,
        view: &mut ClientView,
        mut socket: Option<&mut BrowserSocket>,
        canvas: &HtmlCanvasElement,
        gesture: bool,
        now_ms: u64,
    ) -> Result<(), JsValue> {
        for _ in 0..64 {
            let Some(intent) = self.layer.app.take_intent() else {
                break;
            };
            match intent {
                Intent::Connect { address, name } => {
                    self.save_preferences();
                    self.connecting();
                    self.command = Some(Command::Connect { address, name });
                }
                Intent::Disconnect => self.command = Some(Command::Disconnect),
                Intent::Quit => self.command = Some(Command::Quit),
                Intent::BugReport => self.command = Some(Command::BugReport),
                Intent::PointerCapture(capture) => {
                    if capture && gesture {
                        canvas.request_pointer_lock();
                    }
                    if !capture {
                        view.clear_keys();
                        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
                            document.exit_pointer_lock();
                        }
                    }
                }
                Intent::SettingsChanged => {
                    self.apply_sound()?;
                    self.save_preferences();
                    let _ = view.apply_settings(&self.layer.app.settings);
                    view.invalidate();
                }
                Intent::SavePreferences => self.save_preferences(),
                Intent::ImportPack => self.imports.choose_pack(),
                Intent::ImportMinecraft => self.imports.choose_minecraft(),
                Intent::OpenCredits => download(
                    "CREDITS.txt",
                    &format!("{}\n\n{}", ui::font::FONT_CREDITS, audio::catalog::CREDITS),
                    "text/plain;charset=utf-8",
                )?,
                Intent::PersonalTime(_) => view.invalidate(),
                other => {
                    if let Some(socket) = socket.as_deref_mut() {
                        match client::ui_bridge::dispatch(
                            socket.session_mut().connection_mut(),
                            other,
                            now_ms,
                        ) {
                            Ok(client::ui_bridge::Dispatch::Sent(_)) => {}
                            Ok(client::ui_bridge::Dispatch::Unsupported(_)) => {
                                self.layer.app.set_status("Действие недоступно в этом окне")
                            }
                            Ok(client::ui_bridge::Dispatch::Local(_)) => {}
                            Err(_) => self
                                .layer
                                .app
                                .set_status("Действие ожидает соединения с миром"),
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn download(name: &str, contents: &str, mime: &str) -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("Document unavailable")?;
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(contents));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let anchor = document
        .create_element("a")?
        .dyn_into::<web_sys::HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download(name);
    anchor.click();
    web_sys::Url::revoke_object_url(&url)?;
    Ok(())
}
