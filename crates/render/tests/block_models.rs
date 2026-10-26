#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    block_models::{BlockConfig, BlockError, BlockInstance, BlockModelsGpu, MODEL_KINDS, kind},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn terrain(gpu: &render::Gpu) -> TerrainGpu {
    TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        256,
        192,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 8192,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap()
}
fn camera() -> Camera {
    Camera::perspective(
        [3., 9., 22.],
        [0., 1.7, -3.],
        [0., 1., 0.],
        1.,
        256. / 192.,
        0.1,
        200.,
    )
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
fn read_model(gpu: &render::Gpu, entities: &BlockModelsGpu) -> Vec<u8> {
    let size = entities.model_vertex_buffer().size();
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("immutable entity vertex catalog oracle"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(entities.model_vertex_buffer(), 0, &buffer, 0, size);
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    entities: Option<&mut BlockModelsGpu>,
    camera: Camera,
) -> (Vec<u8>, [u32; MODEL_KINDS]) {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual instanced mob HDR composite"),
        size: wgpu::Extent3d {
            width: 256,
            height: 192,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = output.create_view(&Default::default());
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity scene image oracle"),
        size: 256 * 192 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let counts = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual entity indirect counts"),
        size: MODEL_KINDS as u64 * 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some(entities) = entities {
        entities
            .prepare_frame(&gpu.queue, &mut encoder, camera)
            .unwrap();
        let work = terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
                |pass| entities.draw(pass),
            )
            .unwrap();
        assert!(work.draw_calls >= 1);
        encoder.copy_buffer_to_buffer(
            entities.indirect_buffer(),
            0,
            &counts,
            0,
            MODEL_KINDS as u64 * 16,
        );
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
            )
            .unwrap();
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(192),
            },
        },
        wgpu::Extent3d {
            width: 256,
            height: 192,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let image = read(gpu, &pixels);
    let counts = read(gpu, &counts);
    (
        image,
        std::array::from_fn(|i| {
            u32::from_le_bytes(counts[i * 16 + 4..i * 16 + 8].try_into().unwrap())
        }),
    )
}
fn prop(kind: u32) -> BlockInstance {
    BlockInstance {
        kind,
        position: [(kind % 6) as f32 * 2.5 - 6.25, 0., (kind / 6) as f32 * -4.],
        layer: match kind {
            kind::SNOW => 30,
            kind::CROSS => 78,
            _ => 0,
        },
        connections: 0x555,
        reserved: [3 | (3 << 16); 4],
        ..BlockInstance::default()
    }
}
#[test]
fn actual_static_block_models_gpu_state_changes_and_budget_are_atomic() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 1024,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let startup = models.take_work();
        assert_eq!(startup.startup_vertices, 7512);
        assert_eq!(startup.pipeline_compilations, 3);
        let vertices = read_model(&gpu, &models);
        let mut records = (0..MODEL_KINDS as u32).map(prop).collect::<Vec<_>>();
        let mut hidden = prop(kind::CHEST);
        hidden.position = [10000., 0., 0.];
        records.push(hidden);
        let upload = models
            .upload(&gpu.queue, &records, &mut UploadBudget::new(100000))
            .unwrap();
        assert_eq!(upload.upload_bytes, 18 * 64);
        assert_eq!(upload.cpu_vertices, 0);
        let sky = frame(&gpu, &mut terrain, None, camera()).0;
        let (initial, counts) = frame(&gpu, &mut terrain, Some(&mut models), camera());
        assert_eq!(counts, [1; MODEL_KINDS]);
        let colored = initial
            .chunks_exact(4)
            .zip(sky.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(colored > 700, "static block scene missing {colored}");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/artifacts/m7-block-models.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut png = png::Encoder::new(std::fs::File::create(path).unwrap(), 256, 192);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(&initial)
            .unwrap();
        let mut previous = initial;
        for (index, phase, amount, connections) in [
            (kind::CHEST, 1., 1., 0x555),
            (kind::SNOW, 0., 0.125, 0x555),
            (kind::CABLE, 0., 1., 0),
            (kind::WIND_GENERATOR, 0.8, 1., 0x555),
        ] {
            records[index as usize].phase = phase;
            records[index as usize].amount = amount;
            records[index as usize].connections = connections;
            let work = models
                .update(
                    &gpu.queue,
                    index,
                    records[index as usize],
                    &mut UploadBudget::new(64),
                )
                .unwrap();
            assert_eq!(work.upload_bytes, 64);
            assert_eq!(work.cpu_vertices, 0);
            let current = frame(&gpu, &mut terrain, Some(&mut models), camera()).0;
            assert_ne!(current, previous, "model {index} ignored GPU state");
            previous = current;
        }
        assert_eq!(
            read_model(&gpu, &models),
            vertices,
            "cell state edit rebuilt model vertices"
        );
        let mut bad = records[0];
        bad.position[0] += 3.;
        assert_eq!(
            models.update(&gpu.queue, 0, bad, &mut UploadBudget::new(63)),
            Err(BlockError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut models), camera()).0,
            previous
        );
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            assert_eq!(
                models
                    .upload(&gpu.queue, &records, &mut UploadBudget::new(0))
                    .unwrap()
                    .upload_bytes,
                0
            );
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        println!(
            "M7 props17 startup7512vertices scene{colored}px edits4*64B idle100uploads0allocations0"
        );
    });
}
#[test]
fn every_catalog_kind_draws_actual_pixels_in_isolation() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 1,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let mut total = 0;
        for kind in 0..MODEL_KINDS as u32 {
            let mut record = prop(kind);
            record.position = [0.; 3];
            let c = if kind == kind::WIND_GENERATOR {
                Camera::perspective(
                    [4., 6., 12.],
                    [0., 3., 0.],
                    [0., 1., 0.],
                    1.,
                    256. / 192.,
                    0.1,
                    200.,
                )
            } else {
                Camera::perspective(
                    [2., 2.2, 4.],
                    [0., 0.6, 0.],
                    [0., 1., 0.],
                    1.,
                    256. / 192.,
                    0.1,
                    200.,
                )
            };
            models
                .upload(&gpu.queue, &[record], &mut UploadBudget::new(64))
                .unwrap();
            let sky = frame(&gpu, &mut terrain, None, c).0;
            let (image, counts) = frame(&gpu, &mut terrain, Some(&mut models), c);
            assert_eq!(counts.iter().sum::<u32>(), 1);
            assert_eq!(counts[kind as usize], 1);
            let pixels = image
                .chunks_exact(4)
                .zip(sky.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                pixels > 10,
                "kind {kind} produced no usable model pixels {pixels}"
            );
            total += pixels;
        }
        println!("M7 isolated props17 actual shaded pixels={total}");
    });
}

fn shadow_depths(gpu: &render::Gpu, texture: &wgpu::Texture, resolution: u32) -> Vec<f32> {
    let row = (resolution * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent model shadow depth image"),
        size: u64::from(row) * u64::from(resolution) * 3,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(resolution),
            },
        },
        wgpu::Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: 3,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
        .chunks_exact(row as usize)
        .flat_map(|row| {
            row[..resolution as usize * 4]
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        })
        .collect()
}
#[test]
fn block_and_entity_shadow_pair_preserves_both_exact_version_counters() {
    pollster::block_on(async {
        use render::{
            entity_gpu::{EntityConfig, EntityGpu, EntityInstance},
            shadows::{ShadowCasterPair, ShadowConfig, ShadowGpu},
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let terrain = terrain(&gpu);
        let mut shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 128,
                half_widths: [8., 16., 32.],
                depth_half_extent: 32.,
                max_updates_per_frame: 3,
                dynamic_casters: true,
                ..ShadowConfig::default()
            },
            [0.; 3],
            [0., 1., 0.],
        )
        .unwrap();
        let mut props = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 1,
                atlas_layers: 186,
            },
            Some(&shadows),
        )
        .unwrap();
        let mut mobs = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1,
                atlas_layers: 186,
            },
            Some(&shadows),
        )
        .unwrap();
        let mut chest = prop(kind::CHEST);
        chest.position = [-2., 0., 0.];
        props
            .upload(&gpu.queue, &[chest], &mut UploadBudget::new(64))
            .unwrap();
        let mob = EntityInstance {
            position: [2., 0., 0.],
            body_yaw: 0.,
            head_yaw: 0.,
            pitch: 0.,
            limb_phase: 0.,
            limb_amount: 0.,
            kind: 0,
            flags: 0,
            color: 0,
            id_low: 1,
            id_high: 0,
            death_roll: 0.,
            scale: 1.,
            reserved: 0.,
        };
        mobs.upload(&gpu.queue, &[mob], &[], &mut UploadBudget::new(68))
            .unwrap();
        let encode = |shadows: &mut ShadowGpu, props: &mut BlockModelsGpu, mobs: &mut EntityGpu| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = shadows
                .encode_dynamic(
                    &gpu.queue,
                    &mut encoder,
                    &mut ShadowCasterPair {
                        first: mobs,
                        second: props,
                    },
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            work
        };
        let first = encode(&mut shadows, &mut props, &mut mobs);
        assert_eq!(first.dynamic_draw_calls, 27);
        let initial = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert!(initial.iter().filter(|v| **v > 0.).count() > 15);
        assert_eq!(
            encode(&mut shadows, &mut props, &mut mobs).dynamic_copy_bytes,
            0
        );
        chest.phase = 1.;
        props
            .update(&gpu.queue, 0, chest, &mut UploadBudget::new(64))
            .unwrap();
        let localized = encode(&mut shadows, &mut props, &mut mobs);
        assert_eq!(localized.dynamic_copy_bytes, 0);
        assert!(
            localized.dynamic_restore_pixels > 0
                && localized.dynamic_restore_pixels < 3 * 128 * 128 / 4
        );
        let opened = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert_ne!(opened, initial);
        shadows.invalidate_all();
        encode(&mut shadows, &mut props, &mut mobs);
        assert_eq!(
            shadow_depths(&gpu, shadows.sampled_texture(), 128),
            opened,
            "fully open chest damage differed from full shadow oracle"
        );
        assert_eq!(
            encode(&mut shadows, &mut props, &mut mobs).dynamic_copy_bytes,
            0
        );
        props
            .upload(&gpu.queue, &[], &mut UploadBudget::new(0))
            .unwrap();
        let localized = encode(&mut shadows, &mut props, &mut mobs);
        assert_eq!(localized.dynamic_copy_bytes, 0);
        assert!(
            localized.dynamic_restore_pixels > 0
                && localized.dynamic_restore_pixels < 3 * 128 * 128 / 4
        );
        let mob_only = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert_ne!(mob_only, opened);
        assert!(mob_only.iter().any(|v| *v > 0.));
        assert!(
            shadow_depths(&gpu, shadows.texture(), 128)
                .iter()
                .all(|v| *v == 0.)
        );
        println!(
            "M7 pair mob+chest draws27/localrestore/copy0; static cache untouched; openchest equalsfullBITWISE; removal visible; unchangedframe0work"
        );
    });
}

#[test]
fn stable_reserved_slots_hide_and_reuse_one_record_without_changing_neighbors() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            render::block_models::BlockConfig {
                max_instances: 8,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        assert!(
            models
                .reserve_slots(&gpu.queue, 8, &mut UploadBudget::new(511))
                .is_err()
        );
        assert_eq!(models.reserved_slots(), 0);
        assert_eq!(
            models
                .reserve_slots(&gpu.queue, 8, &mut UploadBudget::new(512))
                .unwrap()
                .upload_bytes,
            512
        );
        assert_eq!(models.active_instances(), 0);
        let mut a = prop(kind::CHEST);
        a.position = [0., 0., 0.];
        let mut b = prop(kind::WIND_GENERATOR);
        b.position = [3., 0., 0.];
        models
            .update(&gpu.queue, 2, a, &mut UploadBudget::new(64))
            .unwrap();
        models
            .update(&gpu.queue, 7, b, &mut UploadBudget::new(64))
            .unwrap();
        let before = frame(&gpu, &mut terrain, Some(&mut models), camera());
        assert_eq!(before.1[kind::CHEST as usize], 1);
        assert_eq!(before.1[kind::WIND_GENERATOR as usize], 1);
        models.take_work();
        assert!(
            models
                .update(
                    &gpu.queue,
                    2,
                    BlockInstance::hidden(),
                    &mut UploadBudget::new(63)
                )
                .is_err()
        );
        assert_eq!(models.active_instances(), 2);
        models
            .update(
                &gpu.queue,
                2,
                BlockInstance::hidden(),
                &mut UploadBudget::new(64),
            )
            .unwrap();
        assert_eq!(models.active_instances(), 1);
        let after = frame(&gpu, &mut terrain, Some(&mut models), camera());
        assert_eq!(after.1[kind::CHEST as usize], 0);
        assert_eq!(after.1[kind::WIND_GENERATOR as usize], 1);
        assert_ne!(before.0, after.0);
        models
            .update(&gpu.queue, 2, a, &mut UploadBudget::new(64))
            .unwrap();
        let restored = frame(&gpu, &mut terrain, Some(&mut models), camera());
        assert_eq!(restored.0, before.0);
        assert_eq!(models.take_work().upload_bytes, 128);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            assert_eq!(
                models
                    .update(&gpu.queue, 7, b, &mut UploadBudget::new(0))
                    .unwrap()
                    .upload_bytes,
                0
            );
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
    });
}

#[test]
fn optional_atmosphere_lighting_tracks_day_and_dark_night_without_recompilation() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut terrain = terrain(&gpu);
    let mut budget = UploadBudget::new(1_000_000);
    let mut atmosphere = render::atmosphere::AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        render::atmosphere::AtmosphereConfig {
            transmittance_size: [32, 8],
            sky_size: [16, 8],
            transmittance_steps: 32,
            sky_steps: 16,
        },
        render::atmosphere::AtmosphereState {
            sun_direction: [0., -1., 0.],
            altitude_km: 0.072,
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let mut consumer = BlockModelsGpu::new_with_atmosphere(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        BlockConfig {
            max_instances: 8,
            atlas_layers: 186,
        },
        None,
        &atmosphere,
    )
    .unwrap();
    let mut chest = prop(kind::CHEST);
    chest.position = [0.; 3];
    consumer.upload(&gpu.queue, &[chest], &mut budget).unwrap();
    consumer.take_work();
    let dark = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let dark_pixels = dark
        .chunks_exact(4)
        .filter(|p| p[3] == 255 && p[..3] == [0, 0, 0])
        .count();
    assert!(
        dark_pixels > 50,
        "night receiver must be dark: {dark_pixels}"
    );
    atmosphere
        .set_state(
            &gpu.queue,
            render::atmosphere::AtmosphereState::default(),
            &mut budget,
        )
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let day = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let lit_pixels = day
        .chunks_exact(4)
        .zip(dark.chunks_exact(4))
        .filter(|(a, b)| b[3] == 255 && b[..3] == [0, 0, 0] && a[..3].iter().any(|v| *v > 3))
        .count();
    assert!(
        lit_pixels > 50,
        "day receivers must react to shared atmosphere: {lit_pixels}"
    );
    assert_eq!(
        consumer.take_work().pipeline_compilations,
        0,
        "day change must reuse pipelines"
    );
    println!(
        "M10_block_models_ATMOSPHERE_GPU_OK consumer=block_models night_black={dark_pixels} day_lit={lit_pixels} runtime_pipeline_compilations=0"
    );
}

#[path = "common/ports_live.rs"]
mod ports_live;

#[path = "common/ports_markers.rs"]
mod ports_markers;

#[path = "common/connector_color.rs"]
mod connector_color;
