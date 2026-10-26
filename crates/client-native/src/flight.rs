//! Local window measurements. GPU readbacks are bounded and asynchronous;
//! unavailable pass timings remain unknown. Export never includes credentials.
use client::{benchmark, capture::FrameGpuCapture, view::ClientView};
use std::{fmt::Write, path::PathBuf, time::Instant};
use telemetry::flight::{FlightRecorder, Observations, ReplayInput, Sample};
mod continuations;

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
    continuations: continuations::CpuContinuations,
}
impl NativeFlight {
    pub fn begin_scene(&mut self) -> Result<(), String> {
        self.recorder
            .begin_input_epoch()
            .map_err(|error| format!("input epoch: {error:?}"))
    }
    pub fn recorder(&self) -> &FlightRecorder {
        &self.recorder
    }
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: Option<BenchmarkOptions>,
    ) -> Result<Self, String> {
        if options
            .as_ref()
            .is_some_and(|options| options.frames == 0 || options.frames > benchmark::FRAMES)
        {
            return Err("benchmark frames must be 1..1800".into());
        }
        Ok(Self {
            recorder: FlightRecorder::new(4096, 4096).map_err(str::to_owned)?,
            capture: FrameGpuCapture::new_with_queue(device, queue),
            started: Instant::now(),
            previous_ns: 0,
            frame: 0,
            presented: 0,
            continuations: continuations::CpuContinuations::default(),
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
        self.continuations = continuations::CpuContinuations::default();
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
        self.collect_with(|_, _, _| {});
    }
    pub fn collect_with(&mut self, completed: impl FnMut(u64, u64, bool)) {
        self.capture.collect_with(&mut self.recorder, completed);
    }
    pub fn begin_gpu_frame(&mut self, encoder: &mut wgpu::CommandEncoder, period: f32) -> bool {
        self.capture.begin_frame(encoder, period)
    }
    pub fn encode(
        &mut self,
        view: &mut ClientView,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Option<client::capture::CaptureTicket> {
        view.encode_gpu_capture(&mut self.capture, encoder, self.frame + 1)
    }
    pub fn submitted(&mut self, ticket: client::capture::CaptureTicket) {
        self.capture.submitted(ticket)
    }
    pub fn record(
        &mut self,
        sample: Sample,
        view: Option<&ClientView>,
        presented: bool,
    ) -> Result<(), String> {
        self.record_with(sample, view, presented, |_| {})
    }
    pub fn record_with(
        &mut self,
        mut sample: Sample,
        view: Option<&ClientView>,
        presented: bool,
        mut recorded: impl FnMut(&Sample),
    ) -> Result<(), String> {
        // A benchmark sample always includes one real window presentation.
        // Main-thread polling while its preceding frame is pending is excluded;
        // that wait remains in the next presentation interval.
        if self.route_active() && !presented {
            return Ok(());
        }
        if !presented {
            self.continuations.push(sample);
            return Ok(());
        }
        self.continuations.finish(&mut sample);
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
        recorded(&sample);
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
        self.export_with_movement(path, adapter, view, width, height, None)
    }
    pub fn export_with_movement(
        &self,
        path: &std::path::Path,
        adapter: &wgpu::AdapterInfo,
        view: Option<&ClientView>,
        width: u32,
        height: u32,
        movement: Option<&client::replay::Recorder>,
    ) -> Result<(), String> {
        let (seed, hash, tick) =
            if let Some(route) = self.route.as_ref().filter(|route| route.started) {
                (
                    view.map_or(0, |view| u64::from(view.scene.config().seed.0)),
                    route.initial_hash,
                    route.start_tick,
                )
            } else if let Some(view) = view {
                (u64::from(view.scene.config().seed.0), 0, 0)
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
        let sample_count = view.map_or(0, ClientView::render_sample_count);
        let whole_intervals = self.capture.stats().whole_intervals;
        write!(json, ",\"benchmark\":{{\"route\":\"{}\",\"width\":{},\"height\":{},\"presentationIncluded\":true,\"presentationMeaning\":\"surface.present plus completed GPU submission; scanout is not instrumented\",\"framesRequested\":{},\"routeFrames\":{},\"routeComplete\":{},\"renderedFrames\":{},\"fixedStepHz\":{},\"blockingReadback\":false,\"scene\":\"{}\",\"colorPassIncludesSky\":true,\"nearRasterization\":\"{}\",\"quadStorage\":\"greedy-16B\",\"gpuObjectCapacity\":{},\"sampleCount\":{sample_count},\"workerAllocationsMeasured\":false,\"oldestJobAgeMeasured\":false,\"checkpointStateEmbedded\":false,\"warmupExcluded\":{},\"gpuTimeMeaning\":\"whole encoder interval when gpuTotalComplete is true, otherwise partial pass sum; queue staging before the encoder, capture readback and scanout excluded\",\"forcePresentation\":{},\"allocatorScope\":\"simulation jobs upload encode present; loading and callback polling excluded\"}},\"gpuCapture\":{{\"wholeIntervals\":{whole_intervals},\"completed\":{},\"unsupported\":{},\"full\":{},\"failed\":{},\"expired\":{}}}}}",
            route_name, width, height, requested, route_frames, complete, self.presented, step_hz, scene, raster, object_limit, requested != 0, requested != 0,
            self.capture.stats().completed, self.capture.stats().unsupported, self.capture.stats().full,
            self.capture.stats().failed, self.capture.stats().expired).map_err(|error| error.to_string())?;
        let movement = movement.or_else(|| view.map(ClientView::movement_replay));
        let available = movement.is_some_and(|movement| movement.append_json(&mut json).is_ok());
        json.pop();
        write!(json, ",\"initialPublicWorldHashKnown\":{},\"currentPublicWorldHash\":\"{:016x}\",\"movementReplayAvailable\":{}", self.route.as_ref().is_some_and(|route| route.started), view.map_or(0, |view| view.scene.world().canonical_hash()), available).map_err(|error| error.to_string())?;
        if !available {
            json.push_str(
                ",\"movementReplayUnavailableReason\":\"no complete movement steps retained\"",
            );
        }
        json.push('}');
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
