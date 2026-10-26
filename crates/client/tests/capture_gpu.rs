#![cfg(not(target_arch = "wasm32"))]
use client::capture::FrameGpuCapture;
use render::terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu};
use telemetry::flight::{FlightRecorder, Sample};
#[test]
fn actual_readbacks_have_bounded_backpressure_frame_identity_and_stale_ticket_guards() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY),
        "software acceptance adapter must support the actual timestamp oracle"
    );
    let mut renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 4,
            max_quads: 64,
            occlusion: true,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let output = gpu
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("capture oracle output"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default());
    let mut capture = FrameGpuCapture::new(&gpu.device);
    let camera = Camera::perspective(
        [5.0, 5.0, 5.0],
        [0.0; 3],
        [0.0, 1.0, 0.0],
        60.0_f32.to_radians(),
        1.0,
        0.1,
        100.0,
    );
    let mut abandoned = gpu.device.create_command_encoder(&Default::default());
    renderer
        .encode_frame(
            &gpu.queue,
            &mut abandoned,
            &output,
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let old = capture.encode(&renderer, &mut abandoned, 7).unwrap();
    capture.cancel(old);
    drop(abandoned);
    let mut abandoned = gpu.device.create_command_encoder(&Default::default());
    renderer.invalidate();
    renderer
        .encode_frame(
            &gpu.queue,
            &mut abandoned,
            &output,
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let replacement = capture.encode(&renderer, &mut abandoned, 7).unwrap();
    capture.cancel(old);
    capture.submitted(old);
    assert_eq!(capture.pending(), 1);
    capture.cancel(replacement);
    assert_eq!(capture.pending(), 0);
    drop(abandoned);
    let mut tickets = Vec::new();
    let mut encoders = Vec::new();
    for frame in 8..=9 {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        renderer.invalidate();
        renderer
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &output,
                &camera,
                CullingMode::FrustumAndHiZ,
            )
            .unwrap();
        tickets.push(capture.encode(&renderer, &mut encoder, frame).unwrap());
        encoders.push(encoder.finish());
    }
    let mut full = gpu.device.create_command_encoder(&Default::default());
    assert!(capture.encode(&renderer, &mut full, 10).is_none());
    assert_eq!(capture.stats().full, 1);
    drop(full);
    gpu.queue.submit(encoders);
    for ticket in tickets {
        capture.submitted(ticket);
    }
    let mut recorder = FlightRecorder::new_full_capture(3, 3).unwrap();
    for frame in 8..=11 {
        recorder
            .record_frame(Sample {
                frame,
                monotonic_ns: frame,
                ..Sample::default()
            })
            .unwrap();
    }
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    capture.collect(&mut recorder);
    assert_eq!(capture.pending(), 0);
    assert_eq!(capture.stats().completed, 2);
    assert_eq!(capture.stats().expired, 1);
    assert_eq!(capture.stats().cancelled, 2);
    assert_eq!(capture.stats().failed, 0);
    let measured = recorder.frames().find(|s| s.frame == 9).unwrap();
    assert!(measured.gpu_ns.unwrap() > 0);
    assert!(measured.gpu_passes_ns[6].is_some(), "actual color query");
    assert!(
        measured.gpu_passes_ns[4].is_none(),
        "unattached shadows remain unmeasured"
    );
    assert!(
        recorder
            .frames()
            .find(|s| s.frame == 10)
            .unwrap()
            .gpu_ns
            .is_none()
    );
    // Copy the same real query stream twice on purpose. Distinct metadata kinds
    // and periods test combined-buffer offsets; this is an API control, not an
    // assertion that these queries came from two different rendering passes.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.invalidate();
    renderer
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &output,
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let graph = renderer.frame_graph();
    let (buffer, period) = renderer.timestamp_buffer().unwrap();
    let sources = [
        render::gpu_time::TimingSource {
            buffer,
            period,
            kinds: [6; 32],
            count: graph.count,
        },
        render::gpu_time::TimingSource {
            buffer,
            period,
            kinds: [13; 32],
            count: graph.count,
        },
    ];
    let ticket = capture.encode_sources(sources, &mut encoder, 12).unwrap();
    gpu.queue.submit([encoder.finish()]);
    capture.submitted(ticket);
    recorder
        .record_frame(Sample {
            frame: 12,
            monotonic_ns: 12,
            ..Sample::default()
        })
        .unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    capture.collect(&mut recorder);
    let sample = recorder.frames().last().unwrap();
    assert!(sample.gpu_passes_ns[6].unwrap() > 0);
    assert_eq!(sample.gpu_passes_ns[6], sample.gpu_passes_ns[13]);
    assert_eq!(sample.gpu_ns, Some(sample.gpu_passes_ns[6].unwrap() * 2));
    let mut invalid = gpu.device.create_command_encoder(&Default::default());
    let source = render::gpu_time::TimingSource {
        buffer,
        period,
        kinds: [16; 32],
        count: graph.count,
    };
    assert!(capture.encode_sources([source], &mut invalid, 13).is_none());
    assert_eq!(capture.stats().failed, 1);
    assert_eq!(capture.pending(), 0);
}

#[test]
fn complete_gpu_interval_includes_unclassified_commands_without_double_counting_passes() {
    use render::gpu_time::{GpuPassTimer, TimingSource};
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut capture = FrameGpuCapture::new_with_queue(&gpu.device, &gpu.queue);
    let timer = GpuPassTimer::<1>::new(&gpu.device, &gpu.queue, "independent inner pass");
    let data = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual unclassified and classified GPU work"),
        size: 4096,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("real timestamp interval workload"),
        source: wgpu::ShaderSource::Wgsl("@group(0)@binding(0)var<storage,read_write> values:array<u32>;@compute @workgroup_size(64)fn main(@builtin(global_invocation_id)i:vec3<u32>){if i.x>=1024u{return;}var x=values[i.x];for(var n=0u;n<256u;n++){x=x*1664525u+1013904223u;}values[i.x]=x;}".into()),
    });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("real timestamp work"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("timestamp workload data"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: data.as_entire_binding(),
        }],
    });
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent completed-work oracle"),
        size: 4096,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut abandoned = gpu.device.create_command_encoder(&Default::default());
    assert!(capture.begin_frame(&mut abandoned, gpu.queue.get_timestamp_period()));
    drop(abandoned);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(capture.begin_frame(&mut encoder, gpu.queue.get_timestamp_period()));
    timer.begin();
    for phase in 0..3 {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("complete interval includes this actual work"),
            timestamp_writes: (phase == 1).then(|| timer.compute(0)).flatten(),
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(if phase == 1 { 1 } else { 16 }, 1, 1);
    }
    timer.finish(&mut encoder);
    encoder.copy_buffer_to_buffer(&data, 0, &output, 0, 4096);
    let (buffer, period) = timer.timestamp_buffer().unwrap();
    let ticket = capture
        .encode_sources(
            [TimingSource {
                buffer,
                period,
                kinds: [0; 32],
                count: 1,
            }],
            &mut encoder,
            1,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    capture.submitted(ticket);
    let mut recorder = FlightRecorder::new(8, 8).unwrap();
    recorder
        .record_frame(Sample {
            frame: 1,
            monotonic_ns: 1,
            ..Default::default()
        })
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    output
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = output.slice(..).get_mapped_range();
    let advance = |mut x: u32| {
        for _ in 0..256 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        }
        x
    };
    for index in [0, 63, 64, 1023] {
        let actual = u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
        let expected = if index < 64 {
            advance(advance(advance(0)))
        } else {
            advance(advance(0))
        };
        assert_eq!(actual, expected, "actual ordered workload at {index}");
    }
    drop(bytes);
    output.unmap();
    capture.collect(&mut recorder);
    let frame = recorder.frames().last().unwrap();
    assert!(frame.gpu_total_complete);
    assert!(frame.gpu_ns.unwrap() > frame.gpu_passes_ns[0].unwrap());
    assert_eq!(frame.gpu_passes_ns.iter().flatten().count(), 1);
    assert_eq!(capture.stats().whole_intervals, 1);
    assert!(
        recorder
            .export_json("test", 0, 0, 0)
            .contains("\"gpuTotalComplete\":true")
    );
    // Reusing the collector without a begin must not expose the old full span.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(capture.encode_sources([], &mut encoder, 2).is_none());
    assert_eq!(capture.pending(), 0);
    assert!(!capture.begin_frame(&mut encoder, f32::NAN));
    assert!(capture.encode_sources([], &mut encoder, 3).is_none());
    println!(
        "COMPLETE_GPU_INTERVAL_OK total={}ns classified={}ns unclassified_work=2passes exact_outputs=true",
        frame.gpu_ns.unwrap(),
        frame.gpu_passes_ns[0].unwrap()
    );
}
