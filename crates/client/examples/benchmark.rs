//! Native offscreen route using the same ClientView, worker pool and renderer.
use client::{benchmark, capture::FrameGpuCapture, view::ClientView};
use render::terrain_gpu::{AtlasImage, TerrainGpu, UploadBudget};
use std::{
    fmt::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
use telemetry::{
    AllocationScope,
    flight::{FlightRecorder, Observations, ReplayInput, Sample},
};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

struct Options {
    out: PathBuf,
    frames: u32,
    width: u32,
    height: u32,
    software: bool,
}
impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Self {
            out: ".local/artifacts/bench/run.json".into(),
            frames: benchmark::FRAMES,
            width: 1280,
            height: 720,
            software: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => options.out = args.next().ok_or("--out requires a path")?.into(),
                "--frames" => {
                    options.frames = args
                        .next()
                        .ok_or("--frames requires a number")?
                        .parse()
                        .map_err(|_| "invalid frame count")?
                }
                "--width" => {
                    options.width = args
                        .next()
                        .ok_or("--width requires a number")?
                        .parse()
                        .map_err(|_| "invalid width")?
                }
                "--height" => {
                    options.height = args
                        .next()
                        .ok_or("--height requires a number")?
                        .parse()
                        .map_err(|_| "invalid height")?
                }
                "--software" => options.software = true,
                _ => return Err(format!("unknown option: {arg}")),
            }
        }
        if options.frames == 0
            || options.frames > benchmark::FRAMES
            || !(64..=3840).contains(&options.width)
            || !(64..=2160).contains(&options.height)
        {
            return Err("frames must be 1..1800; dimensions 64..3840 × 64..2160".into());
        }
        Ok(options)
    }
}
fn ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
fn main() -> Result<(), String> {
    pollster::block_on(run(Options::parse()?))
}
async fn run(options: Options) -> Result<(), String> {
    let instance = render::instance();
    let gpu = render::Gpu::request(&instance, None, options.software).await?;
    let adapter = gpu.adapter.get_info();
    println!(
        "BENCH_SETUP adapter={} backend={:?} presentation_included=false",
        adapter.name, adapter.backend
    );
    let atlas = AtlasImage::donor()?;
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        options.width,
        options.height,
        render::OutputTransfer::Attachment,
        client::view::terrain_gpu_config(),
        &atlas,
    )
    .map_err(|e| e.to_string())?;
    let mut view = ClientView::new(renderer, client::SceneConfig::forest())?;
    let replay_started = Instant::now();
    let pool = client::native_pool(2, 16).map_err(|e| format!("world workers: {e:?}"))?;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("benchmark actual terrain output"),
        size: wgpu::Extent3d {
            width: options.width,
            height: options.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let warm_started = Instant::now();
    let mut stable = 0;
    loop {
        view.set_movement_replay_time(ns(replay_started.elapsed()))?;
        view.advance_input(benchmark::STEP_SECONDS)?;
        let tick = view.tick();
        view.scene
            .pump_native(&pool, tick, &mut client::ClientScene::default_budget(), 8)
            .map_err(|e| e.to_string())?;
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))?;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(
            &gpu.queue,
            &mut encoder,
            &output,
            options.width,
            options.height,
        )?;
        gpu.queue.submit([encoder.finish()]);
        gpu.device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| e.to_string())?;
        let stats = view.stats();
        if view.is_ready() && stats.queued_jobs == 0 && stats.running_jobs == 0 {
            stable += 1;
        } else {
            stable = 0;
        }
        if stable >= 3 {
            break;
        }
        if warm_started.elapsed() > Duration::from_secs(300) {
            return Err(format!("world warmup timed out: {stats:?}"));
        }
        std::thread::yield_now();
    }
    view.start_benchmark_route()?;
    if !view.stats().player_spawned {
        return Err("loaded world has no safe player spawn".into());
    }
    let _ = view.take_frame_work();
    let initial_hash = view.scene.world().canonical_hash();
    let start_tick = view.tick().0;
    let mut recorder =
        FlightRecorder::new_full_capture(options.frames as usize + 1, options.frames as usize + 1)
            .map_err(str::to_owned)?;
    let mut capture = FrameGpuCapture::new_with_queue(&gpu.device, &gpu.queue);
    let started = Instant::now();
    let mut previous = 0;
    let mut completed = 0;
    for frame in 0..options.frames {
        let frame_started = Instant::now();
        let scope = AllocationScope::begin().map_err(|e| format!("allocator: {e:?}"))?;
        let command = benchmark::input(frame).ok_or("route exhausted")?;
        let input_started = Instant::now();
        command.apply(&mut view);
        let input_ns = ns(input_started.elapsed());
        let sim_started = Instant::now();
        view.set_movement_replay_time(ns(replay_started.elapsed()))?;
        view.advance_input(benchmark::STEP_SECONDS)?;
        let simulation_ns = ns(sim_started.elapsed());
        let jobs_started = Instant::now();
        let tick = view.tick();
        view.scene
            .pump_native(&pool, tick, &mut client::ClientScene::default_budget(), 8)
            .map_err(|e| e.to_string())?;
        let jobs_ns = ns(jobs_started.elapsed());
        let upload_started = Instant::now();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))?;
        let upload_ns = ns(upload_started.elapsed());
        let encode_started = Instant::now();
        let rendered = view.needs_redraw(options.width, options.height);
        if rendered {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            capture.begin_frame(&mut encoder, gpu.queue.get_timestamp_period());
            view.encode(
                &gpu.queue,
                &mut encoder,
                &output,
                options.width,
                options.height,
            )?;
            let ticket = capture.encode(view.renderer_mut(), &mut encoder, u64::from(frame) + 1);
            gpu.queue.submit([encoder.finish()]);
            if let Some(ticket) = ticket {
                capture.submitted(ticket);
            }
        }
        let encode_ns = ns(encode_started.elapsed());
        let work = view.take_frame_work();
        let allocations = scope.finish();
        let cpu_ns = ns(frame_started.elapsed());
        if rendered {
            gpu.device
                .poll(wgpu::PollType::Wait)
                .map_err(|e| e.to_string())?;
        }
        let now = ns(started.elapsed());
        let stats = view.stats();
        recorder
            .record_input(ReplayInput {
                monotonic_ns: now,
                tick: view.tick().0,
                sequence: u64::from(frame) + 1,
                buttons: command.buttons,
                yaw_bits: view.player().yaw.to_bits(),
                pitch_bits: view.player().pitch.to_bits(),
            })
            .map_err(|e| format!("input record: {e:?}"))?;
        recorder
            .record_frame(Sample {
                frame: u64::from(frame) + 1,
                monotonic_ns: now,
                interval_ns: now.saturating_sub(previous),
                cpu_ns: Some(cpu_ns),
                cpu_stages_ns: [
                    Some(input_ns),
                    Some(simulation_ns),
                    Some(jobs_ns),
                    None,
                    Some(upload_ns),
                    Some(encode_ns),
                    None,
                    None,
                ],
                work,
                allocations: Some(allocations),
                observations: Observations(if rendered { Observations::GPU_WAIT } else { 0 }),
                queued_jobs: (stats.queued_jobs + stats.running_jobs).min(u32::MAX as usize) as u32,
                in_flight: u8::from(rendered),
                ..Sample::default()
            })
            .map_err(|e| format!("frame record: {e:?}"))?;
        capture.collect(&mut recorder);
        previous = now;
        completed += u32::from(rendered);
        if frame % 300 == 299 {
            println!(
                "BENCH_PROGRESS frames={} rendered={} queued={}",
                frame + 1,
                completed,
                stats.queued_jobs + stats.running_jobs
            );
        }
    }
    let mut json = recorder.export_json(
        &format!("{} / {:?}", adapter.name, adapter.backend),
        u64::from(view.scene.config().seed.0),
        initial_hash,
        start_tick,
    );
    json.pop();
    let sample_count = view.render_sample_count();
    write!(json, ",\"benchmark\":{{\"route\":\"{}\",\"width\":{},\"height\":{},\"presentationIncluded\":false,\"framesRequested\":{},\"routeFrames\":{},\"routeComplete\":{},\"renderedFrames\":{},\"fixedStepHz\":60,\"blockingReadback\":true,\"scene\":\"generated-forest-fixture\",\"colorPassIncludesSky\":true,\"sampleCount\":{sample_count},\"gpuTimeMeaning\":\"whole encoder interval; queue staging before encoder and recorder readback excluded\",\"workerAllocationsMeasured\":false,\"oldestJobAgeMeasured\":false}}}}", benchmark::ROUTE, options.width, options.height, options.frames, benchmark::FRAMES, options.frames == benchmark::FRAMES, completed).map_err(|e| e.to_string())?;
    view.movement_replay()
        .append_json(&mut json)
        .map_err(str::to_owned)?;
    if let Some(parent) = options.out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&options.out, json).map_err(|e| e.to_string())?;
    println!(
        "BENCH_CAPTURE_OK frames={} rendered={} route={} output={} presentation_included=false",
        options.frames,
        completed,
        benchmark::ROUTE,
        options.out.display()
    );
    Ok(())
}
