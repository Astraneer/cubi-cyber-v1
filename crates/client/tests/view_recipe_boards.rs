#![cfg(not(target_arch = "wasm32"))]
use client::{ClientScene, SceneConfig, native_pool, view::ClientView};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};
fn cells(page: PageId, wall: PageId) -> Box<[Cell; 4096]> {
    let mut cells = Box::new([Cell::AIR; 4096]);
    if page == wall {
        for x in -12..=-9 {
            for y in 2..=3 {
                let p = BlockPos {
                    space: SpaceId(0),
                    x,
                    y,
                    z: 3,
                };
                cells[usize::from(p.local_index().get())] = Cell::from_parts(rules::ids::STONE, 0);
            }
        }
    }
    cells
}
fn raw(view: &mut ClientView, page: PageId, wall: PageId, version: u64) {
    view.scene
        .import_authoritative_page(
            &net::PageUpdate::Snapshot(net::PageSnapshot {
                page,
                version,
                source_version: Version(version),
                applied_action: Default::default(),
                cells: cells(page, wall),
            }),
            Tick(0),
        )
        .unwrap();
}
fn light(view: &mut ClientView, page: PageId, cell_view: u64) {
    view.scene
        .import_authoritative_light(
            &net::light::Update::Snapshot(net::light::Snapshot {
                header: net::light::Header {
                    page,
                    cell_view,
                    version: cell_view,
                    source: Version(cell_view),
                },
                values: Box::new([net::light::pack(0, 0, [15; 3]).unwrap(); 4096]),
            }),
            Tick(0),
        )
        .unwrap();
}
fn frame(gpu: &render::Gpu, view: &mut ClientView, output: &wgpu::TextureView) {
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut e = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut e, output, 64, 64).unwrap();
    gpu.queue.submit([e.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}
fn pixels(gpu: &render::Gpu, output: &wgpu::Texture) -> Vec<u8> {
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16384,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &b,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        output.size(),
    );
    gpu.queue.submit([e.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    b.slice(..).get_mapped_range().to_vec()
}
#[test]
fn public_landmark_cards_revoke_on_light_binding_and_buttons_are_local_once_per_press() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 7 * render::terrain_mesh::MAX_PAGE_QUADS as u32,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let center = BlockPos {
        space: SpaceId(0),
        x: -12,
        y: 2,
        z: 3,
    };
    let wall = center.page();
    let mut view = ClientView::new(
        renderer,
        SceneConfig {
            center,
            horizontal_radius: 0,
            vertical_radius: 0,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    view.prepare_recipe_boards_from_assets(&gpu.device, &gpu.queue)
        .unwrap();
    assert_eq!(
        view.recipe_board_stats().unwrap().allocation_bytes,
        1_770_128
    );
    // Immutable card bindings/target count cannot silently outlive a later
    // lighting/MRT attachment. Reject before allocating either new owner.
    let mut lighting = client::lighting::ClientLightingConfig::default();
    lighting.shadows.resolution = 32;
    assert!(
        view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
            .is_err()
    );
    assert!(
        view.prepare_environment(
            &gpu.device,
            &gpu.queue,
            64,
            64,
            OutputTransfer::Attachment,
            Default::default(),
            &atlas
        )
        .is_err()
    );
    assert_eq!(view.recipe_board_stats().unwrap().startup_pipelines, 1);
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        raw(&mut view, page, wall, 1);
        light(&mut view, page, 1);
    }
    let mut player = sim::Player::new([-11.5, 0., 6.04]);
    player.pos[1] = 2.5 - f64::from(player.eye_height());
    player.teleport(player.pos).unwrap();
    player.yaw = 180.;
    view.install_remote_player(&player, Tick(0)).unwrap();
    view.set_public_clock(Tick(1234), 0.).unwrap();
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = output.create_view(&Default::default());
    let workers = native_pool(2, 8).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !view.is_ready() {
        assert!(Instant::now() < deadline, "scene {:?}", view.stats());
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        frame(&gpu, &mut view, &target);
        std::thread::yield_now();
    }
    frame(&gpu, &mut view, &target);
    let plain = pixels(&gpu, &output);
    assert_eq!(view.recipe_board_stats().unwrap().active_boards, 0);
    view.set_recipe_board_landmark(Some(net::spawn::Landmark {
        session: 7,
        center: BlockPos {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
    }))
    .unwrap();
    frame(&gpu, &mut view, &target);
    let cards = pixels(&gpu, &output);
    let stats = view.recipe_board_stats().unwrap();
    assert_eq!(stats.active_boards, 3);
    assert!(stats.draw_calls > 0);
    assert!(cards.iter().zip(&plain).filter(|(a, b)| a != b).count() > 100);
    view.set_place_key(true, false);
    assert_eq!(view.take_recipe_board_time(), Some(1));
    assert!(!view.remote_input().placing && !view.remote_input().place_pressed);
    assert!(view.sun_clock().unwrap().sun_direction[1] > 0.99);
    view.set_place_key(true, false);
    assert_eq!(view.take_recipe_board_time(), None);
    view.set_place_key(false, false);
    // A cell-view-only replacement preserves the wall mesh but revokes LIGHT.
    raw(&mut view, wall, wall, 2);
    frame(&gpu, &mut view, &target);
    assert_eq!(view.recipe_board_stats().unwrap().active_boards, 0);
    light(&mut view, wall, 2);
    frame(&gpu, &mut view, &target);
    assert_eq!(view.recipe_board_stats().unwrap().active_boards, 3);
    player.teleport([-9.5, player.pos[1], 6.04]).unwrap();
    view.install_remote_player(&player, Tick(0)).unwrap();
    view.set_place_key(true, false);
    assert_eq!(view.take_recipe_board_time(), Some(2));
    assert!(view.sun_clock().unwrap().sun_direction[1] < -0.99);
    view.set_place_key(false, false);
    view.set_recipe_board_landmark(None).unwrap();
    frame(&gpu, &mut view, &target);
    assert_eq!(view.recipe_board_stats().unwrap().active_boards, 0);
    eprintln!(
        "VIEW_RECIPE_BOARDS_OK actual_bytes={} active3=true no_landmark0=true LIGHT_rebind=true local_day_night=true held_remotePlace0=true",
        stats.allocation_bytes
    );
}
