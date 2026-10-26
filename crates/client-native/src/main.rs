//! Thin native window/input shell; shared client owns world, budgets and camera.
mod flight;
mod loading;
mod product_ui;
mod remote_transport;
use client::view::{ClientView, InputKey};
use render::terrain_gpu::UploadBudget;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Window, WindowId},
};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SceneMode {
    Sky,
    Forest,
    #[default]
    Product,
}
#[derive(Clone)]
struct ConnectOptions {
    url: String,
    identity: std::path::PathBuf,
    name: String,
}
struct RemoteNative {
    session: client::remote::RemoteSession,
    socket: remote_transport::Socket,
    identity: std::path::PathBuf,
    name: String,
    persisted: bool,
}
impl RemoteNative {
    fn new(options: ConnectOptions) -> Result<Self, String> {
        let mut session = client::remote::RemoteSession::new(client::network::NetworkConfig {
            input_lead_ticks: 2,
            ..client::network::NetworkConfig::remote_scene()
        })?;
        let hello = remote_transport::load_identity(&options.identity)?.unwrap_or(net::Hello {
            identity: net::PlayerId::default(),
            name: options.name.clone(),
            credential: None,
        });
        session
            .connection_mut()
            .hello(hello.name.clone(), hello.identity, hello.credential)
            .map_err(|e| format!("identity handshake: {e:?}"))?;
        Ok(Self {
            socket: remote_transport::Socket::connect(options.url)?,
            session,
            identity: options.identity,
            name: hello.name,
            persisted: hello.credential.is_some(),
        })
    }
    fn pump_socket(&mut self, now: u64) -> Result<(), String> {
        self.socket.pump(&mut self.session, now)?;
        if !self.persisted
            && let Some(welcome) = self.session.connection().welcome()
            && let Some(credential) = self.session.connection().credential()
        {
            remote_transport::save_identity(
                &self.identity,
                &net::Hello {
                    identity: welcome.identity,
                    name: self.name.clone(),
                    credential: Some(credential),
                },
            )?;
            self.persisted = true;
            println!(
                "NATIVE_AUTHORITY_CONNECTED tick={} role={:?}",
                welcome.tick.0, welcome.role
            );
        }
        Ok(())
    }
}
struct NativeState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: render::Gpu,
    sky: render::SkyRenderer,
    mode: SceneMode,
    view: Option<Box<ClientView>>,
    loading: Option<mpsc::Receiver<Result<Box<ClientView>, String>>>,
    loading_thread: Option<std::thread::JoinHandle<()>>,
    workers: Option<jobs::NativePool<client::SceneTask, client::SceneOutput>>,
    // The history is 43 KiB. Keeping it inline duplicates that storage across
    // async return slots and window-event state moves on the 1 MiB Windows stack.
    recorder: Box<telemetry::FrameRecorder<600>>,
    frames: u64,
    last_frame: Instant,
    captured: bool,
    reported_ready: bool,
    modifiers: [bool; 4],
    demo_mesh_delay_ticks: u64,
    remote: Option<RemoteNative>,
    network_started: Instant,
    flight: flight::NativeFlight,
    in_flight: Arc<AtomicU32>,
    drawable: bool,
    product: Option<Box<product_ui::ProductUi>>,
    ui_loading: Option<mpsc::Receiver<Result<Box<product_ui::Prepared>, String>>>,
    ui_loading_thread: Option<std::thread::JoinHandle<()>>,
    ui_pending: bool,
    last_profiler: Instant,
    pointer: [f32; 2],
    raster: client::view::NearRasterization,
    quit_requested: bool,
    ime_allowed: bool,
}
impl NativeState {
    fn poll_product_loading(&mut self) -> Result<(), String> {
        let Some(receiver) = &self.ui_loading else {
            return Ok(());
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(_) => return Err("UI loading worker stopped".into()),
        };
        let mut product = product_ui::ProductUi::from_prepared(*result?);
        if let Some(remote) = &self.remote {
            product.layer.app.name = remote.name.clone();
            product.connecting();
        }
        if let Some(view) = self.view.as_deref_mut() {
            let applied = view.apply_settings(&product.layer.app.settings);
            product
                .layer
                .app
                .set_pending_settings(applied.pending_render);
            product.apply_local(&self.gpu.device, &self.gpu.queue, view)?;
        }
        self.product = Some(product);
        self.ui_loading = None;
        if let Some(thread) = self.ui_loading_thread.take() {
            thread.join().map_err(|_| "UI loading worker panicked")?;
        }
        Ok(())
    }
    fn drain_product(&mut self) -> Result<(), String> {
        let now = self
            .network_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(product) = &mut self.product {
            product.drain(
                self.view.as_deref_mut(),
                self.remote.as_mut().map(|r| r.session.connection_mut()),
                now,
            );
        }
        while let Some(command) = self.product.as_mut().and_then(|p| p.take_command()) {
            match command {
                product_ui::Command::Capture(capture) => self.capture(capture),
                product_ui::Command::Disconnect => {
                    self.remote = None;
                    self.capture(false);
                    if let Some(view) = self.view.as_deref_mut() {
                        view.clear_keys();
                    }
                    if let Some(product) = &mut self.product {
                        product.disconnected("");
                    }
                }
                product_ui::Command::Connect { address, name } => {
                    if let Err(error) = self.connect_product(address, name)
                        && let Some(product) = &mut self.product
                    {
                        product.disconnected(&error);
                    }
                }
                product_ui::Command::BugReport => self.export_flight()?,
                product_ui::Command::Quit => self.quit_requested = true,
            }
        }
        let ime = self.product.as_ref().is_some_and(|product| {
            matches!(
                product.layer.app.focus,
                ui::app::Focus::Name
                    | ui::app::Focus::Address
                    | ui::app::Focus::Search
                    | ui::app::Focus::Chat
            )
        });
        if ime != self.ime_allowed {
            self.window.set_ime_allowed(ime);
            self.ime_allowed = ime;
        }
        Ok(())
    }
    fn connect_product(&mut self, address: String, name: String) -> Result<(), String> {
        let address = if address.contains("://") {
            address
        } else {
            format!("ws://{address}")
        };
        if self.loading.is_some() {
            return Err("Дождитесь завершения загрузки мира".into());
        }
        self.capture(false);
        self.remote = None;
        self.view = None;
        self.workers = None;
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in address.bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        let remote = RemoteNative::new(ConnectOptions {
            url: address,
            identity: product_ui::data_path(&format!("identity-{hash:016x}.cubi")),
            name,
        })?;
        if let Some(product) = &mut self.product {
            product.layer.app.name = remote.name.clone();
            product.save_preferences();
        }
        let preparation = loading::Preparation {
            format: self.config.format,
            width: self.config.width,
            height: self.config.height,
            scene: client::remote::RemoteSession::scene_config(),
            raster: self.raster,
            entities: true,
            lighting: Some(loading::product_lighting()),
            materials: true,
            environment: true,
            horizon: true,
        };
        let device = self.gpu.device.clone();
        let queue = self.gpu.queue.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("cubi-remote-loading".into())
            .spawn(move || {
                let result = loading::prepare(&device, &queue, preparation);
                let _ = sender.send(result);
            })
            .map_err(|e| e.to_string())?;
        self.loading = Some(receiver);
        self.loading_thread = Some(thread);
        self.workers = Some(client::native_pool(2, 16).map_err(|e| format!("{e:?}"))?);
        self.remote = Some(remote);
        self.flight.begin_scene()?;
        self.reported_ready = false;
        Ok(())
    }
    async fn new(
        window: Arc<Window>,
        software: bool,
        mode: SceneMode,
        connection: Option<ConnectOptions>,
        benchmark: Option<flight::BenchmarkOptions>,
        raster: client::view::NearRasterization,
    ) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            match Self::new_backend(
                window.clone(),
                software,
                mode,
                wgpu::Backends::DX12,
                connection.clone(),
                benchmark.clone(),
                raster,
            )
            .await
            {
                Ok(state) => Ok(state),
                Err(dx12) => {
                    eprintln!("D3D12 initialization failed; trying Vulkan: {dx12}");
                    Self::new_backend(
                        window,
                        software,
                        mode,
                        wgpu::Backends::VULKAN,
                        connection,
                        benchmark,
                        raster,
                    )
                    .await
                    .map_err(|vulkan| format!("D3D12:{dx12}; Vulkan:{vulkan}"))
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        Self::new_backend(
            window,
            software,
            mode,
            wgpu::Backends::VULKAN,
            connection,
            benchmark,
            raster,
        )
        .await
    }
    async fn new_backend(
        window: Arc<Window>,
        software: bool,
        mode: SceneMode,
        backend: wgpu::Backends,
        connection: Option<ConnectOptions>,
        benchmark: Option<flight::BenchmarkOptions>,
        raster: client::view::NearRasterization,
    ) -> Result<Self, String> {
        let instance = render::instance_for(backend);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| error.to_string())?;
        let gpu = render::Gpu::request(&instance, Some(&surface), software).await?;
        let flight = flight::NativeFlight::new(&gpu.device, benchmark)?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface has no compatible format")?;
        config.desired_maximum_frame_latency = if mode != SceneMode::Sky {
            1
        } else {
            render::MAX_FRAMES_IN_FLIGHT
        };
        config.present_mode = wgpu::PresentMode::Fifo;
        if let Some(format) = surface
            .get_capabilities(&gpu.adapter)
            .formats
            .iter()
            .find(|format| format.is_srgb())
        {
            config.format = *format;
        }
        surface.configure(&gpu.device, &config);
        let mut sky =
            render::SkyRenderer::new(&gpu.device, config.format, config.width, config.height);
        let startup = sky.take_work();
        println!(
            "SKY_SETUP pipeline_compilations={}",
            startup.pipeline_compilations
        );
        let info = gpu.adapter.get_info();
        println!("GPU adapter={} backend={:?}", info.name, info.backend);
        let (loading, loading_thread, workers) =
            if mode == SceneMode::Forest || (mode == SceneMode::Product && connection.is_some()) {
                let (sender, receiver) = mpsc::sync_channel(1);
                let device = gpu.device.clone();
                let queue = gpu.queue.clone();
                let format = config.format;
                let (width, height) = (config.width, config.height);
                let preparation = loading::Preparation {
                    format,
                    width,
                    height,
                    scene: if connection.is_some() {
                        client::remote::RemoteSession::scene_config()
                    } else {
                        client::SceneConfig::forest()
                    },
                    raster,
                    entities: true,
                    lighting: (mode == SceneMode::Product).then(loading::product_lighting),
                    materials: mode == SceneMode::Product,
                    environment: mode == SceneMode::Product,
                    horizon: mode == SceneMode::Product,
                };
                // Actual separate loading thread: async fn alone would still block frames.
                let thread = std::thread::Builder::new()
                    .name("cubi-pipeline-loading".into())
                    .spawn(move || {
                        let result = loading::prepare(&device, &queue, preparation);
                        let _ = sender.send(result);
                    })
                    .map_err(|error| format!("loading worker failed:{error}"))?;
                let workers = client::native_pool(2, 16)
                    .map_err(|error| format!("world worker pool failed:{error:?}"))?;
                (Some(receiver), Some(thread), Some(workers))
            } else {
                (None, None, None)
            };
        let (ui_loading, ui_loading_thread) = if mode == SceneMode::Product {
            let (sender, receiver) = mpsc::sync_channel(1);
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            let format = config.format;
            let thread = std::thread::Builder::new()
                .name("cubi-ui-loading".into())
                .spawn(move || {
                    let result = product_ui::prepare(&device, &queue, format);
                    let _ = sender.send(result);
                })
                .map_err(|error| error.to_string())?;
            (Some(receiver), Some(thread))
        } else {
            (None, None)
        };
        Ok(Self {
            window,
            surface,
            config,
            gpu,
            sky,
            mode,
            view: None,
            loading,
            loading_thread,
            workers,
            recorder: Box::new(telemetry::FrameRecorder::new()),
            frames: 0,
            last_frame: Instant::now(),
            captured: false,
            reported_ready: false,
            modifiers: [false; 4],
            demo_mesh_delay_ticks: 0,
            remote: connection.map(RemoteNative::new).transpose()?,
            network_started: Instant::now(),
            flight,
            in_flight: Arc::new(AtomicU32::new(0)),
            drawable: size.width != 0 && size.height != 0,
            product: None,
            ui_loading,
            ui_loading_thread,
            ui_pending: false,
            last_profiler: Instant::now(),
            pointer: [0.0; 2],
            raster,
            quit_requested: false,
            ime_allowed: false,
        })
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.drawable = width != 0 && height != 0;
        if !self.drawable {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.sky.resize(&self.gpu.device, width, height);
        if let Some(view) = self.view.as_deref_mut() {
            view.resize(&self.gpu.device, width, height)?;
        }
        Ok(())
    }
    fn poll_loading(&mut self) -> Result<(), String> {
        let Some(receiver) = self.loading.as_ref() else {
            return Ok(());
        };
        match receiver.try_recv() {
            Ok(result) => {
                let mut view = result?;
                view.resize(&self.gpu.device, self.config.width, self.config.height)?;
                view.set_demo_mesh_delay_ticks(self.demo_mesh_delay_ticks);
                if let Some(product) = &mut self.product {
                    let applied = view.apply_settings(&product.layer.app.settings);
                    product
                        .layer
                        .app
                        .set_pending_settings(applied.pending_render);
                    product.apply_local(&self.gpu.device, &self.gpu.queue, &mut view)?;
                }
                self.view = Some(view);
                self.loading = None;
                if let Some(thread) = self.loading_thread.take() {
                    thread.join().map_err(|_| "pipeline worker panicked")?;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("pipeline loading worker disconnected".into());
            }
        }
        Ok(())
    }
    fn draw(&mut self, force_present: bool) -> Result<Option<bool>, String> {
        self.gpu
            .device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| error.to_string())?;
        self.flight.collect();
        self.poll_product_loading()?;
        self.poll_loading()?;
        if let Some(product) = &mut self.product {
            product.poll_import(&self.gpu.device, &self.gpu.queue, self.view.as_deref_mut());
        }
        self.drain_product()?;
        if let Some(view) = self.view.as_deref_mut() {
            self.flight.prepare_route(view)?;
        }
        if self.flight.route_finished() {
            if let Some(view) = self.view.as_deref() {
                let done = self.flight.finish_if_drained(
                    &self.gpu.adapter.get_info(),
                    view,
                    self.config.width,
                    self.config.height,
                    self.in_flight.load(Ordering::Acquire),
                )?;
                return Ok(done.then_some(true));
            }
            return Err("benchmark lost its world".into());
        }
        // A route step corresponds to one actual presentation. Pending GPU work
        // delays that step rather than silently skipping replay inputs.
        if self.flight.route_active() && self.in_flight.load(Ordering::Acquire) != 0 {
            return Ok(None);
        }
        let started = Instant::now();
        let scope = telemetry::AllocationScope::begin()
            .map_err(|error| format!("frame allocator scope: {error:?}"))?;
        let mut stages = [None; telemetry::flight::CPU_STAGES.len()];
        let elapsed = self.last_frame.elapsed().as_secs_f64();
        self.last_frame = Instant::now();
        let now = self
            .network_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(remote) = self.remote.as_mut()
            && let Err(error) = remote.pump_socket(now)
        {
            if self.product.is_none() {
                return Err(error);
            }
            self.remote = None;
            if let Some(product) = &mut self.product {
                product.disconnected(&error);
            }
        }
        if !self.drawable {
            return Ok(None);
        }
        if let Some(view) = self.view.as_deref_mut() {
            if let Some(command) = self.flight.route_input() {
                let input_started = Instant::now();
                command.apply(view);
                stages[0] = Some(flight::nanos(input_started.elapsed()));
            }
            if let Some(remote) = self.remote.as_mut() {
                remote.session.pump(view, now)?;
                remote.pump_socket(now)?;
            }
            if let (Some(product), Some(remote)) = (&mut self.product, &self.remote) {
                product.sync(remote.session.connection(), view)?;
            }
            let simulation_started = Instant::now();
            view.set_movement_replay_time(
                self.network_started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64,
            )?;
            view.advance_input(if self.flight.route_active() {
                client::benchmark::STEP_SECONDS
            } else {
                elapsed
            })?;
            stages[1] = Some(flight::nanos(simulation_started.elapsed()));
            let jobs_started = Instant::now();
            let mut budget = client::ClientScene::default_budget();
            let tick = view.tick();
            view.scene
                .pump_native(
                    self.workers.as_ref().expect("forest workers"),
                    tick,
                    &mut budget,
                    8,
                )
                .map_err(|error| error.to_string())?;
            stages[2] = Some(flight::nanos(jobs_started.elapsed()));
            if let Some(error) = view.scene.error() {
                return Err(error.to_string());
            }
            if self.in_flight.load(Ordering::Acquire)
                < if self.mode == SceneMode::Sky {
                    render::MAX_FRAMES_IN_FLIGHT
                } else {
                    1
                }
            {
                let upload_started = Instant::now();
                view.upload_ready(&self.gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))?;
                stages[4] = Some(flight::nanos(upload_started.elapsed()));
            }
        }
        let busy = self.in_flight.load(Ordering::Acquire)
            >= if self.mode != SceneMode::Sky {
                1
            } else {
                render::MAX_FRAMES_IN_FLIGHT
            };
        if let Some(product) = &mut self.product {
            if product.layer.app.show_debug
                && self.last_profiler.elapsed() >= Duration::from_millis(250)
            {
                product
                    .layer
                    .app
                    .set_profiler(ui::profiler::ProfilerSnapshot::from_recorder(
                        self.flight.recorder(),
                    ));
                self.last_profiler = Instant::now();
            }
            if !busy
                && product
                    .layer
                    .upload(&self.gpu.queue, self.config.width, self.config.height)
                    .map_err(str::to_owned)?
            {
                self.ui_pending = true;
                product.painted();
            }
        }
        let clean = !self.ui_pending
            && !force_present
            && !self.flight.route_active()
            && self
                .view
                .as_deref()
                .is_none_or(|view| !view.needs_redraw(self.config.width, self.config.height));
        if clean || busy {
            let work = self
                .view
                .as_deref_mut()
                .map_or(telemetry::WorkCounts::ZERO, ClientView::take_frame_work);
            let allocations = scope.finish();
            self.record_flight(started, stages, work, allocations, false, busy)?;
            return Ok(None);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                if self.flight.route_active() {
                    return Err("benchmark interrupted by surface reconfiguration".into());
                }
                self.resize(self.config.width, self.config.height)?;
                return Ok(None);
            }
            Err(wgpu::SurfaceError::Timeout) if !self.flight.route_active() => return Ok(None),
            Err(error) => return Err(format!("surface frame failed:{error}")),
        };
        let output = frame.texture.create_view(&Default::default());
        let encode_started = Instant::now();
        let mut work = if let Some(view) = self.view.as_deref_mut() {
            let mut encoder =
                self.gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("shared terrain frame"),
                    });
            view.encode(
                &self.gpu.queue,
                &mut encoder,
                &output,
                self.config.width,
                self.config.height,
            )?;
            if let Some(product) = &mut self.product {
                product.layer.encode(&mut encoder, &output);
            }
            let ticket = self.flight.encode(view, &mut encoder);
            self.gpu.queue.submit([encoder.finish()]);
            if let Some(ticket) = ticket {
                self.flight.submitted(ticket);
            }
            view.take_frame_work()
        } else {
            self.sky.render(&self.gpu.device, &self.gpu.queue, &output);
            if let Some(product) = &mut self.product {
                let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
                product.layer.encode(&mut encoder, &output);
                self.gpu.queue.submit([encoder.finish()]);
            }
            self.sky.take_work()
        };
        if let Some(product) = &mut self.product {
            let ui = product.layer.take_work();
            work.upload_bytes += ui.upload_bytes;
            work.pipeline_compilations += ui.pipeline_compilations;
            work.quads_emitted += ui.vertex_builds / 6;
        }
        self.ui_pending = false;
        stages[5] = Some(flight::nanos(encode_started.elapsed()));
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let pending = self.in_flight.clone();
        let ui_presentation = self
            .product
            .as_ref()
            .and_then(|product| product.presentation());
        self.gpu.queue.on_submitted_work_done(move || {
            pending.fetch_sub(1, Ordering::AcqRel);
            if let Some(report) = ui_presentation {
                report();
            }
        });
        let present_started = Instant::now();
        self.window.pre_present_notify();
        frame.present();
        stages[6] = Some(flight::nanos(present_started.elapsed()));
        self.frames += 1;
        self.recorder.push(telemetry::FrameRecord {
            frame: self.frames,
            cpu_ns: flight::nanos(started.elapsed()),
            gpu_ns: None,
            work,
        });
        let allocations = scope.finish();
        self.record_flight(started, stages, work, allocations, true, false)?;
        let ready = self.mode == SceneMode::Sky
            || if self.mode == SceneMode::Product && self.remote.is_none() {
                self.product.is_some()
            } else {
                self.view.as_deref().is_some_and(ClientView::is_ready)
            };
        if ready && !self.reported_ready {
            self.reported_ready = true;
            if self.view.is_some() {
                self.print_stats("NATIVE_TERRAIN_READY");
            } else {
                println!("NATIVE_UI_READY");
            }
        }
        Ok(Some(ready))
    }
    fn record_flight(
        &mut self,
        started: Instant,
        stages: [Option<u64>; 8],
        work: telemetry::WorkCounts,
        allocations: telemetry::AllocationCounts,
        presented: bool,
        waiting: bool,
    ) -> Result<(), String> {
        self.flight.record(
            telemetry::flight::Sample {
                cpu_ns: Some(flight::nanos(started.elapsed())),
                cpu_stages_ns: stages,
                work,
                allocations: Some(allocations),
                observations: if waiting {
                    flight::waiting()
                } else {
                    Default::default()
                },
                in_flight: self
                    .in_flight
                    .load(Ordering::Acquire)
                    .min(u32::from(u8::MAX)) as u8,
                ..Default::default()
            },
            self.view.as_deref(),
            presented,
        )
    }
    fn export_flight(&mut self) -> Result<(), String> {
        self.flight.collect();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        self.flight.export_with_movement(
            &std::path::PathBuf::from(format!(".local/artifacts/flight/native-{stamp}.json")),
            &self.gpu.adapter.get_info(),
            self.view.as_deref(),
            self.config.width,
            self.config.height,
            self.remote
                .as_ref()
                .map(|remote| remote.session.connection().movement_replay()),
        )
    }
    fn print_stats(&self, label: &str) {
        if let Some(view) = self.view.as_deref() {
            let s = view.stats();
            println!(
                "{label} frames={} resident_pages={} resident_quads={} loaded_pages={} simulation_ticks={} msaa={}",
                s.terrain_frames,
                s.resident_pages,
                s.resident_quads,
                s.loaded_pages,
                s.simulation_ticks,
                render::MSAA_SAMPLES
            );
        }
    }
    fn capture(&mut self, enabled: bool) {
        if enabled && self.flight.benchmark_configured() {
            return;
        }
        if enabled {
            if let Err(error) = self
                .window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| self.window.set_cursor_grab(CursorGrabMode::Confined))
            {
                eprintln!("Cannot capture the mouse: {error}");
                return;
            }
            self.window.set_cursor_visible(false);
            self.captured = true;
            if let Some(view) = self.view.as_deref_mut() {
                view.activate_player();
            }
        } else {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
            self.window.set_cursor_visible(true);
            self.captured = false;
        }
    }
    fn keyboard(&mut self, code: KeyCode, pressed: bool) {
        let modifier = match code {
            KeyCode::ControlLeft => Some(0),
            KeyCode::ControlRight => Some(1),
            KeyCode::ShiftLeft => Some(2),
            KeyCode::ShiftRight => Some(3),
            _ => None,
        };
        if let Some(index) = modifier {
            self.modifiers[index] = pressed;
        }
        if let Some(key) = input_key(code) {
            let down = match key {
                InputKey::Sprint => self.modifiers[0] || self.modifiers[1],
                InputKey::Sneak => self.modifiers[2] || self.modifiers[3],
                _ => pressed,
            };
            self.input(key, down);
        }
    }
    fn ui_modifiers(&self) -> ui::app::Modifiers {
        ui::app::Modifiers {
            ctrl: self.modifiers[0] || self.modifiers[1],
            shift: self.modifiers[2] || self.modifiers[3],
        }
    }
    fn product_key(&mut self, code: KeyCode, pressed: bool, text: Option<&str>) {
        match code {
            KeyCode::ControlLeft => self.modifiers[0] = pressed,
            KeyCode::ControlRight => self.modifiers[1] = pressed,
            KeyCode::ShiftLeft => self.modifiers[2] = pressed,
            KeyCode::ShiftRight => self.modifiers[3] = pressed,
            _ => {}
        }
        let modifiers = self.ui_modifiers();
        if let Some(product) = &mut self.product {
            product.key(
                self.view.as_deref_mut(),
                &format!("{code:?}"),
                pressed,
                text,
                modifiers,
            );
        }
    }
    fn input(&mut self, key: InputKey, pressed: bool) {
        if self.flight.benchmark_configured() {
            return;
        }
        if let Some(view) = self.view.as_deref_mut() {
            view.set_key(key, pressed);
        }
    }
}
impl Drop for NativeState {
    fn drop(&mut self) {
        if let Some(thread) = self.loading_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.ui_loading_thread.take() {
            let _ = thread.join();
        }
    }
}
struct App {
    state: Option<Box<NativeState>>,
    scene: SceneMode,
    smoke_frames: Option<u32>,
    frames: u32,
    failure: Option<String>,
    started: Instant,
    next_redraw: Instant,
    mesh_delay_ticks: u64,
    connection: Option<ConnectOptions>,
    benchmark: Option<flight::BenchmarkOptions>,
    benchmark_frames: u32,
    software: bool,
    legacy_gpu_greedy: bool,
}
impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        eprintln!("{error}");
        self.failure = Some(error);
        event_loop.exit();
    }
}
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = match event_loop.create_window(
            Window::default_attributes()
                .with_title("Cubi Cyber")
                .with_inner_size(winit::dpi::LogicalSize::new(960, 540)),
        ) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(event_loop, error.to_string());
                return;
            }
        };
        match pollster::block_on(NativeState::new(
            window,
            self.smoke_frames.is_some() || self.software,
            self.scene,
            self.connection.clone(),
            self.benchmark.clone(),
            if self.legacy_gpu_greedy {
                client::view::NearRasterization::LegacyGreedy
            } else {
                client::view::NearRasterization::CompactUnit
            },
        )) {
            Ok(mut state) => {
                state.demo_mesh_delay_ticks = self.mesh_delay_ticks;
                self.state = Some(Box::new(state));
            }
            Err(error) => self.fail(event_loop, error),
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Err(error) = state.resize(size.width, size.height) {
                    self.fail(event_loop, error);
                }
            }
            WindowEvent::Focused(false) => {
                if let Some(product) = &mut state.product
                    && product.layer.app.screen == ui::app::Screen::Game
                    && product.layer.app.focus == ui::app::Focus::None
                {
                    product.layer.app.open(ui::app::Screen::Pause);
                }
                state.capture(false);
                state.modifiers.fill(false);
                if let Some(view) = state.view.as_deref_mut() {
                    view.clear_keys();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if state.product.is_some() {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        state.product_key(code, pressed, event.text.as_deref());
                    }
                } else if event.physical_key == PhysicalKey::Code(KeyCode::F8)
                    && pressed
                    && !event.repeat
                {
                    if let Err(error) = state.export_flight() {
                        self.fail(event_loop, error);
                    }
                } else if event.physical_key == PhysicalKey::Code(KeyCode::Escape) && pressed {
                    state.capture(false);
                } else if let PhysicalKey::Code(code) = event.physical_key {
                    state.keyboard(code, pressed);
                }
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                let pressed = button_state == ElementState::Pressed;
                if state.product.is_some() {
                    let world = state.captured
                        && state.product.as_ref().is_some_and(|p| {
                            p.layer.app.screen == ui::app::Screen::Game
                                && p.layer.app.focus == ui::app::Focus::None
                        });
                    if !state.captured
                        && pressed
                        && state.product.as_ref().is_some_and(|p| {
                            p.layer.app.screen == ui::app::Screen::Game
                                && p.layer.app.focus == ui::app::Focus::None
                        })
                    {
                        state.capture(true);
                    } else if world {
                        match button {
                            MouseButton::Left => state.input(InputKey::Attack, pressed),
                            MouseButton::Right => state.input(InputKey::Place, pressed),
                            _ => {}
                        }
                    } else if pressed {
                        let modifiers = state.ui_modifiers();
                        if let Some(product) = &mut state.product {
                            product.layer.app.click(
                                state.pointer[0],
                                state.pointer[1],
                                button == MouseButton::Right,
                                modifiers,
                            );
                            product.click_sound();
                        }
                    }
                } else if pressed && !state.captured {
                    state.capture(true);
                } else {
                    match button {
                        MouseButton::Left => state.input(InputKey::Attack, pressed),
                        MouseButton::Right => state.input(InputKey::Place, pressed),
                        _ => {}
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.pointer = [position.x as f32, position.y as f32];
                if let Some(product) = &mut state.product {
                    product
                        .layer
                        .app
                        .pointer(state.pointer[0], state.pointer[1]);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(product) = &mut state.product {
                    let y = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(position) => {
                            position.y as f32 / 40.0
                        }
                    };
                    product.layer.app.wheel(y.signum() as i32);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if let Some(product) = &mut state.product
                    && matches!(
                        product.layer.app.focus,
                        ui::app::Focus::Name
                            | ui::app::Focus::Address
                            | ui::app::Focus::Search
                            | ui::app::Focus::Chat
                    )
                {
                    product.layer.app.text(&text);
                }
            }
            WindowEvent::RedrawRequested => {
                if state.quit_requested {
                    event_loop.exit();
                    return;
                }
                if self.benchmark.is_some() && self.started.elapsed().as_secs() > 600 {
                    self.fail(event_loop, "native benchmark timeout".into());
                    return;
                }
                if self.smoke_frames.is_some() && self.started.elapsed().as_secs() > 90 {
                    self.fail(
                        event_loop,
                        "native content smoke timeout; no completed visible scene".into(),
                    );
                    return;
                }
                match state.draw(self.smoke_frames.is_some()) {
                    Ok(Some(true)) => {
                        if state.flight.complete() {
                            event_loop.exit();
                            return;
                        }
                        self.frames += 1;
                        if self.smoke_frames.is_some_and(|limit| self.frames >= limit) {
                            if self.scene == SceneMode::Sky {
                                println!(
                                    "NATIVE_SKY_OK frames={} msaa={} max_frames_in_flight={}",
                                    self.frames,
                                    render::MSAA_SAMPLES,
                                    render::MAX_FRAMES_IN_FLIGHT
                                );
                                let work = state
                                    .recorder
                                    .iter()
                                    .next_back()
                                    .expect("presented frame")
                                    .work;
                                println!(
                                    "FRAME_WORK pipeline_compilations={} synchronous_generations={} uploads={}",
                                    work.pipeline_compilations,
                                    work.synchronous_generations,
                                    work.upload_bytes
                                );
                            } else if self.scene == SceneMode::Product && state.view.is_none() {
                                println!(
                                    "NATIVE_UI_OK frames={} screen={:?}",
                                    self.frames,
                                    state.product.as_ref().map(|p| p.layer.app.screen)
                                );
                            } else {
                                state.print_stats("NATIVE_TERRAIN_OK");
                            }
                            event_loop.exit();
                        }
                    }
                    Ok(_) => {}
                    Err(error) => self.fail(event_loop, error),
                }
            }
            _ => {}
        }
    }
    fn device_event(&mut self, _: &ActiveEventLoop, _: winit::event::DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(state) = self.state.as_mut()
            && state.captured
            && let Some(view) = state.view.as_deref_mut()
        {
            if let Some(product) = &state.product {
                if product.layer.app.screen == ui::app::Screen::Game
                    && product.layer.app.focus == ui::app::Focus::None
                    && let Some([dx, dy]) = client::ui_bridge::mouse_delta(
                        &product.layer.app.settings,
                        delta.0,
                        delta.1,
                    )
                {
                    view.mouse_delta(dx, dy);
                    product.trace_motion([dx, dy], view.player().yaw, view.player().pitch);
                }
            } else {
                view.mouse_delta(delta.0, delta.1);
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_redraw {
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
            self.next_redraw = now + Duration::from_millis(16);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_redraw));
    }
}
fn input_key(code: KeyCode) -> Option<InputKey> {
    Some(match code {
        KeyCode::KeyW => InputKey::Forward,
        KeyCode::KeyS => InputKey::Back,
        KeyCode::KeyA => InputKey::Left,
        KeyCode::KeyD => InputKey::Right,
        KeyCode::Space => InputKey::Jump,
        KeyCode::ControlLeft | KeyCode::ControlRight => InputKey::Sprint,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => InputKey::Sneak,
        KeyCode::KeyF => InputKey::Flight,
        KeyCode::KeyH => InputKey::Noclip,
        KeyCode::KeyV => InputKey::Camera,
        _ => return None,
    })
}
fn main() -> Result<(), String> {
    let mut app = App {
        state: None,
        scene: SceneMode::Product,
        smoke_frames: None,
        frames: 0,
        failure: None,
        started: Instant::now(),
        next_redraw: Instant::now(),
        mesh_delay_ticks: 0,
        connection: None,
        benchmark: None,
        benchmark_frames: client::benchmark::FRAMES,
        software: false,
        legacy_gpu_greedy: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--benchmark" => {
                app.scene = SceneMode::Forest;
                app.benchmark = Some(flight::BenchmarkOptions {
                    output: args
                        .next()
                        .ok_or("--benchmark requires a JSON output path")?
                        .into(),
                    frames: app.benchmark_frames,
                });
            }
            "--benchmark-frames" => {
                app.benchmark_frames = args
                    .next()
                    .ok_or("--benchmark-frames requires N")?
                    .parse()
                    .map_err(|_| "invalid benchmark frame count")?;
                if app.benchmark_frames == 0 || app.benchmark_frames > client::benchmark::FRAMES {
                    return Err("benchmark frames must be 1..1800".into());
                }
            }
            "--software" => app.software = true,
            "--legacy-gpu-greedy" => app.legacy_gpu_greedy = true,
            "--connect" => {
                let url = args.next().ok_or("--connect requires ws://loopback:port")?;
                app.connection = Some(ConnectOptions {
                    url,
                    identity: std::path::PathBuf::from(".local/client/identity.cubi"),
                    name: "Player".into(),
                });
            }
            "--identity" => {
                let path = args
                    .next()
                    .ok_or("--identity requires a path after --connect")?;
                app.connection
                    .as_mut()
                    .ok_or("--identity must follow --connect")?
                    .identity = path.into();
            }
            "--name" => {
                let name = args
                    .next()
                    .ok_or("--name requires a name after --connect")?;
                app.connection
                    .as_mut()
                    .ok_or("--name must follow --connect")?
                    .name = name;
            }
            "--scene" => {
                app.scene = match args.next().as_deref() {
                    Some("sky") => SceneMode::Sky,
                    Some("forest") => SceneMode::Forest,
                    _ => return Err("--scene expects sky or forest".into()),
                }
            }
            "--mesh-delay-ms" => {
                let delay = args
                    .next()
                    .ok_or("--mesh-delay-ms requires N")?
                    .parse::<u64>()
                    .map_err(|_| "invalid mesh delay")?;
                if delay > 10_000 {
                    return Err("mesh delay exceeds10000ms bound".into());
                }
                app.mesh_delay_ticks = delay.div_ceil(50);
            }
            "--smoke-frames" => {
                let count = args
                    .next()
                    .ok_or("--smoke-frames requires N")?
                    .parse::<u32>()
                    .map_err(|_| "invalid smoke frame count")?;
                if count == 0 {
                    return Err("smoke frame count must be positive".into());
                }
                app.smoke_frames = Some(count);
            }
            _ => return Err(format!("unknown argument:{arg}")),
        }
    }
    if let Some(benchmark) = &mut app.benchmark {
        benchmark.frames = app.benchmark_frames;
        if app.connection.is_some() || app.scene != SceneMode::Forest || app.smoke_frames.is_some()
        {
            return Err(
                "the fixed benchmark requires standalone forest and no --smoke-frames".into(),
            );
        }
    } else if app.benchmark_frames != client::benchmark::FRAMES {
        return Err("--benchmark-frames requires --benchmark".into());
    }
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())?;
    if let Some(error) = app.failure {
        return Err(error);
    }
    if let Some(expected) = app.smoke_frames
        && app.frames != expected
    {
        return Err(format!(
            "smoke closed after{} of{expected} ready frames",
            app.frames
        ));
    }
    if app.benchmark.is_some()
        && !app
            .state
            .as_ref()
            .is_some_and(|state| state.flight.complete())
    {
        return Err("benchmark window closed before the requested route completed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_state_moves_exclude_the_frame_history_storage() {
        assert!(std::mem::size_of::<telemetry::FrameRecorder<600>>() > 40 * 1024);
        assert!(std::mem::size_of::<NativeState>() < 16 * 1024);
        assert!(std::mem::size_of::<App>() < 1024);
    }
}
