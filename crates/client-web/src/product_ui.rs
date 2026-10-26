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
    intent_sender: client::ui_bridge::sender::IntentSender,
    pub quality: client::ui_bridge::quality::QualityBridge,
    quality_now_ms: u64,
    command: Option<Command>,
    ready_world: bool,
    presentation: client::presentation_gate::PresentationGate,
    sequence: u64,
    ui_requests: std::collections::VecDeque<net::ActionId>,
    sounds: client::audio_bridge::PredictionSounds,
    block_sounds: client::audio_bridge::ConfirmedBlockSounds,
    entity_sounds: client::audio_bridge::EntitySounds,
    causal_sounds: client::audio_bridge::CausalSounds,
    machine_sounds: client::audio_bridge::MachineSounds,
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
        let quality = client::ui_bridge::quality::QualityBridge::new(&layer.app.settings);
        let mut result = Self {
            layer,
            audio,
            imports: BrowserImports::prepare()?,
            bridge: UiBridge::default(),
            intent_sender: Default::default(),
            quality,
            quality_now_ms: 0,
            command: None,
            ready_world: false,
            presentation: Default::default(),
            sequence: 0,
            ui_requests: std::collections::VecDeque::with_capacity(128),
            sounds: client::audio_bridge::PredictionSounds::default(),
            block_sounds: Default::default(),
            entity_sounds: Default::default(),
            causal_sounds: Default::default(),
            machine_sounds: Default::default(),
        };
        result.apply_sound()?;
        Ok(result)
    }
    pub fn settings_target(&self) -> &ui::settings::Settings {
        self.quality.target()
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
    pub fn replace_sounds(&mut self, prepared: PreparedSounds) -> Result<(), JsValue> {
        self.audio.replace_bank(prepared)?;
        // The replacement dropped the old loop voices. Rediscover only the
        // still-confirmed public emitters on the next authority update.
        self.machine_sounds.clear(|_| {});
        Ok(())
    }
    pub fn machine_loops(&self) -> usize {
        self.machine_sounds.active()
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
        if !self.ready_world && matches!(self.layer.app.screen, Screen::Game | Screen::Death) {
            if pressed && code == "Escape" {
                self.command = Some(Command::Disconnect);
            } else if pressed && code == "F8" {
                self.command = Some(Command::BugReport);
            }
            return true;
        }
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
    pub fn sync_quality(&mut self, view: &mut ClientView, now_ms: u64, benchmark: bool) {
        self.quality_now_ms = now_ms;
        if !self.ready_world && matches!(self.layer.app.screen, Screen::Game | Screen::Death) {
            self.quality.runtime.set_active(false, now_ms);
            self.layer.app.set_quality(self.quality.hud());
            return;
        }
        if let Some(request) = self
            .quality
            .advance(&self.layer.app, view, now_ms, benchmark)
        {
            if view.settings_application().reload_required != 0
                && view.near_settings_error().is_none()
            {
                self.command = Some(Command::Connect {
                    address: self.layer.app.address.clone(),
                    name: self.layer.app.name.clone(),
                });
                self.connecting();
                let _ = self
                    .quality
                    .advance(&self.layer.app, view, now_ms, benchmark);
            } else {
                let _ = request;
            }
        }
        self.layer.app.set_quality(self.quality.hud());
    }
    pub fn sync(&mut self, network: &Connection, view: &ClientView) -> Result<(), JsValue> {
        let receipt = view.settings_application();
        self.layer.app.set_pending_settings(
            receipt.pending_render & !(1 << ui::settings::Setting::Hold60 as u32),
        );
        self.layer.app.set_reload_settings(receipt.reload_required);
        self.block_sounds.observe(network);
        if let (Some(welcome), Some(player), Some(source)) =
            (network.welcome(), network.player(), view.props_source())
        {
            self.machine_sounds.set_time(network.acknowledged_tick());
            self.machine_sounds.visit(
                welcome.session,
                &view.scene,
                source,
                player.eye(1.0),
                |command| match command {
                    client::audio_bridge::MachineCommand::Start(cue) => {
                        let _ = self.audio.event_gain(
                            cue.event,
                            cue.emitter,
                            Some(cue.position),
                            cue.seed,
                            cue.gain,
                        );
                    }
                    client::audio_bridge::MachineCommand::Stop(emitter) => {
                        let _ = self.audio.forget_emitter(emitter);
                    }
                },
            );
        }
        if let Some(player) = network.player() {
            self.entity_sounds.visit(network, player.eye(1.0), |cue| {
                let _ = self.audio.event_gain(
                    cue.event,
                    cue.emitter,
                    Some(cue.position),
                    cue.seed,
                    cue.gain,
                );
            });
            self.causal_sounds.visit(network, player.eye(1.0), |cue| {
                let _ = self.audio.event_gain(
                    cue.event,
                    cue.emitter,
                    Some(cue.position),
                    cue.seed,
                    cue.gain,
                );
            });
        }
        if let Some(old) = self.sounds.bind(network) {
            let _ = self.audio.forget_emitter(old);
        }
        if let Some(welcome) = network.welcome() {
            let query = |[x, y, z]: [i32; 3]| {
                view.scene.world().read(core_types::BlockPos {
                    space: welcome.player.space,
                    x,
                    y,
                    z,
                })
            };
            let audio = &mut self.audio;
            self.sounds
                .visit(network, &query, |tick, body, emitter, position| {
                    let _ = audio.steps(tick.0, body, emitter, position);
                })
                .map_err(js_error)?;
            if let Some((event, emitter, position)) = self.sounds.health_event(network) {
                let _ = self
                    .audio
                    .event(event, emitter, Some(position), view.tick().0);
            }
            if let Some(player) = network.player() {
                let eye = player.eye(1.0);
                let _ = self.audio.listener(client::audio_bridge::listener(
                    player,
                    [eye[0], eye[1], eye[2]],
                ));
            }
        }
        self.bridge
            .sync_authority(&mut self.layer.app, network)
            .map_err(js_error)?;
        if network.welcome().is_some()
            && !self.ready_world
            && self.layer.app.status() == "Ожидание ответа мира…"
        {
            self.layer.app.set_status("Загрузка мира…");
        }
        Ok(())
    }
    pub fn observe_completed(&mut self, id: u64) {
        match self.presentation.completed(id) {
            Some(client::presentation_gate::Event::PrepareGameUi) => {
                client::presentation_gate::prepare_game_ui(&mut self.layer.app);
            }
            Some(client::presentation_gate::Event::AdmitInput) => self.ready_world = true,
            None => {}
        }
    }
    pub fn submitted(&mut self, id: u64, world: bool, game_ui: bool) {
        self.presentation.submitted(id, world, game_ui);
    }
    pub fn needs_game_ui(&self) -> bool {
        self.presentation.needs_game_ui()
    }
    pub fn game_inputs_ready(&self) -> bool {
        self.ready_world
    }
    pub fn displayed_screen(&self) -> Screen {
        if !self.ready_world && matches!(self.layer.app.screen, Screen::Game | Screen::Death) {
            Screen::Loading
        } else {
            self.layer.app.screen
        }
    }
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) -> Result<bool, &'static str> {
        let screen = self.layer.app.screen;
        if !self.ready_world
            && !self.needs_game_ui()
            && matches!(screen, Screen::Game | Screen::Death)
        {
            self.layer.app.screen = Screen::Loading;
        }
        let result = self.layer.upload(queue, width, height);
        self.layer.app.screen = screen;
        result
    }
    pub fn connecting(&mut self) {
        self.presentation.reset();
        self.intent_sender.clear();
        self.ready_world = false;
        self.layer.app.set_status("Ожидание ответа мира…");
        self.layer.app.open(Screen::Loading);
    }
    pub fn disconnected(&mut self, message: &str) {
        self.presentation.reset();
        self.intent_sender.clear();
        self.quality.cancel_pending(self.quality_now_ms);
        self.machine_sounds.clear(|command| {
            if let client::audio_bridge::MachineCommand::Stop(emitter) = command {
                let _ = self.audio.forget_emitter(emitter);
            }
        });
        self.block_sounds.clear();
        self.entity_sounds.clear();
        self.causal_sounds.clear();
        self.ready_world = false;
        self.bridge = UiBridge::default();
        self.layer.app.clear_chat();
        if let Some(emitter) = self.sounds.emitter() {
            let _ = self.audio.forget_emitter(emitter);
        }
        self.sounds = client::audio_bridge::PredictionSounds::default();
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
        if let Some(socket) = socket.as_deref_mut() {
            for _ in 0..128 {
                let Some(reply) = socket.session_mut().connection_mut().take_reply() else {
                    break;
                };
                if let Some(cue) = self.block_sounds.reply(reply) {
                    let _ = self.audio.event_gain(
                        cue.event,
                        cue.emitter,
                        Some(cue.position),
                        cue.seed,
                        cue.gain,
                    );
                }
                if !self.ui_requests.contains(&reply.id) {
                    continue;
                }
                match reply.status {
                    net::ActionStatus::Pending => {}
                    net::ActionStatus::Accepted => {
                        self.ui_requests.retain(|id| *id != reply.id);
                        self.layer.app.set_status("");
                    }
                    net::ActionStatus::Rejected(reason) => {
                        self.ui_requests.retain(|id| *id != reply.id);
                        let message = match reason {
                            net::RejectReason::Unauthorized => "Недостаточно прав",
                            net::RejectReason::Stale => "Окно изменилось. Повторите действие",
                            net::RejectReason::Unknown => "Область ещё загружается",
                            net::RejectReason::RateLimited => "Слишком много действий. Подождите",
                            net::RejectReason::Unsupported => "Действие не поддерживается миром",
                            net::RejectReason::OutOfReach => "Слишком далеко",
                            net::RejectReason::StorageUnavailable => {
                                "Мир пока не может сохранить действие"
                            }
                            _ => "Мир отклонил действие",
                        };
                        self.layer.app.set_status(message);
                        self.layer.app.message(message);
                    }
                }
            }
        }
        for _ in 0..64 {
            let Some(intent) = self.intent_sender.take(&mut self.layer.app) else {
                break;
            };
            match intent {
                Intent::Connect { address, name } => {
                    self.save_preferences();
                    self.connecting();
                    self.command = Some(Command::Connect { address, name });
                }
                Intent::Disconnect => {
                    self.intent_sender.clear();
                    self.command = Some(Command::Disconnect);
                }
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
                    self.quality_now_ms = now_ms;
                    self.quality.update_saved(&self.layer.app.settings, now_ms);
                    self.apply_sound()?;
                    self.save_preferences();
                    let previous_near_error = view.near_settings_error();
                    let applied = view.apply_settings(self.quality.target());
                    self.layer.app.set_pending_settings(
                        applied.pending_render & !(1 << ui::settings::Setting::Hold60 as u32),
                    );
                    self.layer.app.set_reload_settings(applied.reload_required);
                    view.invalidate();
                    if let Some(error) = view.near_settings_error() {
                        self.layer.app.set_status(error.message());
                    } else if previous_near_error
                        .is_some_and(|error| self.layer.app.status() == error.message())
                    {
                        self.layer.app.set_status("");
                    }
                    if applied.reload_required != 0
                        && view.near_settings_error().is_none()
                        && self.layer.app.snapshot.connected
                    {
                        self.command = Some(Command::Connect {
                            address: self.layer.app.address.clone(),
                            name: self.layer.app.name.clone(),
                        });
                        self.connecting();
                    }
                }
                Intent::SavePreferences => self.save_preferences(),
                Intent::ImportPack => self.imports.choose_pack(),
                Intent::ImportMinecraft => self.imports.choose_minecraft(),
                Intent::OpenCredits => download(
                    "CREDITS.txt",
                    &format!(
                        "{}\n\n{}\n\n{}",
                        ui::font::FONT_CREDITS,
                        audio::catalog::CREDITS,
                        include_str!("../../../ASSET_CREDITS.md")
                    ),
                    "text/plain;charset=utf-8",
                )?,
                Intent::PersonalTime(_) => {
                    self.quality_now_ms = now_ms;
                    self.quality.update_saved(&self.layer.app.settings, now_ms);
                    self.save_preferences();
                    let receipt = view.apply_settings(self.quality.target());
                    self.layer.app.set_pending_settings(
                        receipt.pending_render & !(1 << ui::settings::Setting::Hold60 as u32),
                    );
                }
                other => {
                    if let Some(socket) = socket.as_deref_mut() {
                        let dispatched = {
                            let mut session = socket.session_mut();
                            self.intent_sender
                                .send(session.connection_mut(), other, now_ms)
                        };
                        match dispatched {
                            Ok(client::ui_bridge::Dispatch::Sent(id)) => {
                                self.block_sounds.observe(socket.session().connection());
                                if self.ui_requests.len() == 128 {
                                    self.ui_requests.pop_front();
                                }
                                self.ui_requests.push_back(id);
                            }
                            Ok(client::ui_bridge::Dispatch::Unsupported(_)) => {
                                self.layer.app.set_status("Действие недоступно в этом окне")
                            }
                            Ok(client::ui_bridge::Dispatch::Local(_)) => {}
                            Err(client::network::NetworkError::Capacity) => {
                                self.layer.app.set_status("Действие ожидает отправки");
                                break;
                            }
                            Err(_) => self.layer.app.set_status("Не удалось отправить действие"),
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
/// Local presentation diagnostics. No credential or imported byte/path field is
/// present, and this object is never added to a network frame or F8 export.
#[wasm_bindgen]
pub fn product_ui_stats() -> JsValue {
    super::ACTIVE_VIEW.with(|active| {
        let Some(state) = active.borrow().as_ref().and_then(std::rc::Weak::upgrade) else {
            return JsValue::UNDEFINED;
        };
        let state = state.borrow();
        let Some(product) = &state.product else {
            return JsValue::UNDEFINED;
        };
        let value = js_sys::Object::new();
        let app = &product.layer.app;
        let audio = product.audio.output().work();
        let quality = product.quality.runtime.stats();
        for (name, text) in [
            ("screen", format!("{:?}", product.displayed_screen())),
            ("role", format!("{:?}", app.snapshot.role)),
            ("windowId", format!("{:016x}", app.snapshot.window_id)),
            (
                "windowRevision",
                format!("{:016x}", app.snapshot.window_revision),
            ),
            ("lastChat", app.last_chat().unwrap_or("").to_owned()),
            (
                "audioState",
                format!("{:?}", product.audio.output().context_state()),
            ),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &text.into());
        }
        for (name, number) in [
            (
                "inventoryItems",
                app.snapshot
                    .inventory
                    .iter()
                    .map(|stack| u64::from(stack.count()))
                    .sum(),
            ),
            ("chatMessages", app.chat_len() as u64),
            ("players", app.snapshot.players.len() as u64),
            ("audioStarted", audio.started),
            ("audioLoopUpdates", audio.loop_updates),
            ("audioStopped", audio.stopped),
            ("audioAcceptedBlockCues", product.block_sounds.played()),
            ("audioEntityCues", product.entity_sounds.emitted),
            ("audioCausalCues", product.causal_sounds.emitted),
            ("audioCausalLost", product.causal_sounds.lost),
            ("qualityWindows", quality.windows),
            ("qualityProposals", quality.proposals),
            ("qualityApplied", quality.applied),
            ("qualityFailed", quality.failed),
            ("qualityMeasuredSavings", quality.measured_savings),
            ("qualityGpuP95Us", u64::from(quality.gpu_p95_us)),
            ("qualityFrameP95Us", u64::from(quality.frame_p95_us)),
            (
                "viewTimeChoice",
                app.settings.get(ui::settings::Setting::ViewTime) as u64,
            ),
            (
                "requestedScale",
                app.settings.get(ui::settings::Setting::Scale) as u64,
            ),
            (
                "requestedExposureMode",
                app.settings.get(ui::settings::Setting::ExposureMode) as u64,
            ),
            (
                "requestedSkyStyle",
                app.settings.get(ui::settings::Setting::SkyStyle) as u64,
            ),
            (
                "exposureAutomatic",
                u64::from(state.view.exposure_automatic().unwrap_or(false)),
            ),
            (
                "celestialPrepared",
                u64::from(state.view.celestial_styles().is_some()),
            ),
            (
                "sunStyle",
                state.view.celestial_styles().map_or(0, |v| u64::from(v[0])),
            ),
            (
                "moonStyle",
                state.view.celestial_styles().map_or(0, |v| u64::from(v[1])),
            ),
            (
                "starStyle",
                state.view.celestial_styles().map_or(0, |v| u64::from(v[2])),
            ),
            ("renderScale", u64::from(state.view.render_scale())),
            (
                "requestedNear",
                app.settings.get(ui::settings::Setting::Near) as u64,
            ),
            (
                "effectiveNearBlocks",
                u64::from(state.view.effective_near_blocks()),
            ),
            (
                "nearUnsupported",
                u64::from(state.view.near_settings_error().is_some()),
            ),
            ("smaaPrepared", u64::from(state.view.smaa_prepared())),
            ("smaaReservedBytes", state.view.smaa_reserved_bytes()),
            ("renderWidth", u64::from(state.view.render_extent()[0])),
            ("renderHeight", u64::from(state.view.render_extent()[1])),
            (
                "requestedSamples",
                app.settings.get(ui::settings::Setting::Msaa) as u64,
            ),
            (
                "requestedHorizon",
                app.settings.get(ui::settings::Setting::Horizon) as u64,
            ),
            (
                "requestedClouds",
                app.settings.get(ui::settings::Setting::Clouds) as u64,
            ),
            (
                "requestedSsao",
                app.settings.get(ui::settings::Setting::Ssao) as u64,
            ),
            (
                "requestedGi",
                app.settings.get(ui::settings::Setting::Gi) as u64,
            ),
            (
                "requestedReflections",
                app.settings.get(ui::settings::Setting::Reflections) as u64,
            ),
            (
                "requestedHold60",
                app.settings.get(ui::settings::Setting::Hold60) as u64,
            ),
            (
                "settingsApplied",
                u64::from(
                    state.view.settings_application().applied
                        | 1 << ui::settings::Setting::Hold60 as u32,
                ),
            ),
            (
                "settingsPending",
                u64::from(
                    state.view.settings_application().pending_render
                        & !(1 << ui::settings::Setting::Hold60 as u32),
                ),
            ),
            (
                "settingsReloadRequired",
                u64::from(state.view.settings_application().reload_required),
            ),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &(number as f64).into());
        }
        let _ = js_sys::Reflect::set(
            &value,
            &"pingMeasured".into(),
            &app.snapshot
                .players
                .iter()
                .any(|player| player.ping_ms.is_some())
                .into(),
        );
        value.into()
    })
}
/// Read-only visible control bounds for accessibility and real-input bots.
/// This exports no field values and never invokes a UI or world action.
#[wasm_bindgen]
pub fn product_ui_widget(name: &str) -> JsValue {
    super::ACTIVE_VIEW.with(|active| {
        let Some(state) = active.borrow().as_ref().and_then(std::rc::Weak::upgrade) else {
            return JsValue::NULL;
        };
        let state = state.borrow();
        let Some(product) = &state.product else {
            return JsValue::NULL;
        };
        let Some(hit) = product
            .layer
            .app
            .hits()
            .iter()
            .find(|hit| format!("{:?}", hit.widget) == name)
        else {
            return JsValue::NULL;
        };
        let sx = state.canvas.client_width().max(1) as f32 / state.config.width as f32;
        let sy = state.canvas.client_height().max(1) as f32 / state.config.height as f32;
        let result = js_sys::Array::new();
        for number in [
            hit.rect.x * sx,
            hit.rect.y * sy,
            hit.rect.w * sx,
            hit.rect.h * sy,
        ] {
            result.push(&number.into());
        }
        result.into()
    })
}
