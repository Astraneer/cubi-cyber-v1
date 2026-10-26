//! Native event/file/device glue for the same Rust UI and authority adapter.
mod consumable_diagnostics;
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
    intent_sender: client::ui_bridge::sender::IntentSender,
    pub quality: client::ui_bridge::quality::QualityBridge,
    quality_now_ms: u64,
    sounds: client::audio_bridge::PredictionSounds,
    block_sounds: client::audio_bridge::ConfirmedBlockSounds,
    entity_sounds: client::audio_bridge::EntitySounds,
    causal_sounds: client::audio_bridge::CausalSounds,
    machine_sounds: client::audio_bridge::MachineSounds,
    commands: VecDeque<Command>,
    requests: VecDeque<net::ActionId>,
    local: Option<audio::resources::LocalResources>,
    importing: Option<mpsc::Receiver<Result<Option<Box<Imported>>, String>>>,
    ready_world: bool,
    presentation: client::presentation_gate::PresentationGate,
    sequence: u64,
    preferences: PathBuf,
    diagnostics: bool,
    consumable_diagnostics: consumable_diagnostics::Observations,
    last_diagnostic: Option<DiagnosticStamp>,
    last_aim: Option<(Option<[i32; 3]>, u16)>,
    last_world_diagnostic: Option<(usize, usize, bool, Option<u8>)>,
    last_horizon_diagnostic: Option<HorizonReceiptStamp>,
    last_light_diagnostic: Option<(bool, usize, usize, u32, u64)>,
    last_time_diagnostic: Option<(i32, bool, u32, i32)>,
    last_render_diagnostic: Option<(u32, [u32; 2], bool)>,
    last_near_diagnostic: Option<(i32, u16, bool)>,
    last_visual_diagnostic: Option<(Option<bool>, Option<[u32; 3]>)>,
    last_props_diagnostic: Option<(bool, usize, usize, usize, bool)>,
    last_quality_diagnostic: Option<(u64, u64, u64, u64)>,
    last_settings_diagnostic: Option<[u32; 3]>,
    intent_waiting: bool,
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
    Option<ui::app::ItemFact>,
);
#[derive(Clone, Copy, PartialEq, Eq)]
struct HorizonReceiptStamp {
    front: bool,
    candidate: u64,
    committed: u64,
    projection: bool,
    requested_error_bits: u64,
    committed_error_bits: Option<u64>,
    missing: u64,
    capacity: bool,
    leaf: u64,
    maximum_error_bits: u64,
}
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
        let quality = client::ui_bridge::quality::QualityBridge::new(&layer.app.settings);
        let mut result = Box::new(Self {
            layer,
            audio,
            bridge: UiBridge::default(),
            intent_sender: Default::default(),
            quality,
            quality_now_ms: 0,
            sounds: Default::default(),
            block_sounds: Default::default(),
            entity_sounds: Default::default(),
            causal_sounds: Default::default(),
            machine_sounds: Default::default(),
            commands: VecDeque::with_capacity(8),
            requests: VecDeque::with_capacity(128),
            local: None,
            importing: None,
            ready_world: false,
            presentation: Default::default(),
            sequence: 0,
            preferences,
            diagnostics: std::env::var_os("CUBI_UI_DIAGNOSTICS").is_some(),
            consumable_diagnostics: Default::default(),
            last_diagnostic: None,
            last_aim: None,
            last_world_diagnostic: None,
            last_horizon_diagnostic: None,
            last_light_diagnostic: None,
            last_time_diagnostic: None,
            last_render_diagnostic: None,
            last_near_diagnostic: None,
            last_visual_diagnostic: None,
            last_props_diagnostic: None,
            last_quality_diagnostic: None,
            last_settings_diagnostic: None,
            intent_waiting: false,
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
            self.displayed_screen(),
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
            snapshot.inventory_facts[usize::from(snapshot.selected)],
        );
        if self.last_diagnostic == Some(stamp) {
            return;
        }
        self.last_diagnostic = Some(stamp);
        println!("NATIVE_UI_STATUS {:?}", self.layer.app.status());
        println!(
            "NATIVE_UI_ITEM selected={} item={} fact={:?}",
            snapshot.selected,
            snapshot.inventory[usize::from(snapshot.selected)].item(),
            stamp.9
        );
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
            use ui::{app::Widget, settings::Setting};
            for hit in self.layer.app.hits().iter().filter(|hit| {
                matches!(
                    hit.widget,
                    Widget::Settings
                        | Widget::Crossroads
                        | Widget::Day
                        | Widget::Night
                        | Widget::ServerTime
                        | Widget::Back
                        | Widget::Respawn
                        | Widget::Setting(
                            Setting::ViewTime
                                | Setting::Msaa
                                | Setting::Scale
                                | Setting::Exposure
                                | Setting::Clouds
                                | Setting::Gi
                                | Setting::Reflections
                                | Setting::Ssao
                                | Setting::Horizon
                                | Setting::ExposureMode
                                | Setting::SkyStyle
                        )
                )
            }) {
                println!(
                    "NATIVE_UI_WIDGET name={:?} x={} y={} width={} height={}",
                    hit.widget, hit.rect.x, hit.rect.y, hit.rect.w, hit.rect.h
                );
            }
            println!(
                "NATIVE_UI_SAVED_EFFECTS clouds={} gi={} reflections={} ssao={} horizon={} hold60={}",
                self.layer.app.settings.get(Setting::Clouds),
                self.layer.app.settings.get(Setting::Gi),
                self.layer.app.settings.get(Setting::Reflections),
                self.layer.app.settings.get(Setting::Ssao),
                self.layer.app.settings.get(Setting::Horizon),
                self.layer.app.settings.get(Setting::Hold60)
            );
            println!(
                "NATIVE_UI_PAINTED screen={:?} window={:016x} tab={} debug={} focus={:?} revision={} window_revision={}",
                self.displayed_screen(),
                self.layer.app.snapshot.window_id,
                self.layer.app.player_list,
                self.layer.app.show_debug,
                self.layer.app.focus,
                self.layer.app.revision(),
                self.layer.app.snapshot.window_revision
            );
        }
    }
    pub fn report_cpu_continuation(&self, count: u64) {
        if self.diagnostics && (count == 1 || count.is_multiple_of(60)) {
            println!("NATIVE_UI_CPU_CONTINUATION total={count}");
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
    pub fn settings_target(&self) -> &ui::settings::Settings {
        self.quality.target()
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
        if !self.ready_world && matches!(self.layer.app.screen, Screen::Game | Screen::Death) {
            if pressed && code == "Escape" {
                self.commands.clear();
                self.commands.push_back(Command::Disconnect);
            } else if pressed && code == "F8" {
                self.command(Command::BugReport);
            }
            return;
        }
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
        self.presentation.reset();
        self.intent_sender.clear();
        self.intent_waiting = false;
        self.last_near_diagnostic = None;
        self.last_horizon_diagnostic = None;
        self.ready_world = false;
        self.layer.app.set_status("Ожидание ответа мира…");
        self.layer.app.open(Screen::Loading);
        self.report_state();
    }
    pub fn disconnected(&mut self, message: &str) {
        self.presentation.reset();
        self.intent_sender.clear();
        self.intent_waiting = false;
        self.quality.cancel_pending(self.quality_now_ms);
        if let Some(audio) = &mut self.audio {
            self.machine_sounds.clear(|command| {
                if let client::audio_bridge::MachineCommand::Stop(emitter) = command {
                    let _ = audio.forget_emitter(emitter);
                }
            });
            if let Some(emitter) = self.sounds.emitter() {
                let _ = audio.forget_emitter(emitter);
            }
        } else {
            self.machine_sounds.clear(|_| {});
        }
        self.block_sounds.clear();
        self.ready_world = false;
        self.requests.clear();
        self.bridge = UiBridge::default();
        self.sounds = Default::default();
        self.entity_sounds.clear();
        self.causal_sounds.clear();
        self.layer.app.clear_chat();
        let _ = self.layer.app.set_snapshot(Snapshot::default());
        self.layer.app.open(Screen::Welcome);
        self.layer.app.set_status(message);
        self.report_state();
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
                self.command(Command::Connect {
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
        if self.diagnostics {
            let stats = self.quality.runtime.stats();
            let stamp = (stats.windows, stats.proposals, stats.applied, stats.failed);
            if self.last_quality_diagnostic != Some(stamp) {
                println!(
                    "NATIVE_UI_QUALITY windows={} proposals={} applied={} failed={} measured_savings={} gpu_p95_us={} frame_p95_us={} pending={:?}",
                    stats.windows,
                    stats.proposals,
                    stats.applied,
                    stats.failed,
                    stats.measured_savings,
                    stats.gpu_p95_us,
                    stats.frame_p95_us,
                    self.quality
                        .runtime
                        .pending()
                        .map(|request| request.reduction.setting)
                );
                self.last_quality_diagnostic = Some(stamp);
            }
        }
    }
    pub fn sync(&mut self, network: &Connection, view: &ClientView) -> Result<(), String> {
        let receipt = view.settings_application();
        self.layer.app.set_pending_settings(
            receipt.pending_render & !(1 << ui::settings::Setting::Hold60 as u32),
        );
        self.layer.app.set_reload_settings(receipt.reload_required);
        self.block_sounds.observe(network);
        if self.diagnostics {
            let settings_stamp = [
                receipt.applied,
                receipt.pending_render,
                receipt.reload_required,
            ];
            if self.last_settings_diagnostic != Some(settings_stamp) {
                println!(
                    "NATIVE_UI_SETTINGS applied={} pending={} reload={}",
                    settings_stamp[0], settings_stamp[1], settings_stamp[2]
                );
                self.last_settings_diagnostic = Some(settings_stamp);
            }
            let visual = (view.exposure_automatic(), view.celestial_styles());
            let near = (
                self.layer.app.settings.get(ui::settings::Setting::Near),
                view.effective_near_blocks(),
                view.near_settings_error().is_some(),
            );
            if self.last_near_diagnostic != Some(near) {
                println!(
                    "NATIVE_UI_NEAR requested={} effective={} unsupported={}",
                    near.0, near.1, near.2
                );
                self.last_near_diagnostic = Some(near);
            }
            if self.last_visual_diagnostic != Some(visual) {
                println!(
                    "NATIVE_UI_VISUAL automatic={:?} styles={:?}",
                    visual.0, visual.1
                );
                self.last_visual_diagnostic = Some(visual);
            }
            let render = (
                view.render_scale(),
                view.render_extent(),
                view.smaa_prepared(),
            );
            if self.last_render_diagnostic != Some(render) {
                println!(
                    "NATIVE_UI_RENDER scale={} width={} height={} smaa={} smaa_bytes={}",
                    render.0,
                    render.1[0],
                    render.1[1],
                    render.2,
                    view.smaa_reserved_bytes()
                );
                self.last_render_diagnostic = Some(render);
            }
            let clock = view.sun_clock();
            let stamp = (
                self.layer.app.settings.get(ui::settings::Setting::ViewTime),
                clock.is_some(),
                view.render_sample_count(),
                view.exposure_compensation()
                    .map_or(0, |gain| (gain * 100.).round() as i32),
            );
            if self.last_time_diagnostic != Some(stamp) {
                println!(
                    "NATIVE_UI_TIME choice={} known={} samples={} exposure_percent={} time_of_day={:?} sun_y={:?}",
                    stamp.0,
                    stamp.1,
                    stamp.2,
                    stamp.3,
                    clock.map(|clock| clock.time_of_day),
                    clock.map(|clock| clock.sun_direction[1])
                );
                self.last_time_diagnostic = Some(stamp);
            }
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
            if let (Some(props), Some(source)) = (view.props_stats(), view.props_source()) {
                let stamp = (
                    props.ready,
                    props.pending_records,
                    props.pending_jobs,
                    stats.resident_pages,
                    view.initial_scene_inputs_ready(),
                );
                if self.last_props_diagnostic != Some(stamp) {
                    println!(
                        "NATIVE_UI_DERIVED ready={} full_mesh_ready={} resident_pages={} meshes_published={} queued_meshes={} running_meshes={} inputs_ready={} props={props:?} source={:?}",
                        stats.ready,
                        stats.full_mesh_ready,
                        stats.resident_pages,
                        stats.meshes_published,
                        stats.queued_jobs,
                        stats.running_jobs,
                        stamp.4,
                        source.stats(view.scene.world())
                    );
                    self.last_props_diagnostic = Some(stamp);
                }
            }
            let position = stats.player_pos;
            let light = view.scene.read_authoritative_light(core_types::BlockPos {
                space: view.scene.config().center.space,
                x: position[0].floor() as i32,
                y: position[1].floor() as i32 + 2,
                z: position[2].floor() as i32,
            });
            let horizon = view.horizon_stats();
            let stamp = (
                network.light_replica().len(),
                horizon.map_or(0, |s| s.height_front),
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
            if let Some(horizon) = horizon {
                let stamp = HorizonReceiptStamp {
                    front: view.has_committed_far_front(),
                    candidate: horizon.candidate_plan_id,
                    committed: horizon.committed_plan_id,
                    projection: horizon.projection_present,
                    requested_error_bits: horizon.requested_error_pixels.to_bits(),
                    committed_error_bits: horizon
                        .committed_requested_error_pixels
                        .map(f64::to_bits),
                    missing: horizon.committed_missing_error_certificates,
                    capacity: horizon.committed_selection_capacity_limited,
                    leaf: horizon.committed_selection_leaf_limited,
                    maximum_error_bits: horizon.committed_maximum_error_pixels.to_bits(),
                };
                if self.last_horizon_diagnostic != Some(stamp) {
                    println!(
                        "NATIVE_UI_HORIZON_RECEIPT front={} candidate_plan={} committed_plan={} projection={} requested_error_px={} committed_error_px={:?} committed_missing={} committed_capacity={} committed_leaf={} committed_max_px={}",
                        stamp.front,
                        stamp.candidate,
                        stamp.committed,
                        stamp.projection,
                        horizon.requested_error_pixels,
                        horizon.committed_requested_error_pixels,
                        stamp.missing,
                        stamp.capacity,
                        stamp.leaf,
                        horizon.committed_maximum_error_pixels,
                    );
                    self.last_horizon_diagnostic = Some(stamp);
                }
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
                            let _ = audio.event_gain(
                                cue.event,
                                cue.emitter,
                                Some(cue.position),
                                cue.seed,
                                cue.gain,
                            );
                        }
                        client::audio_bridge::MachineCommand::Stop(emitter) => {
                            let _ = audio.forget_emitter(emitter);
                        }
                    },
                );
            }
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
                self.causal_sounds.visit(network, player.eye(1.0), |cue| {
                    let played = audio
                        .event_gain(
                            cue.event,
                            cue.emitter,
                            Some(cue.position),
                            cue.seed,
                            cue.gain,
                        )
                        .is_ok();
                    if self.diagnostics {
                        println!(
                            "NATIVE_UI_CAUSAL_AUDIO {{\"event\":\"{}\",\"emitter\":{},\"seed\":{},\"played\":{}}}",
                            cue.event, cue.emitter.0, cue.seed, played,
                        );
                    }
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
        if self.diagnostics {
            self.consumable_diagnostics
                .observe(network, self.layer.app.snapshot.hp);
        }
        self.report_state();
        Ok(())
    }
    pub fn observe_completed(&mut self, id: u64) {
        match self.presentation.completed(id) {
            Some(client::presentation_gate::Event::PrepareGameUi) => {
                if self.diagnostics {
                    println!("NATIVE_INITIAL_PRESENT stage=world_gpu_complete frame={id}");
                }
                client::presentation_gate::prepare_game_ui(&mut self.layer.app);
            }
            Some(client::presentation_gate::Event::AdmitInput) => {
                if self.diagnostics {
                    println!("NATIVE_INITIAL_PRESENT stage=game_ui_gpu_complete frame={id}");
                }
                self.ready_world = true;
                self.report_state();
                self.painted();
            }
            None => {}
        }
    }
    pub fn submitted(&mut self, id: u64, world: bool, game_ui: bool) {
        if self.diagnostics && !self.ready_world && (world || game_ui) {
            println!(
                "NATIVE_INITIAL_PRESENT stage={} frame={id}",
                if game_ui {
                    "cached_game_ui_submitted"
                } else {
                    "world_submitted"
                }
            );
        }
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
            let Some(intent) = self.intent_sender.take(&mut self.layer.app) else {
                break;
            };
            match intent {
                Intent::Connect { address, name } => {
                    self.save_preferences();
                    self.connecting();
                    self.command(Command::Connect { address, name });
                }
                Intent::Disconnect => {
                    self.intent_sender.clear();
                    self.command(Command::Disconnect);
                }
                Intent::Quit => self.command(Command::Quit),
                Intent::BugReport => self.command(Command::BugReport),
                Intent::PointerCapture(capture) => self.command(Command::Capture(capture)),
                Intent::SettingsChanged => {
                    self.quality_now_ms = now;
                    self.quality.update_saved(&self.layer.app.settings, now);
                    self.apply_sound();
                    self.save_preferences();
                    if let Some(view) = view.as_deref_mut() {
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
                            self.command(Command::Connect {
                                address: self.layer.app.address.clone(),
                                name: self.layer.app.name.clone(),
                            });
                            self.connecting();
                        }
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
                    self.quality_now_ms = now;
                    self.quality.update_saved(&self.layer.app.settings, now);
                    self.save_preferences();
                    if let Some(view) = view.as_deref_mut() {
                        let receipt = view.apply_settings(self.quality.target());
                        self.layer.app.set_pending_settings(
                            receipt.pending_render & !(1 << ui::settings::Setting::Hold60 as u32),
                        );
                    }
                }
                other => {
                    if let Some(network) = network.as_deref_mut() {
                        if self.diagnostics && !self.intent_waiting {
                            let (kind, bytes) = match &other {
                                Intent::Chat(text) => (
                                    if text.starts_with('/') {
                                        "command"
                                    } else {
                                        "chat"
                                    },
                                    text.len(),
                                ),
                                _ => ("window_or_slot", 0),
                            };
                            println!("NATIVE_UI_INTENT class={kind} bytes={bytes}");
                        }
                        match self.intent_sender.send(network, other, now) {
                            Ok(client::ui_bridge::Dispatch::Sent(id)) => {
                                self.intent_waiting = false;
                                if self.diagnostics {
                                    println!("NATIVE_UI_INTENT_SENT id={}", id.0);
                                }
                                self.block_sounds.observe(network);
                                if self.requests.len() == 128 {
                                    self.requests.pop_front();
                                }
                                self.requests.push_back(id);
                            }
                            Ok(client::ui_bridge::Dispatch::Unsupported(_)) => {
                                self.layer.app.set_status("Действие недоступно в этом окне")
                            }
                            Err(client::network::NetworkError::Capacity) => {
                                if self.diagnostics && !self.intent_waiting {
                                    println!("NATIVE_UI_INTENT_WAIT cause=capacity");
                                }
                                self.intent_waiting = true;
                                self.layer.app.set_status("Действие ожидает отправки");
                                break;
                            }
                            Err(error) => {
                                self.intent_waiting = false;
                                if self.diagnostics {
                                    println!("NATIVE_UI_INTENT_ERROR cause={error:?}");
                                }
                                self.layer.app.set_status("Не удалось отправить действие");
                            }
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
                self.machine_sounds.clear(|_| {});
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
    view.replace_material_albedo(prepared.atlas())?;
    view.invalidate();
    Ok(())
}
