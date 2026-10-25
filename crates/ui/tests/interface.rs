use app::{Focus, Intent, Modifiers, Role, Screen, Snapshot, UiApp, Widget};
use paint::PaintList;
pub use ui::{app, atlas, bindings, font, gpu, layer, paint, profiler, settings};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn click(app: &mut UiApp, widget: Widget) {
    let rect = app.hit(widget).expect("widget visible");
    app.click(
        rect.x + rect.w * 0.5,
        rect.y + rect.h * 0.5,
        false,
        Modifiers::default(),
    );
}
fn drain(app: &mut UiApp) -> Vec<Intent> {
    std::iter::from_fn(|| app.take_intent()).collect()
}
#[test]
fn bot_menu_settings_and_presets_keep_sound_choices() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    let mut paint = PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::Settings);
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::Section(settings::Section::Sound));
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::Setting(settings::Setting::Master));
    assert_eq!(ui.settings.get(settings::Setting::Master), 0);
    click(&mut ui, Widget::Section(settings::Section::Graphics));
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::Preset(settings::Preset::Heavy));
    assert_eq!(ui.settings.get(settings::Setting::Horizon), 1024);
    assert_eq!(ui.settings.get(settings::Setting::Master), 0);
    assert_eq!(ui.settings.get(settings::Setting::Msaa), 4);
    click(&mut ui, Widget::Back);
    assert_eq!(ui.screen, Screen::Welcome);
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::Play);
    assert!(
        drain(&mut ui)
            .into_iter()
            .any(|i| matches!(i, Intent::Connect { .. }))
    );
}
#[test]
fn inventory_click_is_guarded_intent_and_cannot_grant_creative_items() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    let mut model = Snapshot {
        connected: true,
        inventory_revision: 17,
        role: Role::Player,
        creative: true,
        ..Snapshot::default()
    };
    model.inventory[0] = rules::stack::ItemStack::new(rules::ids::STONE, 3).unwrap();
    ui.set_snapshot(model).unwrap();
    ui.open(Screen::Inventory);
    let mut paint = PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    drain(&mut ui);
    click(&mut ui, Widget::Slot(0));
    assert_eq!(ui.snapshot.inventory[0].count(), 3);
    assert!(matches!(
        ui.take_intent(),
        Some(Intent::SlotClick {
            inventory_revision: 17,
            slot: 0,
            ..
        })
    ));
    click(&mut ui, Widget::Nei(rules::ids::STONE));
    assert_eq!(ui.screen, Screen::Recipe);
    assert!(
        !drain(&mut ui)
            .into_iter()
            .any(|i| matches!(i, Intent::CreativeGive { .. }))
    );
}
#[test]
fn unicode_search_recipe_keys_chat_and_binding_conflicts_work() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    ui.set_snapshot(Snapshot {
        connected: true,
        role: Role::Owner,
        ..Snapshot::default()
    })
    .unwrap();
    ui.open(Screen::Inventory);
    let mut paint = PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    click(&mut ui, Widget::NeiSearch);
    ui.text("желез");
    assert!(!ui.filtered_items().is_empty());
    assert!(ui.filtered_items().iter().all(|id| {
        let item = rules::item(*id).unwrap();
        item.name.to_lowercase().contains("желез")
    }));
    ui.key("Escape", true, Modifiers::default());
    assert_eq!(ui.screen, Screen::Game);
    ui.key("Slash", true, Modifiers::default());
    ui.text("tp");
    ui.key("Tab", true, Modifiers::default());
    assert_eq!(ui.chat_input, "/tp ");
    ui.key("Enter", true, Modifiers::default());
    assert!(drain(&mut ui).contains(&Intent::Chat("/tp ".into())));
    let mut keys = bindings::Bindings::default();
    assert_eq!(
        keys.rebind(bindings::Action::Forward, "KeyS"),
        Err(bindings::BindingError::Conflict(bindings::Action::Back))
    );
    assert_eq!(keys.code(bindings::Action::Forward), "KeyW");
    keys.swap(bindings::Action::Forward, bindings::Action::Back)
        .unwrap();
    assert_eq!(keys.code(bindings::Action::Forward), "KeyS");
    let bytes = keys.encode();
    assert_eq!(bindings::Bindings::decode(&bytes).unwrap().encode(), bytes);
    assert!(keys.rebind(bindings::Action::Forward, "Escape").is_err());
}
#[test]
fn static_ui_ten_thousand_frames_allocate_and_build_nothing() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    let mut paint = PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    let before = paint.quads.len();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        assert!(!ui.paint(&atlas, &mut paint, 780, 437));
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(before, paint.quads.len());
    let scope = telemetry::AllocationScope::begin().unwrap();
    std::hint::black_box(vec![1u8; 4096]);
    assert!(scope.finish().allocating_calls() > 0);
}
#[test]
fn furnace_slots_are_guarded_and_recipe_bookmarks_are_visible_and_persisted() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    ui.set_snapshot(Snapshot {
        connected: true,
        container_slots: 3,
        window_id: 42,
        window_revision: 9,
        inventory_revision: 7,
        furnace_fuel: 500,
        machine_progress: 750,
        ..Snapshot::default()
    })
    .unwrap();
    ui.open(Screen::Furnace);
    let mut paint = PaintList::default();
    ui.paint(&atlas, &mut paint, 780, 437);
    let input = ui.hit(Widget::Slot(41)).unwrap();
    let fuel = ui.hit(Widget::Slot(42)).unwrap();
    let output = ui.hit(Widget::Slot(43)).unwrap();
    assert_eq!(input.x, fuel.x);
    assert!(fuel.y > input.y && output.x > input.x);
    click(&mut ui, Widget::Slot(43));
    assert!(drain(&mut ui).iter().any(|intent| matches!(
        intent,
        Intent::SlotClick {
            window: 42,
            revision: 9,
            inventory_revision: 7,
            slot: 43,
            ..
        }
    )));
    let item = ui.filtered_items()[0];
    let rect = ui.hit(Widget::Nei(item)).unwrap();
    ui.pointer(rect.x + 2.0, rect.y + 2.0);
    ui.key("KeyA", true, Modifiers::default());
    ui.paint(&atlas, &mut paint, 780, 437);
    assert!(ui.hit(Widget::Bookmark(item)).is_some());
    let bytes = ui.encode_preferences();
    let mut reloaded = UiApp::new().unwrap();
    reloaded.load_preferences(&bytes).unwrap();
    reloaded.open(Screen::Inventory);
    reloaded.paint(&atlas, &mut paint, 780, 437);
    assert!(reloaded.hit(Widget::Bookmark(item)).is_some());
    assert!(ui.snapshot.inventory.iter().all(|slot| slot.is_empty()));
}
#[test]
fn all_screens_build_bounded_actual_content_and_empty_slots_never_appear_owned() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut ui = UiApp::new().unwrap();
    let mut paint = PaintList::default();
    for screen in [
        Screen::Welcome,
        Screen::Loading,
        Screen::Game,
        Screen::Pause,
        Screen::Settings(settings::Section::Graphics),
        Screen::Settings(settings::Section::Controls),
        Screen::Settings(settings::Section::Sound),
        Screen::Settings(settings::Section::Interface),
        Screen::Inventory,
        Screen::Workbench,
        Screen::Container,
        Screen::Furnace,
        Screen::Machine,
        Screen::Recipe,
        Screen::Resources,
        Screen::Crossroads,
        Screen::Death,
    ] {
        ui.open(screen);
        assert!(ui.paint(&atlas, &mut paint, 780, 437));
        assert!(!paint.quads.is_empty());
        assert!(paint.quads.len() < paint::MAX_UI_QUADS);
        assert_eq!(paint.clipped, 0);
        assert!(ui.snapshot.inventory.iter().all(|stack| stack.is_empty()));
    }
    ui.focus = Focus::Chat;
    ui.text(&"a".repeat(257));
    assert!(ui.chat_input.is_empty());
}
#[test]
fn scale_hit_testing_preferences_and_empty_craft_hover_are_safe() {
    let atlas = atlas::UiAtlas::load().unwrap();
    let mut app = UiApp::new().unwrap();
    let mut list = PaintList::default();
    app.settings.set(settings::Setting::UiScale, 125).unwrap();
    app.open(Screen::Resources);
    app.paint(&atlas, &mut list, 1000, 640);
    click(&mut app, Widget::ImportPack);
    assert!(drain(&mut app).contains(&Intent::ImportPack));
    let bytes = app.encode_preferences();
    let mut restored = UiApp::new().unwrap();
    restored.load_preferences(&bytes).unwrap();
    assert_eq!(restored.encode_preferences(), bytes);
    let before = restored.encode_preferences();
    assert!(
        restored
            .load_preferences(&bytes[..bytes.len() - 1])
            .is_err()
    );
    assert_eq!(restored.encode_preferences(), before);
    app.open(Screen::Inventory);
    app.paint(&atlas, &mut list, 1000, 640);
    let cell = app.hit(Widget::Slot(100)).unwrap();
    app.pointer(cell.x + 5.0, cell.y + 5.0);
    app.paint(&atlas, &mut list, 1000, 640);
    assert!(!list.quads.is_empty());
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn actual_software_gpu_renders_pixel_font_menu_and_retains_static_geometry() {
    pollster::block_on(async {
        let instance = render::instance();
        let graphics = render::Gpu::request(&instance, None, true)
            .await
            .expect("software GPU required");
        let device = &graphics.device;
        let queue = &graphics.queue;
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = gpu::UiRenderer::prepare(device, queue, format, false).unwrap();
        assert_eq!(renderer.take_work().pipeline_compilations, 1);
        let mut app = UiApp::new().unwrap();
        let mut list = PaintList::default();
        app.paint(&renderer.atlas, &mut list, 512, 384);
        assert!(renderer.upload(queue, &list).unwrap());
        let uploaded = renderer.take_work();
        assert!(uploaded.vertex_builds > 300 && uploaded.upload_bytes > 1000);
        for _ in 0..10000 {
            assert!(!renderer.upload(queue, &list).unwrap());
        }
        assert_eq!(renderer.take_work(), gpu::UiWork::default());
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("UI image oracle"),
            size: wgpu::Extent3d {
                width: 512,
                height: 384,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI readback"),
            size: 512 * 384 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        renderer.encode(&mut encoder, &view);
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(512 * 4),
                    rows_per_image: Some(384),
                },
            },
            wgpu::Extent3d {
                width: 512,
                height: 384,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
        device.poll(wgpu::PollType::Wait).unwrap();
        rx.recv().unwrap().unwrap();
        let bytes = readback.slice(..).get_mapped_range();
        let white = bytes
            .chunks_exact(4)
            .filter(|p| p[0] > 200 && p[1] > 190 && p[2] > 170)
            .count();
        assert!(white > 500, "pixel-font/menu text missing");
        assert!(bytes.chunks_exact(4).all(|p| p[3] == 255));
        std::fs::create_dir_all("../../.local/artifacts").unwrap();
        let file = std::fs::File::create("../../.local/artifacts/m11-ui-menu.png").unwrap();
        let mut png = png::Encoder::new(file, 512, 384);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(&bytes)
            .unwrap();
        drop(bytes);
        readback.unmap();
        assert!(device.pop_error_scope().await.is_none());
        println!("UI_GPU_OK glyph_pixels={white} static_uploads=0");
    });
}
