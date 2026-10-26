//! Native event/file/device glue for the same Rust UI and authority adapter.
use audio::{
    EmitterId,
    scene::{NativeScene, PreparedSounds},
};
use client::{network::Connection, ui_bridge::UiBridge, view::ClientView};
use std::{collections::VecDeque, io::Read, path::PathBuf, sync::mpsc};
use ui::{
    app::{Focus, Intent, Modifiers, Screen, Snapshot},
    layer::UiLayer,
};

pub struct Prepared {
    layer: UiLayer,
    bank: PreparedSounds,
}
pub fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> Result<Box<Prepared>, String> {
    let layer = UiLayer::prepare(device, queue, format, false).map_err(str::to_owned)?;
    let bank = PreparedSounds::prepare(None, None).map_err(|error| error.to_string())?;
    Ok(Box::new(Prepared { layer, bank }))
}
pub enum Command {
    Connect { address: String, name: String },
    Disconnect,
    Capture(bool),
    BugReport,
    Quit,
}
struct Imported {
    local: audio::resources::LocalResources,
    bank: PreparedSounds,
    materials: Option<render::material_pack::PreparedMaterials>,
}
pub struct ProductUi {
    pub layer: UiLayer,
    audio: Option<NativeScene>,
    bridge: UiBridge,
    sounds: client::audio_bridge::PredictionSounds,
    block_sounds: client::audio_bridge::ConfirmedBlockSounds,
    entity_sounds: client::audio_bridge::EntitySounds,
    commands: VecDeque<Command>,
    requests: VecDeque<net::ActionId>,
    local: Option<audio::resources::LocalResources>,
    importing: Option<mpsc::Receiver<Result<Option<Box<Imported>>, String>>>,
    ready_world: bool,
    sequence: u64,
    preferences: PathBuf,
    diagnostics: bool,
    last_diagnostic: Option<DiagnosticStamp>,
    last_aim: Option<(Option<[i32; 3]>, u16)>,
    last_world_diagnostic: Option<(usize, usize, bool, Option<u8>)>,
    last_light_diagnostic: Option<(bool, usize, usize, u32, u64)>,
    audio_samples: u64,
    resource_epoch: u64,
}
type DiagnosticStamp = (
    Screen,
    u64,
    u64,
    usize,
    usize,
    bool,
    u64,
    ui::app::Role,
    bool,
);
pub fn data_path(name: &str) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/client"))
        .join("CubiCyber")
        .join(name)
}
impl ProductUi {
    pub fn from_prepared(prepared: Prepared) -> Box<Self> {
        let Prepared { mut layer, bank } = prepared;
        let preferences = data_path("ui.uip");
        let preferences_bytes = std::fs::File::open(&preferences).and_then(|file| {
            let mut bytes = Vec::new();
            file.take(8193).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        if let Ok(bytes) = preferences_bytes
            && bytes.len() <= 8192
        {
            let _ = layer.app.load_preferences(&bytes);
        }
        let audio = match NativeScene::open(bank) {
            Ok(audio) => Some(audio),
            Err(error) => {
                eprintln!("NATIVE_AUDIO_ENDPOINT_UNAVAILABLE {error}");
                layer.app.set_status("Звуковое устройство недоступно");
                None
            }
        };
        let mut result = Box::new(Self {
            layer,
            audio,
            bridge: UiBridge::default(),
            sounds: Default::default(),
            block_sounds: Default::default(),
            entity_sounds: Default::default(),
            commands: VecDeque::with_capacity(8),
            requests: VecDeque::with_capacity(128),
            local: None,
            importing: None,
            ready_world: false,
            sequence: 0,
            preferences,
            diagnostics: std::env::var_os("CUBI_UI_DIAGNOSTICS").is_some(),
            last_diagnostic: None,
            last_aim: None,
            last_world_diagnostic: None,
            last_light_diagnostic: None,
            audio_samples: 0,
            resource_epoch: 0,
        });
        result.apply_sound();
        let startup = result.layer.take_work();
        println!(
            "UI_SETUP pipeline_compilations={}",
            startup.pipeline_compilations
        );
        result.report_state();
        result
    }
    fn report_state(&mut self) {
        if !self.diagnostics {
            return;
        }
        let snapshot = &self.layer.app.snapshot;
        let stamp = (
            self.layer.app.screen,
            snapshot.window_id,
            snapshot.inventory_revision,
            self.layer.app.chat_len(),
            snapshot.players.len(),
            snapshot
                .players
                .iter()
                .any(|player| player.ping_ms.is_some()),
            snapshot.window_revision,
            snapshot.role,
            snapshot.creative,
        );
        if self.last_diagnostic == Some(stamp) {
            return;
        }
        self.last_diagnostic = Some(stamp);
        println!("NATIVE_UI_STATUS {:?}", self.layer.app.status());
        println!(
            "NATIVE_UI_STATE screen={:?} window={:016x} inventory_revision={} chat_messages={} players={} ping_measured={} window_revision={} role={:?}",
            stamp.0, stamp.1, stamp.2, stamp.3, stamp.4, stamp.5, stamp.6, stamp.7
        );
        println!(
            "NATIVE_UI_WINDOW inventory_items={} cursor_item={} cursor_count={} craft0_item={} craft0_count={} result_item={} result_count={} container_items={} selected={} creative={}",
            snapshot
                .inventory
                .iter()
                .map(|stack| u64::from(stack.count()))
                .sum::<u64>(),
            snapshot.cursor.item(),
            snapshot.cursor.count(),
            snapshot.craft[0].item(),
            snapshot.craft[0].count(),
            snapshot.craft_result.item(),
            snapshot.craft_result.count(),
            snapshot
                .container
                .iter()
                .map(|stack| u64::from(stack.count()))
                .sum::<u64>(),
            snapshot.selected,
            u8::from(snapshot.creative),
        );
        if let Some(audio) = &self.audio
            && let Ok(work) = audio.output().with_mixer(|mixer| mixer.take_work())
        {
            self.audio_samples = self.audio_samples.saturating_add(work.voice_samples);
            println!(
                "NATIVE_UI_AUDIO virtual={} frames={} voice_samples={} callback_errors={} missed_callbacks={}",
                std::env::var_os("CUBI_AUDIO_VIRTUAL").is_some(),
                work.frames,
                self.audio_samples,
                audio.output().callback_errors(),
                audio.output().missed_callbacks()
            );
        }
    }
    pub fn take_command(&mut self) -> Option<Command> {
        self.commands.pop_front()
    }
    pub fn painted(&self) {
        if self.diagnostics {
            println!(
                "NATIVE_UI_PAINTED screen={:?} window={:016x} tab={} debug={} revision={} window_revision={}",
                self.layer.app.screen,
                self.layer.app.snapshot.window_id,
                self.layer.app.player_list,
                self.layer.app.show_debug,
                self.layer.app.revision(),
                self.layer.app.snapshot.window_revision
            );
        }
    }
    pub fn presentation(&self) -> Option<impl FnOnce() + Send + 'static> {
        if !self.diagnostics {
            return None;
        }
        let revision = self.layer.uploaded_revision()?;
        Some(move || println!("NATIVE_UI_PRESENTED revision={revision}"))
    }
    pub fn trace_motion(&self, delta: [f64; 2], yaw: f32, pitch: f32) {
        if self.diagnostics {
            println!(
                "NATIVE_UI_MOTION dx={} dy={} yaw={} pitch={}",
                delta[0], delta[1], yaw, pitch
            );
        }
    }
    fn command(&mut self, command: Command) {
        if self.commands.len() < 8 {
            self.commands.push_back(command);
        }
    }
    pub fn save_preferences(&mut self) {
        let bytes = self.layer.app.encode_preferences();
        let result = self
            .preferences
            .parent()
            .ok_or_else(|| std::io::Error::other("settings directory"))
            .and_then(std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.preferences, bytes));
        if result.is_err() {
            self.layer.app.set_status("Не удалось сохранить настройки");
        }
    }
    fn apply_sound(&mut self) {
        if let Some(audio) = &self.audio {
            let _ = audio.output().with_mixer(|mixer| {
                client::ui_bridge::apply_sound(&self.layer.app.settings, mixer)
            });
        }
    }
    pub fn click_sound(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        if let Some(audio) = &self.audio {
            let _ = audio.event("ui.click", EmitterId(u64::MAX), None, self.sequence);
        }
    }
    pub fn key(
        &mut self,
        view: Option<&mut ClientView>,
        code: &str,
        pressed: bool,
        text: Option<&str>,
        modifiers: Modifiers,
    ) {
        let focus = matches!(
            self.layer.app.focus,
            Focus::Name | Focus::Address | Focus::Search | Focus::Chat
        );
        if let Some(view) = view {
            client::ui_bridge::apply_key(&mut self.layer.app, view, code, pressed, modifiers);
        } else {
            client::ui_bridge::route_key(&mut self.layer.app, code, pressed, modifiers);
        }
        if pressed
            && focus
            && !modifiers.ctrl
            && let Some(text) = text
            && text.chars().all(|c| !c.is_control())
        {
            self.layer.app.text(text);
        }
    }
    pub fn connecting(&mut self) {
        self.ready_world = false;
        self.layer.app.set_status("Ожидание ответа мира…");
        self.layer.app.open(Screen::Loading);
        self.report_state();
    }
    pub fn disconnected(&mut self, message: &str) {
        self.block_sounds.clear();
        self.ready_world = false;
        self.requests.clear();
        self.bridge = UiBridge::default();
        self.sounds = Default::default();
        self.entity_sounds.clear();
        self.layer.app.clear_chat();
        let _ = self.layer.app.set_snapshot(Snapshot::default());
        self.layer.app.open(Screen::Welcome);
        self.layer.app.set_status(message);
        self.report_state();
    }
    pub fn sync(&mut self, network: &Connection, view: &ClientView) -> Result<(), String> {
        self.block_sounds.observe(network);
        if self.diagnostics {
            if let Some(light) = view.lighting_stats() {
                let stamp = (
                    light.ready,
                    light.pending_pages,
                    light.pending_cells,
                    light.pending_shadow_cascades,
                    light.oldest_age_ticks,
                );
                if self.last_light_diagnostic != Some(stamp) {
                    println!(
                        "NATIVE_UI_LIGHT ready={} pending_pages={} pending_cells={} pending_shadows={} oldest_ticks={}",
                        stamp.0, stamp.1, stamp.2, stamp.3, stamp.4
                    );
                    self.last_light_diagnostic = Some(stamp);
                }
            }
            let stats = view.stats();
            let position = stats.player_pos;
            let light = view.scene.read_authoritative_light(core_types::BlockPos {
                space: view.scene.config().center.space,
                x: position[0].floor() as i32,
                y: position[1].floor() as i32 + 2,
                z: position[2].floor() as i32,
            });
            let stamp = (
                network.light_replica().len(),
                view.horizon_stats().map_or(0, |s| s.height_front),
                view.environment_stats().is_some(),
                light.map(|s| s.sky),
            );
            if self.last_world_diagnostic != Some(stamp) {
                println!(
                    "NATIVE_UI_WORLD light_pages={} far_tiles={} environment={} authoritative={} eye_sky={:?}",
                    stamp.0,
                    stamp.1,
                    stamp.2,
                    view.lighting_stats()
                        .is_some_and(|s| s.authoritative_source),
                    stamp.3
                );
                self.last_world_diagnostic = Some(stamp);
            }
            let aim = (stats.target_position, stats.target_cell);
            if self.last_aim != Some(aim) {
                println!("NATIVE_UI_AIM target={:?} cell={}", aim.0, aim.1);
                self.last_aim = Some(aim);
            }
        }
        if let Some(old) = self.sounds.bind(network)
            && let Some(audio) = &mut self.audio
        {
            let _ = audio.forget_emitter(old);
        }
        if let Some(audio) = &mut self.audio {
            if let Some(player) = network.player() {
                self.entity_sounds.visit(network, player.eye(1.0), |cue| {
                    let _ = audio.event_gain(
                        cue.event,
                        cue.emitter,
                        Some(cue.position),
                        cue.seed,
                        cue.gain,
                    );
                });
            }
            let world = view.scene.world();
            self.sounds
                .visit(
                    network,
                    &|position| {
                        let [x, y, z] = position;
                        world.read(core_types::BlockPos {
                            space: network
                                .welcome()
                                .map_or(core_types::SpaceId(0), |w| w.player.space),
                            x,
                            y,
                            z,
                        })
                    },
                    |tick, body, emitter, position| {
                        let _ = audio.steps(tick.0, body, emitter, position);
                    },
                )
                .map_err(|_| "invalid audio tick history")?;
            if let Some((event, emitter, position)) = self.sounds.health_event(network) {
                let _ = audio.event(event, emitter, Some(position), view.tick().0);
            }
            if let Some(player) = network.player() {
                let _ = audio.listener(client::audio_bridge::listener(player, player.eye(1.0)));
            }
        }
        self.bridge
            .sync_authority(&mut self.layer.app, network)
            .map_err(str::to_owned)?;
        if network.player().is_some() && view.is_ready() && !self.ready_world {
            self.ready_world = true;
            self.layer.app.set_status("");
            if self.layer.app.screen == Screen::Loading {
                self.layer.app.open(Screen::Game);
            }
        }
        self.report_state();
        Ok(())
    }
    pub fn drain(
        &mut self,
        mut view: Option<&mut ClientView>,
        mut network: Option<&mut Connection>,
        now: u64,
    ) {
        if let Some(network) = network.as_deref_mut() {
            for _ in 0..128 {
                let Some(reply) = network.take_reply() else {
                    break;
                };
                if self.diagnostics {
                    println!(
                        "NATIVE_UI_REPLY id={} status={:?}",
                        reply.id.0, reply.status
                    );
                }
                if let Some(cue) = self.block_sounds.reply(reply)
                    && let Some(audio) = &self.audio
                {
                    let played = audio.event_gain(
                        cue.event,
                        cue.emitter,
                        Some(cue.position),
                        cue.seed,
                        cue.gain,
                    );
                    if self.diagnostics && played.is_ok() {
                        println!(
                            "NATIVE_UI_BLOCK_AUDIO accepted_cues={}",
                            self.block_sounds.played()
                        );
                    }
                }
                if !self.requests.contains(&reply.id) {
                    continue;
                }
                match reply.status {
                    net::ActionStatus::Pending => {}
                    net::ActionStatus::Accepted => {
                        self.requests.retain(|id| *id != reply.id);
                        self.layer.app.set_status("");
                    }
                    net::ActionStatus::Rejected(reason) => {
                        self.requests.retain(|id| *id != reply.id);
                        let message = match reason {
                            net::RejectReason::Unauthorized => "Недостаточно прав",
                            net::RejectReason::Stale => "Окно изменилось. Повторите действие",
                            net::RejectReason::Unknown => "Область ещё загружается",
                            net::RejectReason::Unsupported => "Действие недоступно в этом окне",
                            net::RejectReason::RateLimited => "Слишком много действий. Подождите",
                            net::RejectReason::OutOfReach => "Слишком далеко",
                            _ => "Мир отклонил действие",
                        };
                        self.layer.app.set_status(message);
                        self.layer.app.message(message);
                    }
                }
            }
        }
        for _ in 0..64 {
            let Some(intent) = self.layer.app.take_intent() else {
                break;
            };
            match intent {
                Intent::Connect { address, name } => {
                    self.save_preferences();
                    self.connecting();
                    self.command(Command::Connect { address, name });
                }
                Intent::Disconnect => self.command(Command::Disconnect),
                Intent::Quit => self.command(Command::Quit),
                Intent::BugReport => self.command(Command::BugReport),
                Intent::PointerCapture(capture) => self.command(Command::Capture(capture)),
                Intent::SettingsChanged => {
                    self.apply_sound();
                    self.save_preferences();
                    if let Some(view) = view.as_deref_mut() {
                        let applied = view.apply_settings(&self.layer.app.settings);
                        self.layer.app.set_pending_settings(applied.pending_render);
                        view.invalidate();
                    }
                }
                Intent::SavePreferences => self.save_preferences(),
                Intent::ImportPack => self.begin_import(false),
                Intent::ImportMinecraft => self.begin_import(true),
                Intent::OpenCredits => {
                    let text = format!(
                        "{}\n\n{}\n\n{}",
                        ui::font::FONT_CREDITS,
                        audio::catalog::CREDITS,
                        include_str!("../../../ASSET_CREDITS.md")
                    );
                    let path = data_path("CREDITS.txt");
                    let result = path
                        .parent()
                        .map(std::fs::create_dir_all)
                        .transpose()
                        .and_then(|_| std::fs::write(path, text));
                    self.layer.app.set_status(if result.is_ok() {
                        "Авторы и лицензии сохранены рядом с настройками"
                    } else {
                        "Не удалось сохранить уведомления"
                    });
                }
                Intent::PersonalTime(_) => {
                    if let Some(view) = view.as_deref_mut() {
                        view.invalidate();
                    }
                }
                other => {
                    if let Some(network) = network.as_deref_mut() {
                        match client::ui_bridge::dispatch(network, other, now) {
                            Ok(client::ui_bridge::Dispatch::Sent(id)) => {
                                self.block_sounds.observe(network);
                                if self.requests.len() == 128 {
                                    self.requests.pop_front();
                                }
                                self.requests.push_back(id);
                            }
                            Ok(client::ui_bridge::Dispatch::Unsupported(_)) => {
                                self.layer.app.set_status("Действие недоступно в этом окне")
                            }
                            Err(_) => self.layer.app.set_status("Нет соединения с миром"),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    fn begin_import(&mut self, minecraft: bool) {
        if self.importing.is_some() {
            return;
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = minecraft;
            self.layer
                .app
                .set_status("Выбор локальных файлов доступен в Windows и браузере");
        }
        #[cfg(target_os = "windows")]
        {
            let (sender, receiver) = mpsc::sync_channel(1);
            self.importing = Some(receiver);
            self.layer.app.set_status("Выберите локальные ресурсы…");
            std::thread::spawn(move || {
                let result = (|| {
                    let local = if minecraft {
                        pollster::block_on(audio::local_files::choose_minecraft())
                    } else {
                        pollster::block_on(audio::local_files::choose_pack())
                    }
                    .map_err(|e| format!("{e:?}"))?;
                    let Some(local) = local else {
                        return Ok(None);
                    };
                    let bank = PreparedSounds::prepare(Some(&local), local.minecraft_index())
                        .map_err(|e| e.to_string())?;
                    let materials = client::local_materials::prepare(&local, Default::default())
                        .map_err(|e| format!("{e:?}"))?;
                    Ok(Some(Box::new(Imported {
                        local,
                        bank,
                        materials,
                    })))
                })();
                let _ = sender.send(result);
            });
        }
    }
    /// Poll outside the measured frame; decoding happens on the import worker.
    pub fn poll_import(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: Option<&mut ClientView>,
    ) {
        let Some(receiver) = &self.importing else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(_) => Err("import worker stopped".into()),
        };
        self.importing = None;
        match result {
            Ok(Some(imported)) => {
                let Imported {
                    local,
                    bank,
                    materials,
                } = *imported;
                let revision = self.resource_epoch.saturating_add(1);
                let result = if let Some(view) = view {
                    apply_materials(device, queue, view, materials, revision)
                } else {
                    Ok(())
                };
                if result.is_err() {
                    self.layer
                        .app
                        .set_status("Изображения отклонены; прежние ресурсы сохранены");
                    return;
                }
                if let Some(audio) = &mut self.audio
                    && audio.replace_bank(bank).is_err()
                {
                    self.layer
                        .app
                        .set_status("Не удалось применить звуки набора");
                    return;
                }
                self.local = Some(local);
                self.resource_epoch = revision;
                self.apply_sound();
                self.layer.app.set_status(if self.audio.is_some() {
                    "Локальные ресурсы применены"
                } else {
                    "Изображения применены; звуковое устройство недоступно"
                });
            }
            Ok(None) => self.layer.app.set_status(""),
            Err(_) => self
                .layer
                .app
                .set_status("Локальные ресурсы отклонены; прежние сохранены"),
        }
    }
    pub fn apply_local(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &mut ClientView,
    ) -> Result<(), String> {
        if let Some(local) = &self.local {
            let materials = client::local_materials::prepare(local, Default::default())
                .map_err(|e| format!("{e:?}"))?;
            apply_materials(device, queue, view, materials, self.resource_epoch)?;
        }
        Ok(())
    }
}
fn apply_materials(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &mut ClientView,
    materials: Option<render::material_pack::PreparedMaterials>,
    revision: u64,
) -> Result<(), String> {
    let enabled = materials.is_some();
    let mut prepared = match materials {
        Some(value) => value,
        None => {
            let base = render::terrain_gpu::AtlasImage::donor()?;
            render::material_pack::PreparedMaterials::prepare(
                &base,
                &[],
                revision,
                Default::default(),
            )
            .map_err(|e| format!("{e:?}"))?
        }
    };
    prepared.revision = revision;
    let pack = render::material_pack::MaterialPackGpu::upload(device, queue, &prepared)
        .map_err(|e| format!("{e:?}"))?;
    if pack.reserved_metals {
        return Err("unsupported reserved metal id".into());
    }
    render::material_pack::apply_albedo_in_place(queue, view.renderer_mut().geometry(), &prepared)
        .map_err(|e| format!("{e:?}"))?;
    view.renderer_mut()
        .replace_material_pack(device, queue, pack, enabled)
        .map_err(|e| e.to_string())?;
    view.invalidate();
    Ok(())
}
