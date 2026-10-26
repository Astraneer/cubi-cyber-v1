//! Model receiver energy uses an analytic constant SH field, independently of
//! the separately verified DDA baker. Source voxels still certify known cells.
#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    atmosphere::{AtmosphereConfig, AtmosphereGpu, AtmosphereState},
    block_models::{BlockConfig, BlockInstance, BlockModelsGpu, kind},
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance},
    gi::{GiConfig, GiGpu, GiVoxel, ProbeSample},
    light_volume::PackedLight,
    terrain_gpu::{AtlasImage, AtlasMip, Camera, GpuConfig, TerrainGpu, UploadBudget},
};
const SIZE: u32 = 64;
enum Model<'a> {
    Entity(&'a mut EntityGpu),
    Block(&'a mut BlockModelsGpu),
}
fn frame(gpu: &render::Gpu, model: Model<'_>, camera: Camera) -> Vec<u8> {
    frame_outputs(gpu, model, camera, false).0
}
fn frame_outputs(
    gpu: &render::Gpu,
    mut model: Model<'_>,
    camera: Camera,
    ambient: bool,
) -> (Vec<u8>, Vec<u8>) {
    let texture = |samples| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model GI linear HDR oracle"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | if samples == 1 {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    };
    let msaa = texture(4);
    let output = texture(1);
    let ambient_msaa = ambient.then(|| texture(4));
    let ambient_output = ambient.then(|| texture(1));
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model GI depth"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 4,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("model GI HDR readback"),
        size: u64::from(SIZE * SIZE * 8) * if ambient { 2 } else { 1 },
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    match &mut model {
        Model::Entity(model) => {
            model
                .prepare_frame(&gpu.queue, &mut encoder, camera)
                .unwrap();
        }
        Model::Block(model) => {
            model
                .prepare_frame(&gpu.queue, &mut encoder, camera)
                .unwrap();
        }
    }
    {
        let mv = msaa.create_view(&Default::default());
        let ov = output.create_view(&Default::default());
        let dv = depth.create_view(&Default::default());
        let av = ambient_msaa
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        let ar = ambient_output
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        let mut attachments = vec![Some(wgpu::RenderPassColorAttachment {
            view: &mv,
            depth_slice: None,
            resolve_target: Some(&ov),
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })];
        if let (Some(av), Some(ar)) = (&av, &ar) {
            attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: av,
                depth_slice: None,
                resolve_target: Some(ar),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            }));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("independent model GI surface draw"),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        match &model {
            Model::Entity(model) => {
                model.draw(&mut pass);
            }
            Model::Block(model) => {
                model.draw(&mut pass);
            }
        }
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 8),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    if let Some(output) = &ambient_output {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: u64::from(SIZE * SIZE * 8),
                    bytes_per_row: Some(SIZE * 8),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
    }
    gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = read.slice(..).get_mapped_range().to_vec();
    read.unmap();
    if ambient {
        let split = (SIZE * SIZE * 8) as usize;
        (bytes[..split].to_vec(), bytes[split..].to_vec())
    } else {
        (bytes, Vec::new())
    }
}
fn half(bytes: &[u8], pixel: usize, channel: usize) -> f32 {
    let at = pixel * 8 + channel * 2;
    let bits = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let sign = if bits & 32768 != 0 { -1. } else { 1. };
    let exponent = (bits >> 10) & 31;
    let mantissa = f32::from(bits & 1023);
    if exponent == 0 {
        sign * mantissa * 2f32.powi(-24)
    } else if exponent == 31 {
        f32::NAN
    } else {
        sign * (1. + mantissa / 1024.) * 2f32.powi(i32::from(exponent) - 15)
    }
}
fn energy(reference: &[u8], actual: &[u8]) -> (usize, f32) {
    let mut checked = 0;
    let mut error = 0f32;
    for pixel in 0..(SIZE * SIZE) as usize {
        if half(reference, pixel, 3) < 0.999 {
            continue;
        }
        checked += 1;
        for (channel, term) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            error = error
                .max((half(actual, pixel, channel) - half(reference, pixel, channel) * term).abs());
        }
    }
    assert!(
        checked > 50,
        "the oracle must rasterize an actual visible model"
    );
    assert!(
        error < 0.001,
        "albedo*reflected irradiance/PI error {error}"
    );
    (checked, error)
}
fn dark(actual: &[u8]) {
    for pixel in 0..(SIZE * SIZE) as usize {
        for channel in 0..3 {
            assert_eq!(
                half(actual, pixel, channel),
                0.,
                "uninitialized or Unknown receiver must not acquire hidden sky/GI"
            );
        }
    }
}
#[test]
fn entity_and_block_receivers_share_lambert_units_preserve_no_gi_and_reject_unknown() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    assert_eq!(
        gpu.device.limits().max_storage_buffers_per_shader_stage,
        8,
        "test the portable minimum, not adapter-specific excess bindings"
    );
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 186,
        mips: vec![AtlasMip {
            size: 1,
            rgba: [128, 192, 64, 255].repeat(186),
            tint_mask: vec![0; 186],
        }],
    };
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        SIZE,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 1,
            max_quads: 16,
            occlusion: false,
        },
        &atlas,
    )
    .unwrap();
    let mut budget = UploadBudget::new(2 * 1024 * 1024);
    let mut atmosphere = AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        AtmosphereConfig {
            transmittance_size: [16, 8],
            sky_size: [16, 8],
            transmittance_steps: 8,
            sky_steps: 8,
        },
        AtmosphereState {
            sun_direction: [0., 1., 0.],
            altitude_km: 0.,
        },
        &mut budget,
    )
    .unwrap();
    // The real daytime atmosphere is deliberately bright. Zero packed
    // skylight must exclude both sky and direct sunlight, not reflected GI.
    let mut setup = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut setup, &mut 1_000_000)
        .unwrap();
    let sky_read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("model oracle verifies real nonzero solar irradiance"),
        size: 80,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    setup.copy_buffer_to_buffer(atmosphere.lighting_buffer(), 0, &sky_read, 0, 80);
    gpu.queue.submit([setup.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    sky_read
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let sky = sky_read.slice(..).get_mapped_range();
    assert!(f32::from_le_bytes(sky[48..52].try_into().unwrap()) > 1.);
    drop(sky);
    sky_read.unmap();
    let mut gi = GiGpu::new(
        &gpu.device,
        &gpu.queue,
        GiConfig {
            world_origin_page: [0; 3],
            world_pages: [1; 3],
            probe_origin: [6.5; 3],
            probe_counts: [2; 3],
            max_jobs: 1,
            ..Default::default()
        },
        &mut budget,
    )
    .unwrap();
    gi.upload_page(
        &gpu.queue,
        [0; 3],
        1,
        &[GiVoxel::EMPTY; 4096],
        0,
        &mut budget,
    )
    .unwrap();
    let probes: Vec<_> = (0..gi.probe_count())
        .map(|slot| ProbeSample {
            position: [
                gi.probe_position(slot).unwrap()[0],
                gi.probe_position(slot).unwrap()[1],
                gi.probe_position(slot).unwrap()[2],
                1.,
            ],
            sh_r: [0.25 * std::f32::consts::PI / 0.282_094_8, 0., 0., 0.],
            sh_g: [0.5 * std::f32::consts::PI / 0.282_094_8, 0., 0., 0.],
            sh_b: [0.75 * std::f32::consts::PI / 0.282_094_8, 0., 0., 0.],
            moments: [0.; 4],
            visibility_a: [32.; 4],
            visibility_b: [32.; 4],
            stamps: [1, 1, 1, 32],
        })
        .collect();
    gpu.queue
        .write_buffer(gi.probe_buffer(), 0, bytemuck::cast_slice(&probes));
    let camera = Camera::perspective(
        [8., 9.2, 12.],
        [8., 8.4, 8.],
        [0., 1., 0.],
        1.,
        1.,
        0.1,
        100.,
    );
    let shadows = render::shadows::ShadowGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        render::shadows::ShadowConfig {
            resolution: 32,
            dynamic_casters: true,
            ..Default::default()
        },
        [8.; 3],
        [0., 1., 0.],
    )
    .unwrap();
    let config = EntityConfig {
        max_entities: 1,
        atlas_layers: 186,
    };
    let mut old = EntityGpu::new_with_environment(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        None,
        None,
        &atmosphere,
    )
    .unwrap();
    let mut disabled = EntityGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        None,
        None,
        &atmosphere,
        None,
    )
    .unwrap();
    let mut entity = EntityGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        Some(&shadows),
        None,
        &atmosphere,
        Some(&gi),
    )
    .unwrap();
    let instance = EntityInstance {
        position: [8.; 3],
        body_yaw: 0.,
        head_yaw: 0.,
        pitch: 0.,
        limb_phase: 0.,
        limb_amount: 0.,
        kind: 6,
        flags: render::entity_gpu::PIPE | 32,
        color: 0,
        id_low: 1,
        id_high: 0,
        death_roll: 0.,
        scale: 0.8,
        reserved: 0.,
    };
    let zero = PackedLight::new(0, 0, [0; 3]).unwrap();
    let full = PackedLight::new(0, 0, [15; 3]).unwrap();
    for receiver in [&mut old, &mut disabled, &mut entity] {
        receiver
            .upload(&gpu.queue, &[instance], &[zero], &mut budget)
            .unwrap();
    }
    let before = frame(&gpu, Model::Entity(&mut old), camera);
    dark(&before);
    assert_eq!(
        frame(&gpu, Model::Entity(&mut disabled), camera),
        before,
        "NoGi must remain bitwise identical"
    );
    old.upload(&gpu.queue, &[instance], &[full], &mut budget)
        .unwrap();
    disabled
        .upload(&gpu.queue, &[instance], &[full], &mut budget)
        .unwrap();
    let reference = frame(&gpu, Model::Entity(&mut old), camera);
    assert_eq!(
        frame(&gpu, Model::Entity(&mut disabled), camera),
        reference,
        "positive colored NoGi pixels must also remain bitwise identical"
    );
    let entity_gi = frame(&gpu, Model::Entity(&mut entity), camera);
    let (entity_pixels, entity_error) = energy(&reference, &entity_gi);
    let config = BlockConfig {
        max_instances: 1,
        atlas_layers: 186,
    };
    let mut old_block = BlockModelsGpu::new_with_atmosphere(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        None,
        &atmosphere,
    )
    .unwrap();
    let mut disabled_block = BlockModelsGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        None,
        &atmosphere,
        None,
    )
    .unwrap();
    let mut block = BlockModelsGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        Some(&shadows),
        &atmosphere,
        Some(&gi),
    )
    .unwrap();
    let item = BlockInstance {
        position: [8.; 3],
        kind: kind::CUBE_MACHINE,
        light: zero.raw(),
        amount: 0.,
        tint: 0xff40ff80,
        ..Default::default()
    };
    for receiver in [&mut old_block, &mut disabled_block, &mut block] {
        receiver.upload(&gpu.queue, &[item], &mut budget).unwrap();
    }
    let before_block = frame(&gpu, Model::Block(&mut old_block), camera);
    dark(&before_block);
    assert_eq!(
        frame(&gpu, Model::Block(&mut disabled_block), camera),
        before_block
    );
    old_block
        .upload(
            &gpu.queue,
            &[BlockInstance {
                light: full.raw(),
                ..item
            }],
            &mut budget,
        )
        .unwrap();
    disabled_block
        .upload(
            &gpu.queue,
            &[BlockInstance {
                light: full.raw(),
                ..item
            }],
            &mut budget,
        )
        .unwrap();
    let reference_block = frame(&gpu, Model::Block(&mut old_block), camera);
    assert_eq!(
        frame(&gpu, Model::Block(&mut disabled_block), camera),
        reference_block
    );
    let block_gi = frame(&gpu, Model::Block(&mut block), camera);
    let (block_pixels, block_error) = energy(&reference_block, &block_gi);
    // Keep valid, bright cached probes, but revoke the actual receiver's source
    // certificate. Without the voxel guard both images would remain colored.
    gi.upload_page(
        &gpu.queue,
        [0; 3],
        2,
        &[GiVoxel::UNKNOWN; 4096],
        1,
        &mut budget,
    )
    .unwrap();
    dark(&frame(&gpu, Model::Entity(&mut entity), camera));
    dark(&frame(&gpu, Model::Block(&mut block), camera));
    gi.upload_page(
        &gpu.queue,
        [0; 3],
        3,
        &[GiVoxel::EMPTY; 4096],
        2,
        &mut budget,
    )
    .unwrap();
    assert_eq!(
        frame(&gpu, Model::Entity(&mut entity), camera),
        entity_gi,
        "GI-only updates never require new model vertices/instances"
    );
    assert_eq!(frame(&gpu, Model::Block(&mut block), camera), block_gi);
    // The optional MRT must preserve first-output energy while publishing
    // only the removable indirect term, using the same real GI attachments.
    terrain.prepare_ambient_mrt(&gpu.device).unwrap();
    let mut mrt_entity = EntityGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        EntityConfig {
            max_entities: 1,
            atlas_layers: 186,
        },
        Some(&shadows),
        None,
        &atmosphere,
        Some(&gi),
    )
    .unwrap();
    mrt_entity
        .upload(&gpu.queue, &[instance], &[zero], &mut budget)
        .unwrap();
    let (mrt_color, mrt_delta) = frame_outputs(&gpu, Model::Entity(&mut mrt_entity), camera, true);
    assert_eq!(
        mrt_color, entity_gi,
        "MRT retains actual entity HDR first output"
    );
    let mut mrt_block = BlockModelsGpu::new_with_lighting(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        config,
        Some(&shadows),
        &atmosphere,
        Some(&gi),
    )
    .unwrap();
    mrt_block.upload(&gpu.queue, &[item], &mut budget).unwrap();
    let (mrt_block_color, mrt_block_delta) =
        frame_outputs(&gpu, Model::Block(&mut mrt_block), camera, true);
    assert_eq!(
        mrt_block_color, block_gi,
        "MRT retains actual block HDR first output"
    );
    for (color, delta) in [
        (&mrt_color, &mrt_delta),
        (&mrt_block_color, &mrt_block_delta),
    ] {
        for pixel in 0..(SIZE * SIZE) as usize {
            for channel in 0..3 {
                assert!(
                    (half(color, pixel, channel) - half(delta, pixel, channel)).abs() < 0.00002,
                    "with Sky/RGB0 all model energy is removable GI"
                );
            }
        }
    }
    eprintln!(
        "MODEL_GI_RECEIVER_GPU_OK entity_pixels={entity_pixels} block_pixels={block_pixels} entity_error={entity_error} block_error={block_error} NoGi_BITWISE=true Unknown_dark=true sky0_excludes_bright_atmo=true portable_storage_limit=8"
    );
}
