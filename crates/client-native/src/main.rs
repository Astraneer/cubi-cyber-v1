//! Thin native shell. Windows uses D3D12 with Vulkan fallback; Linux supports CI.

use std::sync::Arc;

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

struct NativeState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: render::Gpu,
    sky: render::SkyRenderer,
    recorder: telemetry::FrameRecorder<600>,
    frames: u64,
}

impl NativeState {
    async fn new(window: Arc<Window>, software: bool) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            match Self::new_with_backend(window.clone(), software, wgpu::Backends::DX12).await {
                Ok(state) => Ok(state),
                Err(dx12_error) => {
                    eprintln!("D3D12 initialization failed; trying Vulkan: {dx12_error}");
                    Self::new_with_backend(window, software, wgpu::Backends::VULKAN)
                        .await
                        .map_err(|vulkan_error| {
                            format!("D3D12: {dx12_error}; Vulkan: {vulkan_error}")
                        })
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        Self::new_with_backend(window, software, wgpu::Backends::VULKAN).await
    }

    async fn new_with_backend(
        window: Arc<Window>,
        software: bool,
        backend: wgpu::Backends,
    ) -> Result<Self, String> {
        let instance = render::instance_for(backend);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| format!("surface initialization failed: {error}"))?;
        let gpu = render::Gpu::request(&instance, Some(&surface), software).await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface has no compatible format")?;
        config.desired_maximum_frame_latency = render::MAX_FRAMES_IN_FLIGHT;
        config.present_mode = wgpu::PresentMode::Fifo;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        if let Some(format) = capabilities.formats.iter().find(|format| format.is_srgb()) {
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
        Ok(Self {
            window,
            surface,
            config,
            gpu,
            sky,
            recorder: telemetry::FrameRecorder::new(),
            frames: 0,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.sky.resize(&self.gpu.device, width, height);
    }

    fn draw(&mut self) -> Result<bool, String> {
        let started = std::time::Instant::now();
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.resize(size.width, size.height);
                return Ok(false);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(false),
            Err(error) => return Err(format!("surface frame failed: {error}")),
        };
        let output = frame.texture.create_view(&Default::default());
        self.sky.render(&self.gpu.device, &self.gpu.queue, &output);
        self.window.pre_present_notify();
        frame.present();
        self.frames += 1;
        self.recorder.push(telemetry::FrameRecord {
            frame: self.frames,
            cpu_ns: started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            gpu_ns: None,
            work: self.sky.take_work(),
        });
        Ok(true)
    }
}

#[derive(Default)]
struct App {
    state: Option<NativeState>,
    smoke_frames: Option<u32>,
    frames: u32,
    failure: Option<String>,
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
                .with_title("Cubi Cyber — M0")
                .with_inner_size(winit::dpi::LogicalSize::new(960, 540)),
        ) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(event_loop, format!("window initialization failed: {error}"));
                return;
            }
        };
        match pollster::block_on(NativeState::new(window, self.smoke_frames.is_some())) {
            Ok(state) => self.state = Some(state),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::RedrawRequested => match state.draw() {
                Ok(true) => {
                    self.frames += 1;
                    if self.smoke_frames.is_some_and(|limit| self.frames >= limit) {
                        println!(
                            "NATIVE_SKY_OK frames={} msaa={} max_frames_in_flight={}",
                            self.frames,
                            render::MSAA_SAMPLES,
                            render::MAX_FRAMES_IN_FLIGHT,
                        );
                        let record = state
                            .recorder
                            .iter()
                            .next_back()
                            .expect("presented frame recorded");
                        println!(
                            "FRAME_WORK pipeline_compilations={} synchronous_generations={} uploads={}",
                            record.work.pipeline_compilations,
                            record.work.synchronous_generations,
                            record.work.upload_bytes
                        );
                        event_loop.exit();
                    }
                }
                Ok(false) => {}
                Err(error) => self.fail(event_loop, error),
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

fn main() -> Result<(), String> {
    let mut app = App::default();
    let mut args = std::env::args().skip(1);
    if let Some(argument) = args.next() {
        if argument != "--smoke-frames" {
            return Err(format!(
                "unknown argument: {argument}; expected --smoke-frames N"
            ));
        }
        let count = args
            .next()
            .ok_or("--smoke-frames requires a positive integer")?
            .parse::<u32>()
            .map_err(|_| "--smoke-frames requires a positive integer")?;
        if count == 0 || args.next().is_some() {
            return Err("usage: client-native [--smoke-frames N], N > 0".into());
        }
        app.smoke_frames = Some(count);
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
            "smoke test closed after {} of {expected} frames",
            app.frames
        ));
    }
    Ok(())
}
