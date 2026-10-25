//! Thin native window/input shell; shared client owns world, budgets and camera.
mod remote_transport;
use client::view::{ClientView, InputKey};
use render::terrain_gpu::{AtlasImage, TerrainGpu, UploadBudget};
use std::{
    sync::{Arc, mpsc},
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
    #[default]
    Forest,
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
            name: options.name,
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
    view: Option<ClientView>,
    loading: Option<mpsc::Receiver<Result<TerrainGpu, String>>>,
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
}
impl NativeState {
    async fn new(
        window: Arc<Window>,
        software: bool,
        mode: SceneMode,
        connection: Option<ConnectOptions>,
    ) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            match Self::new_backend(
                window.clone(),
                software,
                mode,
                wgpu::Backends::DX12,
                connection.clone(),
            )
            .await
            {
                Ok(state) => Ok(state),
                Err(dx12) => {
                    eprintln!("D3D12 initialization failed; trying Vulkan: {dx12}");
                    Self::new_backend(window, software, mode, wgpu::Backends::VULKAN, connection)
                        .await
                        .map_err(|vulkan| format!("D3D12:{dx12}; Vulkan:{vulkan}"))
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        Self::new_backend(window, software, mode, wgpu::Backends::VULKAN, connection).await
    }
    async fn new_backend(
        window: Arc<Window>,
        software: bool,
        mode: SceneMode,
        backend: wgpu::Backends,
        connection: Option<ConnectOptions>,
    ) -> Result<Self, String> {
        let instance = render::instance_for(backend);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| error.to_string())?;
        let gpu = render::Gpu::request(&instance, Some(&surface), software).await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface has no compatible format")?;
        config.desired_maximum_frame_latency = if mode == SceneMode::Forest {
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
        let (loading, loading_thread, workers) = if mode == SceneMode::Forest {
            let (sender, receiver) = mpsc::sync_channel(1);
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            let format = config.format;
            let (width, height) = (config.width, config.height);
            // Actual separate loading thread: async fn alone would still block frames.
            let thread = std::thread::Builder::new()
                .name("cubi-pipeline-loading".into())
                .spawn(move || {
                    let result = AtlasImage::donor().and_then(|atlas| {
                        TerrainGpu::new(
                            &device,
                            &queue,
                            format,
                            width,
                            height,
                            render::OutputTransfer::Attachment,
                            client::view::terrain_gpu_config(),
                            &atlas,
                        )
                        .map_err(|error| error.to_string())
                    });
                    let _ = sender.send(result);
                })
                .map_err(|error| format!("loading worker failed:{error}"))?;
            let workers = client::native_pool(2, 16)
                .map_err(|error| format!("world worker pool failed:{error:?}"))?;
            (Some(receiver), Some(thread), Some(workers))
        } else {
            (None, None, None)
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
        })
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.sky.resize(&self.gpu.device, width, height);
        if let Some(view) = self.view.as_mut() {
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
                let mut terrain = result?;
                terrain
                    .resize(&self.gpu.device, self.config.width, self.config.height)
                    .map_err(|error| error.to_string())?;
                let scene = if self.remote.is_some() {
                    client::remote::RemoteSession::scene_config()
                } else {
                    client::SceneConfig::forest()
                };
                let mut view = ClientView::new(terrain, scene)?;
                view.set_demo_mesh_delay_ticks(self.demo_mesh_delay_ticks);
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
        let started = Instant::now();
        let elapsed = self.last_frame.elapsed().as_secs_f64();
        self.last_frame = Instant::now();
        self.poll_loading()?;
        let now = self
            .network_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(remote) = self.remote.as_mut() {
            remote.pump_socket(now)?;
        }
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(None);
        }
        if let Some(view) = self.view.as_mut() {
            if let Some(remote) = self.remote.as_mut() {
                remote.session.pump(view, now)?;
                remote.pump_socket(now)?;
            }
            view.advance_input(elapsed)?;
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
            if let Some(error) = view.scene.error() {
                return Err(error.to_string());
            }
            view.upload_ready(&self.gpu.queue, &mut UploadBudget::new(512 * 1024))?;
        }
        if !force_present
            && self
                .view
                .as_ref()
                .is_some_and(|view| !view.needs_redraw(self.config.width, self.config.height))
        {
            return Ok(None);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.resize(size.width, size.height)?;
                return Ok(None);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(None),
            Err(error) => return Err(format!("surface frame failed:{error}")),
        };
        let output = frame.texture.create_view(&Default::default());
        let work = if let Some(view) = self.view.as_mut() {
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
            self.gpu.queue.submit([encoder.finish()]);
            view.take_frame_work()
        } else {
            self.sky.render(&self.gpu.device, &self.gpu.queue, &output);
            self.sky.take_work()
        };
        self.window.pre_present_notify();
        frame.present();
        self.frames += 1;
        self.recorder.push(telemetry::FrameRecord {
            frame: self.frames,
            cpu_ns: started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            gpu_ns: None,
            work,
        });
        let ready =
            self.mode == SceneMode::Sky || self.view.as_ref().is_some_and(ClientView::is_ready);
        if ready && !self.reported_ready {
            self.reported_ready = true;
            if self.mode == SceneMode::Forest {
                self.print_stats("NATIVE_TERRAIN_READY");
            }
        }
        Ok(Some(ready))
    }
    fn print_stats(&self, label: &str) {
        if let Some(view) = self.view.as_ref() {
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
            if let Some(view) = self.view.as_mut() {
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
    fn input(&mut self, key: InputKey, pressed: bool) {
        if let Some(view) = self.view.as_mut() {
            view.set_key(key, pressed);
        }
    }
}
impl Drop for NativeState {
    fn drop(&mut self) {
        if let Some(thread) = self.loading_thread.take() {
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
            self.smoke_frames.is_some(),
            self.scene,
            self.connection.clone(),
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
                state.capture(false);
                state.modifiers.fill(false);
                if let Some(view) = state.view.as_mut() {
                    view.clear_keys();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if event.physical_key == PhysicalKey::Code(KeyCode::Escape) && pressed {
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
                if pressed && !state.captured {
                    state.capture(true);
                } else {
                    match button {
                        MouseButton::Left => state.input(InputKey::Attack, pressed),
                        MouseButton::Right => state.input(InputKey::Place, pressed),
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if self.smoke_frames.is_some() && self.started.elapsed().as_secs() > 90 {
                    self.fail(
                        event_loop,
                        "native content smoke timeout; no completed visible scene".into(),
                    );
                    return;
                }
                match state.draw(self.smoke_frames.is_some()) {
                    Ok(Some(true)) => {
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
            && let Some(view) = state.view.as_mut()
        {
            view.mouse_delta(delta.0, delta.1);
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
        scene: SceneMode::Forest,
        smoke_frames: None,
        frames: 0,
        failure: None,
        started: Instant::now(),
        next_redraw: Instant::now(),
        mesh_delay_ticks: 0,
        connection: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
