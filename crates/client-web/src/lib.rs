//! Thin browser surface/input/worker transport. World and camera are shared Rust.
#![cfg(target_arch = "wasm32")]
mod generation_worker;
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
        Ok(())
    }
    fn draw(&mut self) -> Result<bool, JsValue> {
        self.observe_presentations();
        let completed_at_edit = u64::from(self.completed_submission.load(Ordering::Acquire));
        self.resize()?;
        let now = web_sys::window()
            .and_then(|w| w.performance())
            .map_or(self.last_ms, |clock| clock.now());
        let elapsed = (now - self.last_ms).max(0.0) / 1000.0;
        self.last_ms = now;
        self.view.advance_input(elapsed).map_err(js_error)?;
        self.presentation_probe
            .arm(self.view.edit_revision(), completed_at_edit);
        self.pump()?;
        self.view
            .upload_ready(&self.gpu.queue, &mut UploadBudget::new(512 * 1024))
            .map_err(js_error)?;
        // Browser canvas presentation does not bound pending GPU submissions on
        // every implementation. Keep at most two owned frames in flight.
        if self
            .first_ready_submission
            .is_some_and(|id| self.completed_submission.load(Ordering::Acquire) >= id)
            && self.view.is_ready()
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
        if !self
            .view
            .needs_redraw(self.config.width, self.config.height)
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
        self.view
            .encode(
                &self.gpu.queue,
                &mut encoder,
                &output,
                self.config.width,
                self.config.height,
            )
            .map_err(js_error)?;
        self.gpu.queue.submit([encoder.finish()]);
        frame.present();
        self.submitted = self
            .submitted
            .checked_add(1)
            .ok_or("frame sequence exhausted")?;
        let submitted = self.submitted;
        self.in_flight.fetch_add(1, Ordering::AcqRel);
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
        if self.view.is_ready() && self.first_ready_submission.is_none() {
            self.first_ready_submission = Some(submitted);
        }
        Ok(true)
    }
    fn keyboard(&mut self, code: &str, pressed: bool) {
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
fn stats_object(view: &ClientView) -> JsValue {
    let stats = view.stats();
    let value = js_sys::Object::new();
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
            if key(&event.code()).is_some() {
                event.prevent_default();
                if let Some(state) = weak.upgrade() {
                    state.borrow_mut().keyboard(&event.code(), pressed);
                }
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
        if let Some(state) = weak.upgrade()
            && web_sys::window()
                .and_then(|w| w.document())
                .is_some_and(|d| d.pointer_lock_element().is_some())
        {
            state
                .borrow_mut()
                .view
                .mouse_delta(f64::from(event.movement_x()), f64::from(event.movement_y()));
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
async fn start_forest() -> Result<JsValue, JsValue> {
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
    let mut view = ClientView::new(terrain, client::SceneConfig::forest()).map_err(js_error)?;
    let params = web_sys::UrlSearchParams::new_with_str(&window.location().search()?)?;
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
        start_forest().await
    }
}
