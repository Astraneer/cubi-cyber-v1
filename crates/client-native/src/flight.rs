//! Local window measurements. GPU readbacks are bounded and asynchronous;
//! unavailable pass timings remain unknown. Export never includes credentials.
use client::{benchmark, capture::FrameGpuCapture, view::ClientView};
use std::{fmt::Write, path::PathBuf, time::Instant};
use telemetry::flight::{FlightRecorder, Observations, ReplayInput, Sample};

#[derive(Clone)]
pub struct BenchmarkOptions {
    pub output: PathBuf,
    pub frames: u32,
}
struct Route {
    options: BenchmarkOptions,
    started: bool,
    frame: u32,
    stable: u8,
    initial_hash: u64,
    start_tick: u64,
    exported: bool,
}
pub struct NativeFlight {
    recorder: FlightRecorder,
    capture: FrameGpuCapture,
    started: Instant,
    previous_ns: u64,
    frame: u64,
    route: Option<Route>,
    presented: u64,
}
impl NativeFlight {
    pub fn new(device: &wgpu::Device, options: Option<BenchmarkOptions>) -> Result<Self, String> {
        if options
            .as_ref()
            .is_some_and(|options| options.frames == 0 || options.frames > benchmark::FRAMES)
        {
            return Err("benchmark frames must be 1..1800".into());
        }
        Ok(Self {
            recorder: FlightRecorder::new(4096, 4096).map_err(str::to_owned)?,
            capture: FrameGpuCapture::new(device),
            started: Instant::now(),
            previous_ns: 0,
            frame: 0,
            presented: 0,
            route: options.map(|options| Route {
                options,
                started: false,
                frame: 0,
                stable: 0,
                initial_hash: 0,
                start_tick: 0,
                exported: false,
            }),
        })
    }
    /// Run before beginning the measured allocation scope. The bounded full
    /// route buffer is reserved once after CPU/GPU world warmup has completed.
    pub fn prepare_route(&mut self, view: &mut ClientView) -> Result<(), String> {
        let Some(route) = &mut self.route else {
            return Ok(());
        };
        if route.started {
            return Ok(());
        }
        let stats = view.stats();
        if view.is_ready() && stats.queued_jobs == 0 && stats.running_jobs == 0 {
            route.stable = route.stable.saturating_add(1);
        } else {
            route.stable = 0;
        }
        if route.stable < 3 {
            return Ok(());
        }
        view.start_benchmark_route()?;
        if !view.stats().player_spawned {
            return Err("benchmark world has no known safe player spawn".into());
        }
        route.initial_hash = view.scene.world().canonical_hash();
        route.start_tick = view.tick().0;
        route.started = true;
        self.recorder = FlightRecorder::new_full_capture(
            route.options.frames as usize + 1,
            route.options.frames as usize + 1,
        )
        .map_err(str::to_owned)?;
        self.started = Instant::now();
        self.previous_ns = 0;
        self.presented = 0;
        let _ = view.take_frame_work();
        println!(
            "NATIVE_BENCH_START route={} frames={} presentation_included=true",
            benchmark::ROUTE,
            route.options.frames
        );
        Ok(())
    }
    pub fn route_input(&self) -> Option<benchmark::RouteInput> {
        self.route
            .as_ref()
            .filter(|route| route.started && route.frame < route.options.frames)
            .and_then(|route| benchmark::input(route.frame))
    }
    pub fn route_active(&self) -> bool {
        self.route.as_ref().is_some_and(|route| route.started)
    }
    pub fn benchmark_configured(&self) -> bool {
        self.route.is_some()
    }
    pub fn route_finished(&self) -> bool {
        self.route
            .as_ref()
            .is_some_and(|route| route.started && route.frame == route.options.frames)
    }
    pub fn complete(&self) -> bool {
        self.route.as_ref().is_some_and(|route| route.exported)
    }
    pub fn collect(&mut self) {
        self.capture.collect(&mut self.recorder)
    }
    pub fn encode(
        &mut self,
        view: &mut ClientView,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Option<client::capture::CaptureTicket> {
        self.capture
            .encode(view.renderer_mut(), encoder, self.frame + 1)
    }
    pub fn submitted(&mut self, ticket: client::capture::CaptureTicket) {
        self.capture.submitted(ticket)
    }
    pub fn record(
        &mut self,
        mut sample: Sample,
        view: Option<&ClientView>,
        presented: bool,
    ) -> Result<(), String> {
        // A benchmark sample always includes one real window presentation.
        // Main-thread polling while its preceding frame is pending is excluded;
        // that wait remains in the next presentation interval.
        if self.route_active() && !presented {
            return Ok(());
        }
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or("flight frame counter exhausted")?;
        let now = nanos(self.started.elapsed());
        sample.frame = self.frame;
        sample.monotonic_ns = now;
        sample.interval_ns = now.saturating_sub(self.previous_ns);
        if let Some(view) = view {
            let stats = view.stats();
            sample.queued_jobs =
                (stats.queued_jobs + stats.running_jobs).min(u32::MAX as usize) as u32;
            self.recorder
                .record_input(ReplayInput {
                    monotonic_ns: now,
                    tick: view.tick().0,
                    sequence: self.frame,
                    buttons: view.input_buttons(),
                    yaw_bits: view.player().yaw.to_bits(),
                    pitch_bits: view.player().pitch.to_bits(),
                })
                .map_err(|error| format!("input recorder: {error:?}"))?;
        }
        self.recorder
            .record_frame(sample)
            .map_err(|error| format!("frame recorder: {error:?}"))?;
        self.previous_ns = now;
        self.presented += u64::from(presented);
        if presented
            && let Some(route) = &mut self.route
            && route.started
        {
            route.frame += 1;
        }
        Ok(())
    }
    pub fn finish_if_drained(
        &mut self,
        adapter: &wgpu::AdapterInfo,
        view: &ClientView,
        width: u32,
        height: u32,
        in_flight: u32,
    ) -> Result<bool, String> {
        if !self.route_finished() || in_flight != 0 || self.capture.pending() != 0 {
            return Ok(false);
        }
        let route = self.route.as_ref().expect("finished route");
        if !route.exported {
            let path = route.options.output.clone();
            self.export(&path, adapter, Some(view), width, height)?;
            self.route.as_mut().expect("route retained").exported = true;
            println!(
                "NATIVE_BENCH_OK frames={} presented={} output={} presentation_included=true",
                self.route.as_ref().unwrap().frame,
                self.presented,
                path.display()
            );
        }
        Ok(true)
    }
    pub fn export(
        &self,
        path: &std::path::Path,
        adapter: &wgpu::AdapterInfo,
        view: Option<&ClientView>,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let (seed, hash, tick) =
            if let Some(route) = self.route.as_ref().filter(|route| route.started) {
                (
                    view.map_or(0, |view| u64::from(view.scene.config().seed.0)),
                    route.initial_hash,
                    route.start_tick,
                )
            } else if let Some(view) = view {
                (
                    u64::from(view.scene.config().seed.0),
                    view.scene.world().canonical_hash(),
                    view.tick().0,
                )
            } else {
                (0, 0, 0)
            };
        let mut json = self.recorder.export_json(
            &format!("{} / {:?}", adapter.name, adapter.backend),
            seed,
            hash,
            tick,
        );
        json.pop();
        let (requested, route_frames, complete) = self
            .route
            .as_ref()
            .filter(|route| route.started)
            .map_or((0, 0, false), |route| {
                (
                    route.options.frames,
                    route.frame,
                    route.frame == benchmark::FRAMES,
                )
            });
        let route_name = if requested != 0 {
            benchmark::ROUTE
        } else {
            "manual-window-input"
        };
        let step_hz = if requested != 0 { "60" } else { "null" };
        let scene = match view {
            Some(view) if view.authoritative_remote() => "authoritative-remote",
            Some(_) => "generated-forest-fixture",
            None => "sky",
        };
        let raster = view.map_or("none", |view| {
            if view.demo_gpu_unit_mesh() {
                "gpu-unit"
            } else {
                "legacy-greedy"
            }
        });
        let object_limit = view.map_or(0, ClientView::gpu_object_limit);
        write!(json, ",\"benchmark\":{{\"route\":\"{}\",\"width\":{},\"height\":{},\"presentationIncluded\":true,\"presentationMeaning\":\"surface.present plus completed GPU submission; scanout is not instrumented\",\"framesRequested\":{},\"routeFrames\":{},\"routeComplete\":{},\"renderedFrames\":{},\"fixedStepHz\":{},\"blockingReadback\":false,\"scene\":\"{}\",\"colorPassIncludesSky\":true,\"nearRasterization\":\"{}\",\"quadStorage\":\"greedy-16B\",\"gpuObjectCapacity\":{},\"workerAllocationsMeasured\":false,\"oldestJobAgeMeasured\":false,\"checkpointStateEmbedded\":false,\"captureIncludesWarmup\":true,\"forcePresentation\":{},\"allocatorScope\":\"simulation jobs upload encode present; loading and callback polling excluded\"}},\"gpuCapture\":{{\"completed\":{},\"unsupported\":{},\"full\":{},\"failed\":{},\"expired\":{}}}}}",
            route_name, width, height, requested, route_frames, complete, self.presented, step_hz, scene, raster, object_limit, requested != 0,
            self.capture.stats().completed, self.capture.stats().unsupported, self.capture.stats().full,
            self.capture.stats().failed, self.capture.stats().expired).map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(path, json).map_err(|error| error.to_string())?;
        println!("NATIVE_FLIGHT_SAVED {}", path.display());
        Ok(())
    }
}
pub fn nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
pub fn waiting() -> Observations {
    Observations(Observations::GPU_WAIT)
}
