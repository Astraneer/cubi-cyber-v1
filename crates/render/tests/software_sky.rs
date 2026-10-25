#![cfg(not(target_arch = "wasm32"))]

#[test]
fn software_vulkan_renders_shared_msaa_sky() {
    pollster::block_on(async {
        let instance = render::instance();
        // Failure to create a software adapter is a failed test, never a silent skip.
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("install lavapipe and select its Vulkan ICD for software rendering");
        let info = gpu.adapter.get_info();
        assert_eq!(info.device_type, wgpu::DeviceType::Cpu, "{info:?}");
        assert_eq!(render::MSAA_SAMPLES, 4);
        let probe = render::render_probe(&gpu.device, &gpu.queue);
        assert_eq!(probe.setup_work.pipeline_compilations, 1);
        assert_eq!(probe.frame_work, telemetry::WorkCounts::ZERO);
        let (sender, receiver) = std::sync::mpsc::channel();
        probe
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).expect("readback receiver alive");
            });
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU poll");
        receiver
            .recv()
            .expect("map callback")
            .expect("map readback");
        let bytes = probe.buffer.slice(..).get_mapped_range();
        let colors = probe.validate(&bytes).expect("shader matches reference");
        println!(
            "SOFTWARE_SKY_OK adapter={} backend={:?} pixels={} top={:?} bottom={:?} msaa={}",
            info.name,
            info.backend,
            probe.width * probe.height,
            colors.top,
            colors.bottom,
            render::MSAA_SAMPLES,
        );
        drop(bytes);
        probe.buffer.unmap();
    });
}
