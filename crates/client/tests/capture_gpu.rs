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
}
