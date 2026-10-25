#![cfg(not(target_arch = "wasm32"))]

use render::atmosphere::{
    AtmosphereConfig, AtmosphereError, AtmosphereGpu, AtmosphereState, reference_transmittance,
};
use render::terrain_gpu::UploadBudget;

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = (bits >> 10) & 31;
    let fraction = f32::from(bits & 1023) / 1024.0;
    match exponent {
        0 => sign * fraction * 2.0f32.powi(-14),
        31 => {
            if fraction == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + fraction) * 2.0f32.powi(i32::from(exponent) - 15),
    }
}
fn pixels(gpu: &render::Gpu, texture: &wgpu::Texture, size: [u32; 2]) -> Vec<[f32; 4]> {
    let row = (size[0] * 8).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atmosphere numeric readback"),
        size: u64::from(row) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let pixels = mapped
        .chunks_exact(row as usize)
        .flat_map(|line| {
            line[..size[0] as usize * 8].chunks_exact(8).map(|pixel| {
                std::array::from_fn(|channel| {
                    half(u16::from_le_bytes(
                        pixel[channel * 2..channel * 2 + 2].try_into().unwrap(),
                    ))
                })
            })
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    pixels
}

#[test]
fn atmosphere_luts_match_numeric_optics_and_budget_updates() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let config = AtmosphereConfig {
            transmittance_size: [64, 32],
            sky_size: [48, 32],
            transmittance_steps: 64,
            sky_steps: 48,
        };
        let state = AtmosphereState::default();
        let mut denied = UploadBudget::new(0);
        assert!(matches!(
            AtmosphereGpu::new(&gpu.device, &gpu.queue, config, state, &mut denied),
            Err(AtmosphereError::UploadBudget)
        ));
        let mut budget = UploadBudget::new(1024);
        let mut atmosphere =
            AtmosphereGpu::new(&gpu.device, &gpu.queue, config, state, &mut budget).unwrap();
        assert_eq!(1024 - budget.remaining_bytes, 96);
        let _views = (atmosphere.transmittance_view(), atmosphere.sky_view());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(matches!(
            atmosphere.encode_update(&mut encoder, &mut 0),
            Err(AtmosphereError::WorkBudget)
        ));
        assert_eq!(atmosphere.pending_work().passes, 2);
        let mut work_budget = 1_000_000;
        let work = atmosphere
            .encode_update(&mut encoder, &mut work_budget)
            .unwrap();
        assert_eq!(work.passes, 2);
        assert_eq!(work.lut_pixels, 64 * 32 + 48 * 32);
        gpu.queue.submit([encoder.finish()]);
        let trans = pixels(
            &gpu,
            atmosphere.transmittance_texture(),
            config.transmittance_size,
        );
        let sky = pixels(&gpu, atmosphere.sky_texture(), config.sky_size);
        assert!(
            trans
                .iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        );
        assert!(sky.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
        assert!(
            sky.iter().any(|p| p[0] > 0.01 && p[2] > p[0]),
            "physical daylight sky is absent"
        );
        let mut maximum_error = 0.0f64;
        let mut checked = 0;
        for y in (0..32).step_by(3) {
            for x in (0..64).step_by(3) {
                let h = (f64::from(y) + 0.5) / 32.0 * 100.0;
                let mu = (f64::from(x) + 0.5) / 64.0 * 2.0 - 1.0;
                let expected = reference_transmittance(h, mu, 4096);
                let actual = trans[(x + 64 * y) as usize];
                for channel in 0..3 {
                    maximum_error =
                        maximum_error.max((f64::from(actual[channel]) - expected[channel]).abs());
                }
                checked += 1;
            }
        }
        assert!(
            maximum_error < 0.006,
            "GPU optical-depth integration differs from f64/4096-step oracle: {maximum_error}"
        );
        assert_eq!(atmosphere.pending_work().integration_steps, 0);
        atmosphere
            .set_state(&gpu.queue, state, &mut budget)
            .unwrap();
        assert_eq!(budget.remaining_bytes, 928);
        let night = AtmosphereState {
            sun_direction: [0.0, -1.0, 0.0],
            ..state
        };
        assert_eq!(
            atmosphere.set_state(&gpu.queue, night, &mut denied),
            Err(AtmosphereError::UploadBudget)
        );
        assert_eq!(atmosphere.pending_work().passes, 0);
        atmosphere
            .set_state(&gpu.queue, night, &mut budget)
            .unwrap();
        assert_eq!(atmosphere.pending_work().passes, 1);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let night_work = atmosphere
            .encode_update(&mut encoder, &mut work_budget)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        let night_sky = pixels(&gpu, atmosphere.sky_texture(), config.sky_size);
        assert!(
            night_sky.iter().all(|p| p[..3].iter().all(|v| *v == 0.0)),
            "sunlight leaked through the solid planet"
        );
        assert_eq!(
            pixels(
                &gpu,
                atmosphere.transmittance_texture(),
                config.transmittance_size
            ),
            trans,
            "sun motion rebuilt or corrupted static transmittance"
        );
        assert_eq!(night_work.integration_steps, 48 * 32 * 48);
        println!(
            "ATMOSPHERE_GPU_OK checked_optics={checked} max_abs_error={maximum_error:.8} initial_steps={} sun_change_steps={} static_steps=0 planet_night_leaks=0",
            work.integration_steps, night_work.integration_steps
        );
    });
}
