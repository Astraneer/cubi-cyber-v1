//! Thin browser surface/input/worker transport. World and camera are shared Rust.
#![cfg(target_arch = "wasm32")]
mod browser_horizon;
mod browser_network;
mod generation_worker;
mod product_ui;
mod sky;
use client::view::{ClientView, InputKey};
use jobs::{BrowserEvent, BrowserOutcome, BrowserWorkerPool, CancellationToken, JobId};
use render::terrain_gpu::{AtlasImage, TerrainGpu, UploadBudget};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, HtmlCanvasElement, KeyboardEvent, MouseEvent};
type AnimationCallback = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;
thread_local! {static ACTIVE_VIEW:RefCell<Option<Weak<RefCell<BrowserState>>>>=const {RefCell::new(None)};}
struct BrowserState {
    canvas: HtmlCanvasElement,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: render::Gpu,
    view: ClientView,
    workers: BrowserWorkerPool,
    running: BTreeMap<JobId, CancellationToken>,
    horizon_workers: Option<browser_horizon::BrowserHorizon>,
    last_ms: f64,
    resolve: Option<js_sys::Function>,
    reject: Option<js_sys::Function>,
    listeners: Vec<Closure<dyn FnMut(Event)>>,
    worker_restarts: u8,
    in_flight: Arc<AtomicU32>,
    completed_submission: Arc<AtomicU32>,
    submitted: u32,
    first_ready_submission: Option<u32>,
    modifiers: [bool; 4],
    presentation_probe: client::presentation::PresentationProbe,
    presentation_completion: Arc<Mutex<client::presentation::FrameCompletion>>,
    product: Option<product_ui::ProductUi>,
    socket: Option<browser_network::BrowserSocket>,
    resetting: bool,
    recorder: Option<telemetry::flight::FlightRecorder>,
    gpu_capture: Option<client::capture::FrameGpuCapture>,
    last_present_ms: f64,
    last_profiler_ms: f64,
    ui_pending: bool,
    connection_epoch: u64,
    local_materials_revision: u64,
    local_materials_resolution: u32,
    local_materials_layers: usize,
    local_materials_upload_bytes: u64,
}
impl BrowserState {
    fn observe_presentations(&mut self) {
        let completion = *self
            .presentation_completion
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.presentation_probe.observe(completion);
    }
    fn stats(&self) -> JsValue {
        let value = stats_object(&self.view);
        let light_pages = self.socket.as_ref().map_or(0, |socket| {
            socket.session().connection().light_replica().len()
        });
        let _ = js_sys::Reflect::set(
            &value,
            &"authoritativeLightPages".into(),
            &(light_pages as f64).into(),
        );
        if let Some(product) = &self.product {
            for (name, item) in [
                ("uiScreen", format!("{:?}", product.layer.app.screen)),
                ("uiStatus", product.layer.app.status().to_owned()),
                (
                    "scene",
                    if self.socket.is_some() {
                        "remote".into()
                    } else {
                        "welcome".into()
                    },
                ),
            ] {
                let _ = js_sys::Reflect::set(&value, &name.into(), &item.into());
            }
            let _ = js_sys::Reflect::set(
                &value,
                &"connected".into(),
                &product.layer.app.snapshot.connected.into(),
            );
            let _ = js_sys::Reflect::set(
                &value,
                &"uiNeedsUpload".into(),
                &product
                    .layer
                    .needs_redraw(self.config.width, self.config.height)
                    .into(),
            );
            let _ = js_sys::Reflect::set(&value, &"uiFramePending".into(), &self.ui_pending.into());
            for (name, revision) in [
                ("uiRevision", product.layer.app.revision()),
                (
                    "uiUploadedRevision",
                    product.layer.uploaded_revision().unwrap_or(0),
                ),
            ] {
                let _ = js_sys::Reflect::set(&value, &name.into(), &(revision as f64).into());
            }
            let _ = js_sys::Reflect::set(
                &value,
                &"interactionMode".into(),
                &"authoritative-remote".into(),
            );
        }
        for (name, number) in [
            ("submittedFrames", self.submitted),
            (
                "completedFrames",
                self.completed_submission.load(Ordering::Acquire),
            ),
            ("inFlightFrames", self.in_flight.load(Ordering::Acquire)),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &number.into());
        }
        for (name, number) in [
            ("renderedUploadBytes", self.view.rendered_upload_bytes()),
            ("worldVersion", self.view.scene.world().version().0),
            ("renderedWorldVersion", self.view.rendered_world_version().0),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &(number as f64).into());
        }
        for (name, number) in [
            ("localMaterialsRevision", self.local_materials_revision),
            (
                "localMaterialsResolution",
                u64::from(self.local_materials_resolution),
            ),
            ("localMaterialsLayers", self.local_materials_layers as u64),
            (
                "localMaterialsUploadBytes",
                self.local_materials_upload_bytes,
            ),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &(number as f64).into());
        }
        let presentation = self.presentation_probe.stats();
        for (name, number) in [
            ("presentedEditCount", presentation.measured_edits),
            ("pendingEditPresentations", presentation.pending_edits),
            ("editPresentedFramesMax", presentation.max_frames),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &(number as f64).into());
        }
        for (name, flag) in [
            (
                "presentationLatencyMeasured",
                presentation.measured_edits > 0 && !presentation.overflow,
            ),
            ("presentationProbeOverflow", presentation.overflow),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &flag.into());
        }
        for (name, signature) in [
            (
                "currentCamera",
                self.view
                    .current_camera_signature(self.config.width, self.config.height),
            ),
            ("renderedCamera", self.view.rendered_camera_signature()),
        ] {
            let _ = js_sys::Reflect::set(&value, &name.into(), &format!("{signature:016x}").into());
        }
        value
    }
    fn resize(&mut self) -> Result<(), JsValue> {
        let window = web_sys::window().ok_or("window missing")?;
        let ratio = window.device_pixel_ratio();
        let width = (f64::from(self.canvas.client_width().max(1)) * ratio).round() as u32;
        let height = (f64::from(self.canvas.client_height().max(1)) * ratio).round() as u32;
        if width != self.config.width || height != self.config.height {
            self.canvas.set_width(width);
            self.canvas.set_height(height);
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.gpu.device, &self.config);
            self.view
                .resize(&self.gpu.device, width, height)
                .map_err(js_error)?;
        }
        Ok(())
    }
    fn pump(&mut self) -> Result<(), JsValue> {
        let mut failed = false;
        for _ in 0..8 {
            let Some(event) = self.workers.try_recv() else {
                break;
            };
            match event {
                BrowserEvent::Completion { id, outcome } => {
                    self.running.remove(&id);
                    match outcome {
                        BrowserOutcome::Bytes(bytes) => self.view.scene.complete_bytes(id, &bytes),
                        BrowserOutcome::Cancelled => self.view.scene.cancelled(id),
                        BrowserOutcome::Failed(error) => {
                            failed = true;
                            self.view.scene.fail(id, error)
                        }
                    }
                    .map_err(|error| js_error(error.to_string()))?;
                }
                BrowserEvent::WorkerFailed { worker, message } => {
                    web_sys::console::warn_1(&format!("scene worker {worker}: {message}").into());
                    failed = true;
                }
            }
        }
        if failed {
            self.worker_restarts = self.worker_restarts.saturating_add(1);
            if self.worker_restarts > 3 {
                return Err(js_error("scene worker repeatedly failed"));
            }
            self.workers.restart_failed()?;
        }
        while let Some(id) = self.view.scene.take_cancellation() {
            self.workers.cancel(id)?;
        }
        for (&id, token) in &self.running {
            if token.is_cancelled() {
                self.workers.cancel(id)?;
            }
        }
        let tick = self.view.tick();
        self.view
            .scene
            .phase(tick, 8)
            .map_err(|error| js_error(error.to_string()))?;
        let mut budget = client::ClientScene::default_budget();
        for _ in 0..4 {
            if self.workers.available_workers() == 0 {
                break;
            }
            let Some(dispatch) = self.view.scene.take_task(tick, &mut budget) else {
                break;
            };
            let bytes = dispatch.payload.encode();
            match self
                .workers
                .try_submit(dispatch.id, &bytes, dispatch.spec.retained_bytes)
            {
                Ok(()) => {
                    self.running.insert(dispatch.id, dispatch.cancellation);
                }
                Err(_) => {
                    self.view.scene.requeue(dispatch);
                    break;
                }
            }
        }
        if let Some(error) = self.view.scene.error() {
            return Err(js_error(error));
        }
        if let Some(horizon) = &mut self.horizon_workers {
            horizon.pump(&mut self.view)?;
        }
        Ok(())
    }
    fn draw(&mut self) -> Result<bool, JsValue> {
        let cpu_start = clock_ms();
        if let (Some(capture), Some(recorder)) = (&mut self.gpu_capture, &mut self.recorder) {
            capture.collect(recorder);
        }
        self.observe_presentations();
        let completed_at_edit = u64::from(self.completed_submission.load(Ordering::Acquire));
        self.resize()?;
        let now = web_sys::window()
            .and_then(|w| w.performance())
            .map_or(self.last_ms, |clock| clock.now());
        let elapsed = (now - self.last_ms).max(0.0) / 1000.0;
        self.last_ms = now;
        if self.product.is_none()
            || self
                .socket
                .as_ref()
                .is_some_and(|socket| socket.session().connection().welcome().is_some())
        {
            self.view
                .set_movement_replay_time((now.max(0.0) * 1_000_000.0).min(u64::MAX as f64) as u64)
                .map_err(js_error)?;
            self.view.advance_input(elapsed).map_err(js_error)?;
        }
        if !self.resetting
            && let Some(socket) = &mut self.socket
        {
            if let Err(error) = socket.pump(&mut self.view, now as u64) {
                self.socket = None;
                self.view.clear_keys();
                if let Some(product) = &mut self.product {
                    product.disconnected(&error);
                }
            } else if let Some(product) = &mut self.product {
                product.sync(socket.session().connection(), &self.view)?;
            }
        }
        let simulation_end = clock_ms();
        self.presentation_probe
            .arm(self.view.edit_revision(), completed_at_edit);
        if !self.resetting {
            self.pump()?;
        }
        let jobs_end = clock_ms();
        let can_upload = self.in_flight.load(Ordering::Acquire) < 1;
        if can_upload {
            self.view
                .upload_ready(&self.gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
                .map_err(js_error)?;
        }
        let mut ui_changed = false;
        if let Some(product) = &mut self.product {
            product.poll_imports();
            product.drain(
                &mut self.view,
                self.socket.as_mut(),
                &self.canvas,
                false,
                now as u64,
            )?;
            if product.layer.app.show_debug && now - self.last_profiler_ms >= 100.0 {
                if let Some(recorder) = &self.recorder {
                    product
                        .layer
                        .app
                        .set_profiler(ui::profiler::ProfilerSnapshot::from_recorder(recorder));
                }
                self.last_profiler_ms = now;
            }
            ui_changed = can_upload
                && product
                    .layer
                    .upload(&self.gpu.queue, self.config.width, self.config.height)
                    .map_err(js_error)?;
        }
        self.ui_pending |= ui_changed;
        let upload_end = clock_ms();
        // Browser canvas presentation does not bound pending GPU submissions on
        // every implementation. Keep at most two owned frames in flight.
        if self
            .first_ready_submission
            .is_some_and(|id| self.completed_submission.load(Ordering::Acquire) >= id)
            && (self.product.is_some() || self.view.is_ready())
            && let Some(resolve) = self.resolve.take()
        {
            let _ = resolve.call1(&JsValue::UNDEFINED, &self.stats());
            self.reject = None;
        }
        // One pending terrain frame bounds an edit to at most the old frame
        // followed by its new visible frame; input/jobs still pump every rAF.
        if self.in_flight.load(Ordering::Acquire) >= 1 {
            return Ok(false);
        }
        let draw_world = self
            .product
            .as_ref()
            .is_none_or(|product| product.layer.app.snapshot.connected);
        if !(self.ui_pending
            || draw_world
                && self
                    .view
                    .needs_redraw(self.config.width, self.config.height))
            && self.first_ready_submission.is_some()
        {
            return Ok(false);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.gpu.device, &self.config);
                self.view.invalidate();
                return Ok(false);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(false),
            Err(error) => return Err(js_error(format!("WebGPU frame failed:{error}"))),
        };
        let output = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shared forest frame"),
            });
        let ticket = if draw_world {
            self.view
                .encode(
                    &self.gpu.queue,
                    &mut encoder,
                    &output,
                    self.config.width,
                    self.config.height,
                )
                .map_err(js_error)?;
            self.gpu_capture.as_mut().and_then(|capture| {
                self.view
                    .encode_gpu_capture(capture, &mut encoder, u64::from(self.submitted) + 1)
            })
        } else {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("product menu background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.03,
                            b: 0.04,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            None
        };
        if let Some(product) = &mut self.product {
            product.layer.encode(&mut encoder, &output);
        }
        let encode_end = clock_ms();
        self.gpu.queue.submit([encoder.finish()]);
        self.ui_pending = false;
        if let (Some(capture), Some(ticket)) = (&mut self.gpu_capture, ticket) {
            capture.submitted(ticket);
        }
        frame.present();
        self.submitted = self
            .submitted
            .checked_add(1)
            .ok_or("frame sequence exhausted")?;
        let submitted = self.submitted;
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        if let Some(recorder) = &mut self.recorder {
            let finished = clock_ms();
            let work = self.view.take_frame_work();
            let stats = self.view.stats();
            let ns = |ms: f64| (ms.max(0.0) * 1_000_000.0) as u64;
            let _ = recorder.record_frame(telemetry::flight::Sample {
                frame: u64::from(self.submitted),
                monotonic_ns: ns(finished),
                interval_ns: ns(finished - self.last_present_ms),
                cpu_ns: Some(ns(finished - cpu_start)),
                cpu_stages_ns: [
                    None,
                    Some(ns(simulation_end - cpu_start)),
                    Some(ns(jobs_end - simulation_end)),
                    None,
                    Some(ns(upload_end - jobs_end)),
                    Some(ns(encode_end - upload_end)),
                    Some(ns(finished - encode_end)),
                    None,
                ],
                work,
                queued_jobs: stats.queued_jobs as u32,
                in_flight: self.in_flight.load(Ordering::Acquire) as u8,
                ..Default::default()
            });
            self.last_present_ms = finished;
        }
        let in_flight = self.in_flight.clone();
        let completed = self.completed_submission.clone();
        let presentation = self.presentation_completion.clone();
        let revision = self.view.rendered_edit_revision();
        self.gpu.queue.on_submitted_work_done(move || {
            let mut progress = presentation
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if revision > progress.edit_revision {
                *progress = client::presentation::FrameCompletion {
                    frame: u64::from(submitted),
                    edit_revision: revision,
                };
            }
            drop(progress);
            completed.fetch_max(submitted, Ordering::Release);
            in_flight.fetch_sub(1, Ordering::AcqRel);
        });
        if (self.product.is_some() || self.view.is_ready()) && self.first_ready_submission.is_none()
        {
            self.first_ready_submission = Some(submitted);
        }
        Ok(true)
    }
    fn keyboard(&mut self, code: &str, key_text: &str, pressed: bool) {
        let modifier = match code {
            "ControlLeft" => Some(0),
            "ControlRight" => Some(1),
            "ShiftLeft" => Some(2),
            "ShiftRight" => Some(3),
            _ => None,
        };
        if let Some(index) = modifier {
            self.modifiers[index] = pressed;
        }
        if let Some(product) = &mut self.product {
            if pressed {
                product.gesture();
            }
            let modifiers = ui::app::Modifiers {
                shift: self.modifiers[2] || self.modifiers[3],
                ctrl: self.modifiers[0] || self.modifiers[1],
            };
            product.keyboard(&mut self.view, code, key_text, pressed, modifiers);
            let _ = product.drain(
                &mut self.view,
                self.socket.as_mut(),
                &self.canvas,
                true,
                clock_ms() as u64,
            );
            return;
        }
        if let Some(key) = key(code) {
            let down = match key {
                InputKey::Sprint => self.modifiers[0] || self.modifiers[1],
                InputKey::Sneak => self.modifiers[2] || self.modifiers[3],
                _ => pressed,
            };
            self.view.set_key(key, down);
        }
    }
    fn fail(&mut self, error: JsValue) {
        web_sys::console::error_1(&error);
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document
                .document_element()
                .map(|e| e.set_attribute("data-m2", "error"));
            if let Some(status) = document.get_element_by_id("status") {
                status
                    .set_text_content(Some("Не удалось подготовить мир. Перезагрузите страницу."));
            }
        }
        if let Some(reject) = self.reject.take() {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
        self.resolve = None;
    }
}
fn js_error(error: impl ToString) -> JsValue {
    JsValue::from_str(&error.to_string())
}
fn clock_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now())
}
fn process_product_commands(state: &Rc<RefCell<BrowserState>>) {
    let command = state
        .borrow_mut()
        .product
        .as_mut()
        .and_then(|product| product.take_command());
    let Some(command) = command else {
        return;
    };
    match command {
        product_ui::Command::Connect { address, name } => {
            let (fresh, epoch) = {
                let mut state = state.borrow_mut();
                state.connection_epoch = state.connection_epoch.wrapping_add(1);
                state.socket = None;
                let fresh = state.view.authoritative_remote()
                    && state.view.scene.world().version().0 == 0
                    && state.running.is_empty();
                state.resetting = !fresh;
                (fresh, state.connection_epoch)
            };
            if fresh {
                connect_socket(state, address, name);
                return;
            }
            let weak = Rc::downgrade(state);
            wasm_bindgen_futures::spawn_local(async move {
                let result = async {
                    animation_frame().await?;
                    animation_frame().await?;
                    let state = weak.upgrade().ok_or("view released")?;
                    let (device, queue, format, width, height) = {
                        let state = state.borrow();
                        (
                            state.gpu.device.clone(),
                            state.gpu.queue.clone(),
                            state.config.format,
                            state.config.width,
                            state.config.height,
                        )
                    };
                    let atlas = AtlasImage::donor().map_err(js_error)?;
                    let renderer = TerrainGpu::new(
                        &device,
                        &queue,
                        format,
                        width,
                        height,
                        render::OutputTransfer::ShaderSrgb,
                        client::view::terrain_gpu_config(),
                        &atlas,
                    )
                    .map_err(js_error)?;
                    let mut view =
                        ClientView::new(renderer, client::remote::RemoteSession::scene_config())
                            .map_err(js_error)?;
                    animation_frame().await?;
                    prepare_product_world(&mut view, &device, &queue, width, height, &atlas)?;
                    prepare_material_variant(&mut view, &device, &queue, &atlas)?;
                    let materials = audio::browser::with_local_resources(|local| {
                        client::local_materials::prepare(local, Default::default())
                    });
                    let material_revision =
                        audio::browser::with_local_resources(|local| local.revision());
                    let href = web_sys::window().ok_or("window")?.location().href()?;
                    let worker = web_sys::Url::new_with_base("workers/jobs.js", &href)?.href();
                    let module = web_sys::Url::new_with_base("pkg/client_web.js", &href)?.href();
                    let workers = BrowserWorkerPool::new(
                        &worker,
                        &module,
                        "execute_scene_task",
                        2,
                        32 * 1024 * 1024,
                    )?;
                    let horizon_workers = browser_horizon::BrowserHorizon::new(&worker, &module)?;
                    let mut borrowed = state.borrow_mut();
                    if borrowed.connection_epoch != epoch {
                        return Ok::<_, JsValue>(());
                    }
                    borrowed.view = view;
                    if let Some(recorder) = &mut borrowed.recorder {
                        recorder
                            .begin_input_epoch()
                            .map_err(|error| js_error(format!("new world recorder: {error:?}")))?;
                    }
                    borrowed.local_materials_revision = 0;
                    borrowed.local_materials_resolution = 0;
                    borrowed.local_materials_layers = 0;
                    borrowed.local_materials_upload_bytes = 0;
                    if let Ok(materials) = materials {
                        let _ = apply_local_materials(&mut borrowed, materials, material_revision);
                    }
                    borrowed.workers = workers;
                    borrowed.horizon_workers = Some(horizon_workers);
                    borrowed.running.clear();
                    borrowed.resetting = false;
                    drop(borrowed);
                    connect_socket(&state, address, name);
                    Ok(())
                }
                .await;
                if let Err(error) = result
                    && let Some(state) = weak.upgrade()
                {
                    let mut state = state.borrow_mut();
                    if state.connection_epoch == epoch {
                        state.resetting = false;
                        if let Some(product) = &mut state.product {
                            product.disconnected(
                                &error
                                    .as_string()
                                    .unwrap_or_else(|| "Не удалось подготовить новый мир".into()),
                            );
                        }
                    }
                }
            });
        }
        product_ui::Command::Disconnect | product_ui::Command::Quit => {
            let mut state = state.borrow_mut();
            state.connection_epoch = state.connection_epoch.wrapping_add(1);
            state.socket = None;
            state.resetting = false;
            state.view.clear_keys();
            if let Some(product) = &mut state.product {
                product.disconnected("");
            }
            if let Some(document) = web_sys::window().and_then(|w| w.document()) {
                document.exit_pointer_lock();
            }
        }
        product_ui::Command::RebuildResources => {
            let weak = Rc::downgrade(state);
            wasm_bindgen_futures::spawn_local(async move {
                let _ = animation_frame().await;
                let _ = animation_frame().await;
                let revision = audio::browser::with_local_resources(|local| local.revision());
                let prepared = audio::browser::prepare_current_sound_bank().await;
                let _ = animation_frame().await;
                let materials = audio::browser::with_local_resources(|local| {
                    client::local_materials::prepare(local, Default::default())
                });
                if audio::browser::with_local_resources(|local| local.revision()) != revision {
                    return;
                }
                if let Some(state) = weak.upgrade() {
                    let mut state = state.borrow_mut();
                    let colors_ok = match materials {
                        Ok(materials) => {
                            apply_local_materials(&mut state, materials, revision).is_ok()
                        }
                        Err(_) => false,
                    };
                    let Some(product) = &mut state.product else {
                        return;
                    };
                    let result = prepared.and_then(|prepared| product.audio.replace_bank(prepared));
                    product
                        .layer
                        .app
                        .set_status(if result.is_ok() && colors_ok {
                            "Локальные звуки и материалы набора применены"
                        } else if result.is_ok() {
                            "Звуки подготовлены; изображения отклонены, прежние цвета сохранены"
                        } else {
                            "Звуки набора отклонены; прежний звук сохранён"
                        });
                    let _ = product.apply_sound();
                }
            });
        }
        product_ui::Command::BugReport => {
            let state = state.borrow();
            if let Some(recorder) = &state.recorder {
                let seed = state
                    .socket
                    .as_ref()
                    .and_then(|socket| socket.session().connection().welcome())
                    .map_or(0, |welcome| u64::from(welcome.public_seed));
                let mut report = recorder.export_json("webgpu", seed, 0, 0);
                let movement = state.socket.as_ref().map_or_else(
                    || state.view.movement_replay(),
                    |socket| socket.session().connection().movement_replay(),
                );
                let movement_result = movement.append_json(&mut report);
                report.pop();
                use std::fmt::Write;
                let _ = write!(
                    report,
                    ",\"movementReplayAvailable\":{},\"initialPublicWorldHashKnown\":false,\"currentPublicWorldHash\":\"{:016x}\"",
                    movement_result.is_ok(),
                    state.view.scene.world().canonical_hash()
                );
                if let Err(reason) = movement_result {
                    let _ = write!(report, ",\"movementReplayUnavailableReason\":{reason:?}");
                }
                report.push('}');
                report.insert_str(1,"\"replayAvailable\":false,\"replayUnavailableReason\":\"initial public world snapshot not captured\",");
                let _ =
                    product_ui::download("cubi-flight-recorder.json", &report, "application/json");
            }
        }
    }
}
fn apply_local_materials(
    state: &mut BrowserState,
    materials: Option<render::material_pack::PreparedMaterials>,
    revision: u64,
) -> Result<(), render::material_pack::MaterialError> {
    let layers = materials.as_ref().map_or(0, |value| value.imported_layers);
    let materials = if materials.is_none() && state.local_materials_layers > 0 {
        let base = AtlasImage::donor().map_err(|_| render::material_pack::MaterialError::Atlas)?;
        Some(render::material_pack::PreparedMaterials::prepare(
            &base,
            &[],
            revision,
            Default::default(),
        )?)
    } else {
        materials
    };
    let (resolution, uploaded) = if let Some(materials) = materials {
        let gpu = render::material_pack::MaterialPackGpu::upload(
            &state.gpu.device,
            &state.gpu.queue,
            &materials,
        )?;
        if gpu.reserved_metals {
            return Err(render::material_pack::MaterialError::UnsupportedMetal);
        }
        let actual_resolution = gpu.resolution;
        let geometry = state.view.renderer_mut().geometry();
        let (_, proxy_bytes) =
            render::material_pack::apply_albedo_in_place(&state.gpu.queue, geometry, &materials)?;
        let uploaded = gpu.upload_bytes + proxy_bytes;
        state
            .view
            .renderer_mut()
            .replace_material_pack(&state.gpu.device, &state.gpu.queue, gpu, layers > 0)
            .map_err(|_| render::material_pack::MaterialError::Target)?;
        (actual_resolution, uploaded)
    } else {
        (0, 0)
    };
    state.local_materials_revision = revision;
    state.local_materials_resolution = resolution;
    state.local_materials_layers = layers;
    state.local_materials_upload_bytes = uploaded;
    state.view.invalidate();
    Ok(())
}
fn prepare_product_world(
    view: &mut ClientView,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    atlas: &AtlasImage,
) -> Result<(), JsValue> {
    view.prepare_lighting(
        device,
        queue,
        client::lighting::ClientLightingConfig {
            source: client::lighting::LightSource::Authoritative,
            shadows: render::shadows::ShadowConfig {
                resolution: 2048,
                dynamic_casters: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .map_err(js_error)?;
    view.prepare_environment(
        device,
        queue,
        width,
        height,
        render::OutputTransfer::ShaderSrgb,
        client::view::EnvironmentProfile::default(),
        atlas,
    )
    .map_err(js_error)?;
    view.prepare_entities(device, queue).map_err(js_error)?;
    view.prepare_horizon(device, queue, client::view::horizon::Config::default())
        .map_err(js_error)?;
    Ok(())
}

fn prepare_material_variant(
    view: &mut ClientView,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &AtlasImage,
) -> Result<(), JsValue> {
    let prepared =
        render::material_pack::PreparedMaterials::prepare(atlas, &[], 0, Default::default())
            .map_err(|error| js_error(format!("material defaults: {error:?}")))?;
    let gpu = render::material_pack::MaterialPackGpu::upload(device, queue, &prepared)
        .map_err(|error| js_error(format!("material defaults GPU: {error:?}")))?;
    view.renderer_mut()
        .prepare_material_pack(device, queue, gpu, false)
        .map_err(js_error)
}
fn connect_socket(state: &Rc<RefCell<BrowserState>>, address: String, name: String) {
    let mut state = state.borrow_mut();
    match browser_network::BrowserSocket::connect(&address, &name, clock_ms() as u64) {
        Ok(socket) => {
            state.socket = Some(socket);
        }
        Err(error) => {
            if let Some(product) = &mut state.product {
                product.disconnected(
                    &error
                        .as_string()
                        .unwrap_or_else(|| "Не удалось подключиться к миру".into()),
                );
            }
        }
    }
}
fn stats_object(view: &ClientView) -> JsValue {
    let stats = view.stats();
    let value = js_sys::Object::new();
    let position = stats.player_pos;
    let eye_light = view.scene.read_authoritative_light(core_types::BlockPos {
        space: view.scene.config().center.space,
        x: position[0].floor() as i32,
        y: position[1].floor() as i32 + 2,
        z: position[2].floor() as i32,
    });
    for (name, number) in [
        (
            "horizonHeightFront",
            view.horizon_stats().map_or(0, |s| s.height_front) as f64,
        ),
        (
            "horizonProxyFront",
            view.horizon_stats().map_or(0, |s| s.proxy_front) as f64,
        ),
        (
            "pendingLightSourcePages",
            view.lighting_stats().map_or(0, |s| s.pending_source_pages) as f64,
        ),
        (
            "lightPendingGpuPages",
            view.lighting_stats().map_or(0, |s| s.pending_pages) as f64,
        ),
        (
            "lightPendingCells",
            view.lighting_stats().map_or(0, |s| s.pending_cells) as f64,
        ),
        (
            "lightPendingShadows",
            view.lighting_stats()
                .map_or(0, |s| s.pending_shadow_cascades) as f64,
        ),
        (
            "lightOldestTicks",
            view.lighting_stats().map_or(0, |s| s.oldest_age_ticks) as f64,
        ),
    ] {
        let _ = js_sys::Reflect::set(&value, &name.into(), &number.into());
    }
    let _ = js_sys::Reflect::set(
        &value,
        &"publicEyeSky".into(),
        &eye_light.map_or(JsValue::NULL, |s| s.sky.into()),
    );
    for (name, flag) in [
        (
            "authoritativeLighting",
            view.lighting_stats()
                .is_some_and(|s| s.authoritative_source),
        ),
        ("environmentPrepared", view.environment_stats().is_some()),
        ("horizonPrepared", view.horizon_stats().is_some()),
        ("publicEyeLightKnown", eye_light.is_some()),
        ("lightReady", view.lighting_stats().is_some_and(|s| s.ready)),
    ] {
        let _ = js_sys::Reflect::set(&value, &name.into(), &flag.into());
    }
    for (name, number) in [
        ("residentPages", stats.resident_pages as f64),
        ("residentQuads", stats.resident_quads as f64),
        ("terrainFrames", stats.terrain_frames as f64),
        ("loadedPages", stats.loaded_pages as f64),
        ("queuedJobs", stats.queued_jobs as f64),
        ("runningJobs", stats.running_jobs as f64),
        ("meshesPublished", stats.meshes_published as f64),
        ("uploadBytes", stats.upload_bytes_total as f64),
        ("simulationTicks", stats.simulation_ticks as f64),
        ("droppedTimeSeconds", stats.dropped_time_seconds),
        ("deniedDebugRequests", stats.denied_debug_requests as f64),
        ("blocksBroken", stats.blocks_broken as f64),
        ("blocksPlaced", stats.blocks_placed as f64),
        ("editsRolledBack", stats.edits_rolled_back as f64),
        ("editDenied", stats.edit_denied as f64),
        ("miningProgress", f64::from(stats.mining_progress)),
        ("targetCell", f64::from(stats.target_cell)),
        ("editShellPages", stats.edit_shell_pages as f64),
        ("editShellOwners", stats.edit_shell_owners as f64),
        ("editOverlayUploads", stats.edit_overlay_uploads as f64),
        ("baseRebases", stats.base_rebases as f64),
        ("overlayQuads", stats.overlay_quads as f64),
        ("editRevision", stats.edit_revision as f64),
        ("renderedEditRevision", stats.rendered_edit_revision as f64),
        ("editMaxVisibleFrames", stats.edit_max_visible_frames as f64),
        ("demoMeshDelayTicks", stats.demo_mesh_delay_ticks as f64),
        ("demoTool", f64::from(client::demo_interaction::DEMO_TOOL)),
        (
            "demoBlock",
            f64::from(client::demo_interaction::DEMO_BLOCK.id()),
        ),
        ("cameraMode", f64::from(stats.camera_mode)),
        (
            "startupPipelineCompilations",
            stats.pipeline_compilations as f64,
        ),
        (
            "framePipelineCompilations",
            stats.frame_pipeline_compilations as f64,
        ),
        ("sampleCount", f64::from(render::MSAA_SAMPLES)),
    ] {
        let _ = js_sys::Reflect::set(&value, &name.into(), &number.into());
    }
    for (name, flag) in [
        ("ok", true),
        ("ready", stats.ready),
        ("playerSpawned", stats.player_spawned),
        ("selfModelRendered", stats.self_model_rendered),
        ("fullMeshReady", stats.full_mesh_ready),
        ("flying", stats.flying),
        ("noclip", stats.noclip),
        ("onGround", stats.on_ground),
        ("diagnosticUnitMesh", view.demo_unit_mesh()),
        ("diagnosticGpuUnitMesh", view.demo_gpu_unit_mesh()),
    ] {
        let _ = js_sys::Reflect::set(&value, &name.into(), &flag.into());
    }
    let position = js_sys::Array::new();
    for coordinate in stats.player_pos {
        position.push(&coordinate.into());
    }
    let _ = js_sys::Reflect::set(&value, &"playerPosition".into(), &position);
    let _ = js_sys::Reflect::set(&value, &"scene".into(), &"forest".into());
    let _ = js_sys::Reflect::set(&value, &"interactionMode".into(), &"standalone-demo".into());
    for (name, coordinates) in [
        ("targetPosition", stats.target_position),
        (
            "lastEditPosition",
            stats.last_edit_position.map(|p| [p.x, p.y, p.z]),
        ),
    ] {
        let item = coordinates.map_or(JsValue::NULL, |coordinates| {
            let array = js_sys::Array::new();
            for coordinate in coordinates {
                array.push(&coordinate.into());
            }
            array.into()
        });
        let _ = js_sys::Reflect::set(&value, &name.into(), &item);
    }
    value.into()
}
#[wasm_bindgen]
pub fn scene_stats() -> JsValue {
    ACTIVE_VIEW.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .map_or(JsValue::UNDEFINED, |state| state.borrow().stats())
    })
}

/// Reconciliation diagnostic belongs only to the explicit standalone demo.
#[wasm_bindgen]
pub fn rollback_demo_edit() -> Result<bool, JsValue> {
    ACTIVE_VIEW.with(|active| {
        let state = active
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or("standalone demo unavailable")?;
        let mut state = state.borrow_mut();
        let completed = u64::from(state.completed_submission.load(Ordering::Acquire));
        let result = state.view.rollback_last_demo_edit().map_err(js_error)?;
        let revision = state.view.edit_revision();
        state.presentation_probe.arm(revision, completed);
        Ok(result)
    })
}

/// Starts a new completion-based measurement window after a visual oracle.
#[wasm_bindgen]
pub fn reset_edit_presentation_probe() -> Result<(), JsValue> {
    ACTIVE_VIEW.with(|active| {
        let state = active
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or("standalone demo unavailable")?;
        let mut state = state.borrow_mut();
        let revision = state.view.edit_revision();
        state.presentation_probe.reset(revision);
        Ok(())
    })
}

/// Visual-oracle barrier for the standalone demo; workers keep bounded results.
#[wasm_bindgen]
pub fn pause_demo_mesh_publication(paused: bool) -> Result<(), JsValue> {
    ACTIVE_VIEW.with(|active| {
        let state = active
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or("standalone demo unavailable")?;
        state
            .borrow_mut()
            .view
            .set_demo_mesh_publication_paused(paused);
        Ok(())
    })
}

/// Worker entry executes the same bounded public generator/mesher as native.
#[wasm_bindgen]
pub fn execute_scene_task(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    let task = client::SceneTask::decode(request).map_err(|error| js_error(error.to_string()))?;
    let cancelled = || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    };
    task.execute_with_cancellation(cancelled)
        .map(|output| output.encode())
        .map_err(js_error)
}
fn key(code: &str) -> Option<InputKey> {
    Some(match code {
        "KeyW" => InputKey::Forward,
        "KeyS" => InputKey::Back,
        "KeyA" => InputKey::Left,
        "KeyD" => InputKey::Right,
        "Space" => InputKey::Jump,
        "ControlLeft" | "ControlRight" => InputKey::Sprint,
        "ShiftLeft" | "ShiftRight" => InputKey::Sneak,
        "KeyF" => InputKey::Flight,
        "KeyH" => InputKey::Noclip,
        "KeyV" => InputKey::Camera,
        _ => return None,
    })
}
fn add_listener(
    state: &Rc<RefCell<BrowserState>>,
    target: &web_sys::EventTarget,
    name: &str,
    handler: impl FnMut(Event) + 'static,
) -> Result<(), JsValue> {
    let callback = Closure::wrap(Box::new(handler) as Box<dyn FnMut(Event)>);
    target.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())?;
    state.borrow_mut().listeners.push(callback);
    Ok(())
}
fn install_input(state: &Rc<RefCell<BrowserState>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("window unavailable")?;
    let document = window.document().ok_or("document unavailable")?;
    for (name, pressed) in [("keydown", true), ("keyup", false)] {
        let weak = Rc::downgrade(state);
        add_listener(state, window.as_ref(), name, move |event| {
            let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
                return;
            };
            if let Some(state) = weak.upgrade() {
                let product = state.borrow().product.is_some();
                if !product && key(&event.code()).is_none() {
                    return;
                }
                if pressed
                    && event.repeat()
                    && state
                        .borrow()
                        .product
                        .as_ref()
                        .is_some_and(|p| p.layer.app.focus == ui::app::Focus::None)
                {
                    event.prevent_default();
                    return;
                }
                event.prevent_default();
                state
                    .borrow_mut()
                    .keyboard(&event.code(), &event.key(), pressed);
                process_product_commands(&state);
            }
        })?;
    }
    let weak = Rc::downgrade(state);
    add_listener(state, window.as_ref(), "blur", move |_| {
        if let Some(state) = weak.upgrade() {
            let mut state = state.borrow_mut();
            state.modifiers.fill(false);
            state.view.clear_keys();
        }
    })?;
    let weak = Rc::downgrade(state);
    add_listener(state, document.as_ref(), "mousemove", move |event| {
        let Some(event) = event.dyn_ref::<MouseEvent>() else {
            return;
        };
        if let Some(state) = weak.upgrade() {
            let locked = web_sys::window()
                .and_then(|w| w.document())
                .is_some_and(|d| d.pointer_lock_element().is_some());
            let mut state = state.borrow_mut();
            if locked {
                let delta = state.product.as_ref().map_or(
                    Some([f64::from(event.movement_x()), f64::from(event.movement_y())]),
                    |p| {
                        client::ui_bridge::mouse_delta(
                            &p.layer.app.settings,
                            f64::from(event.movement_x()),
                            f64::from(event.movement_y()),
                        )
                    },
                );
                if let Some([dx, dy]) = delta {
                    state.view.mouse_delta(dx, dy);
                }
            } else {
                let x = event.client_x() as f32 * state.config.width as f32
                    / state.canvas.client_width().max(1) as f32;
                let y = event.client_y() as f32 * state.config.height as f32
                    / state.canvas.client_height().max(1) as f32;
                if let Some(product) = &mut state.product {
                    product.layer.app.pointer(x, y);
                }
            }
        }
    })?;
    let weak = Rc::downgrade(state);
    let target: HtmlCanvasElement = state.borrow().canvas.clone();
    add_listener(state, target.as_ref(), "mousedown", move |event| {
        let Some(event) = event.dyn_ref::<MouseEvent>() else {
            return;
        };
        if event.button() != 0 && event.button() != 2 {
            return;
        }
        event.prevent_default();
        if let Some(state) = weak.upgrade() {
            let locked = web_sys::window()
                .and_then(|w| w.document())
                .is_some_and(|d| d.pointer_lock_element().is_some());
            {
                let mut borrowed = state.borrow_mut();
                let x = event.client_x() as f32 * borrowed.config.width as f32
                    / borrowed.canvas.client_width().max(1) as f32;
                let y = event.client_y() as f32 * borrowed.config.height as f32
                    / borrowed.canvas.client_height().max(1) as f32;
                let BrowserState {
                    product,
                    view,
                    socket,
                    canvas,
                    ..
                } = &mut *borrowed;
                if let Some(product) = product {
                    product.gesture();
                    if product.layer.app.captures_keyboard() {
                        product.layer.app.click(
                            x,
                            y,
                            event.button() == 2,
                            ui::app::Modifiers {
                                shift: event.shift_key(),
                                ctrl: event.ctrl_key(),
                            },
                        );
                        product.click_sound();
                        let _ =
                            product.drain(view, socket.as_mut(), canvas, true, clock_ms() as u64);
                        drop(borrowed);
                        process_product_commands(&state);
                        return;
                    }
                }
            }
            if !locked {
                state.borrow().canvas.request_pointer_lock();
                state.borrow_mut().view.activate_player();
            } else {
                state.borrow_mut().view.set_key(
                    if event.button() == 2 {
                        InputKey::Place
                    } else {
                        InputKey::Attack
                    },
                    true,
                );
            }
        }
    })?;
    let weak = Rc::downgrade(state);
    add_listener(state, document.as_ref(), "mouseup", move |event| {
        if let Some(event) = event.dyn_ref::<MouseEvent>()
            && (event.button() == 0 || event.button() == 2)
            && let Some(state) = weak.upgrade()
        {
            state.borrow_mut().view.set_key(
                if event.button() == 2 {
                    InputKey::Place
                } else {
                    InputKey::Attack
                },
                false,
            );
        }
    })?;
    let weak = Rc::downgrade(state);
    add_listener(state, target.as_ref(), "wheel", move |event| {
        if let Some(event) = event.dyn_ref::<web_sys::WheelEvent>()
            && let Some(state) = weak.upgrade()
        {
            let mut state = state.borrow_mut();
            if let Some(product) = &mut state.product {
                event.prevent_default();
                product.layer.app.wheel(event.delta_y().signum() as i32);
            }
        }
    })?;
    let weak = Rc::downgrade(state);
    add_listener(state, document.as_ref(), "pointerlockchange", move |_| {
        if let Some(state) = weak.upgrade()
            && web_sys::window()
                .and_then(|w| w.document())
                .is_some_and(|d| d.pointer_lock_element().is_none())
        {
            let mut state = state.borrow_mut();
            state.view.clear_keys();
            if let Some(product) = &mut state.product
                && product.layer.app.screen == ui::app::Screen::Game
                && product.layer.app.focus == ui::app::Focus::None
            {
                product.layer.app.open(ui::app::Screen::Pause);
            }
        }
    })?;
    add_listener(state, target.as_ref(), "contextmenu", |event| {
        event.prevent_default()
    })?;
    Ok(())
}
async fn animation_frame() -> Result<(), JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        if let Err(error) = web_sys::window()
            .expect("window")
            .request_animation_frame(&resolve)
        {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ())
}
fn animate(state: Rc<RefCell<BrowserState>>) -> Result<(), JsValue> {
    let callback: AnimationCallback = Rc::new(RefCell::new(None));
    let retained = callback.clone();
    *callback.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        let result = { state.borrow_mut().draw() };
        process_product_commands(&state);
        if let Err(error) = result {
            state.borrow_mut().fail(error);
            return;
        }
        if let Some(callback) = retained.borrow().as_ref() {
            let _ = web_sys::window()
                .expect("window")
                .request_animation_frame(callback.as_ref().unchecked_ref());
        }
    }) as Box<dyn FnMut()>));
    web_sys::window()
        .ok_or("window missing")?
        .request_animation_frame(
            callback
                .borrow()
                .as_ref()
                .expect("animation installed")
                .as_ref()
                .unchecked_ref(),
        )?;
    Ok(())
}
async fn start_forest(diagnostic: bool) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or("window missing")?;
    let document = window.document().ok_or("document missing")?;
    let canvas = document
        .get_element_by_id("game")
        .ok_or("canvas missing")?
        .dyn_into::<HtmlCanvasElement>()?;
    let instance = render::instance();
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|error| js_error(error.to_string()))?;
    let gpu = render::Gpu::request(&instance, Some(&surface), false)
        .await
        .map_err(js_error)?;
    let width =
        (f64::from(canvas.client_width().max(1)) * window.device_pixel_ratio()).round() as u32;
    let height =
        (f64::from(canvas.client_height().max(1)) * window.device_pixel_ratio()).round() as u32;
    canvas.set_width(width);
    canvas.set_height(height);
    let mut config = surface
        .get_default_config(&gpu.adapter, width, height)
        .ok_or("canvas format unavailable")?;
    config.desired_maximum_frame_latency = 1;
    config.present_mode = wgpu::PresentMode::Fifo;
    let formats = surface.get_capabilities(&gpu.adapter).formats;
    config.format = formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
            )
        })
        .ok_or("unorm canvas unavailable")?;
    surface.configure(&gpu.device, &config);
    // Yield the loading UI before synchronous browser pipeline preparation. Native
    // uses a real loading thread; the browser limitation remains explicit.
    animation_frame().await?;
    let atlas = AtlasImage::donor().map_err(js_error)?;
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        config.format,
        width,
        height,
        render::OutputTransfer::ShaderSrgb,
        client::view::terrain_gpu_config(),
        &atlas,
    )
    .map_err(|error| js_error(error.to_string()))?;
    let params = web_sys::UrlSearchParams::new_with_str(&window.location().search()?)?;
    let scene_config = if diagnostic {
        client::SceneConfig::forest()
    } else {
        client::remote::RemoteSession::scene_config()
    };
    let legacy = diagnostic
        && (params.get("unit_mesh").as_deref() == Some("1")
            || params.get("legacy_gpu_greedy").as_deref() == Some("1"));
    let mut view = if legacy {
        ClientView::new_with_rasterization(
            terrain,
            scene_config,
            client::view::NearRasterization::LegacyGreedy,
        )
    } else {
        ClientView::new(terrain, scene_config)
    }
    .map_err(js_error)?;
    let mut product = if diagnostic {
        None
    } else {
        Some(product_ui::ProductUi::prepare(
            &gpu.device,
            &gpu.queue,
            config.format,
        )?)
    };
    if let Some(product) = &mut product {
        animation_frame().await?;
        prepare_product_world(&mut view, &gpu.device, &gpu.queue, width, height, &atlas)?;
        prepare_material_variant(&mut view, &gpu.device, &gpu.queue, &atlas)?;
        let applied = view.apply_settings(&product.layer.app.settings);
        product
            .layer
            .app
            .set_pending_settings(applied.pending_render);
    }
    let recorder = if diagnostic {
        None
    } else {
        Some(telemetry::flight::FlightRecorder::new(4096, 2048).map_err(js_error)?)
    };
    let gpu_capture = if diagnostic {
        None
    } else {
        Some(client::capture::FrameGpuCapture::new(&gpu.device))
    };
    if diagnostic && params.get("unit_mesh").as_deref() == Some("1") {
        view.set_demo_unit_mesh(true).map_err(js_error)?;
    }
    if diagnostic && params.get("gpu_unit_mesh").as_deref() == Some("1") {
        view.prepare_gpu_unit_mesh().map_err(js_error)?;
    }
    let delay_ms = params
        .get("meshDelayMs")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(10_000);
    view.set_demo_mesh_delay_ticks(delay_ms.div_ceil(50));
    let worker_delay_ms = params
        .get("workerMeshDelayMs")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(10_000);
    let href = window.location().href()?;
    let worker_path = if worker_delay_ms == 0 {
        "workers/jobs.js".to_string()
    } else {
        format!("workers/scene.js?meshDelayMs={worker_delay_ms}")
    };
    let worker_url = web_sys::Url::new_with_base(&worker_path, &href)?.href();
    let module_url = web_sys::Url::new_with_base("pkg/client_web.js", &href)?.href();
    let workers = BrowserWorkerPool::new(
        &worker_url,
        &module_url,
        "execute_scene_task",
        2,
        32 * 1024 * 1024,
    )?;
    let horizon_workers = if diagnostic {
        None
    } else {
        Some(browser_horizon::BrowserHorizon::new(
            &worker_url,
            &module_url,
        )?)
    };
    let mut resolve_fn = None;
    let mut reject_fn = None;
    let ready = js_sys::Promise::new(&mut |resolve, reject| {
        resolve_fn = Some(resolve);
        reject_fn = Some(reject);
    });
    let state = Rc::new(RefCell::new(BrowserState {
        canvas,
        surface,
        config,
        gpu,
        view,
        workers,
        running: BTreeMap::new(),
        horizon_workers,
        last_ms: window.performance().map_or(0.0, |p| p.now()),
        resolve: resolve_fn,
        reject: reject_fn,
        listeners: Vec::new(),
        worker_restarts: 0,
        in_flight: Arc::new(AtomicU32::new(0)),
        completed_submission: Arc::new(AtomicU32::new(0)),
        submitted: 0,
        first_ready_submission: None,
        modifiers: [false; 4],
        product,
        socket: None,
        resetting: false,
        recorder,
        gpu_capture,
        last_present_ms: clock_ms(),
        last_profiler_ms: 0.0,
        local_materials_revision: 0,
        local_materials_resolution: 0,
        local_materials_layers: 0,
        local_materials_upload_bytes: 0,
        ui_pending: false,
        connection_epoch: 0,
        presentation_probe: client::presentation::PresentationProbe::new(0),
        presentation_completion: Arc::new(Mutex::new(
            client::presentation::FrameCompletion::default(),
        )),
    }));
    ACTIVE_VIEW.with(|active| *active.borrow_mut() = Some(Rc::downgrade(&state)));
    install_input(&state)?;
    animate(state)?;
    JsFuture::from(ready).await
}
#[wasm_bindgen]
pub async fn start() -> Result<JsValue, JsValue> {
    let search = web_sys::window()
        .ok_or("window unavailable")?
        .location()
        .search()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search)?;
    if params.get("scene").as_deref() == Some("sky") {
        sky::start_sky().await
    } else {
        start_forest(params.get("scene").as_deref() == Some("forest")).await
    }
}
