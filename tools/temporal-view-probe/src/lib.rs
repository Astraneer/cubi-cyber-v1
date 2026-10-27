//! Diagnostic-only actual ClientView. CPU jobs execute synchronously through
//! their production codecs; this is not a browser Worker performance claim.
use client::{
    ClientScene, SceneConfig,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    gi::GiConfig,
    terrain_gpu::{AtlasImage, AtlasMip, GpuConfig, TerrainGpu, UploadBudget},
};
use serde_json::{Value, json};
mod boards;
mod far;
const W: u32 = 128;
const H: u32 = 96;

fn atlas() -> AtlasImage {
    AtlasImage {
        tile_size: 1,
        layers: 186,
        mips: vec![AtlasMip {
            size: 1,
            rgba: (0..186)
                .flat_map(|layer| {
                    if layer == rules::ids::L_LEAVES {
                        [32, 180, 40, 128]
                    } else {
                        [110, 130, 150, 255]
                    }
                })
                .collect(),
            tint_mask: vec![255; 186],
        }],
    }
}
fn pump(view: &mut ClientView, gpu: &render::Gpu) -> Result<(), String> {
    for _ in 0..8 {
        let mut budget = ClientScene::default_budget();
        view.scene.phase(Tick(1), 8).map_err(|e| e.to_string())?;
        for _ in 0..4 {
            let Some(task) = view.scene.take_task(Tick(1), &mut budget) else {
                break;
            };
            let bytes = client::execute_task(&task.payload.encode(), || false)?;
            view.scene
                .complete_bytes(task.id, &bytes)
                .map_err(|e| e.to_string())?;
        }
        view.advance_input(0.)?;
        for _ in 0..2 {
            let Some(task) = view.take_props_task() else {
                break;
            };
            let request = task.payload.encode().map_err(|e| format!("{e:?}"))?;
            let result = client::props_source::execute_props_request(&request, || false)
                .map_err(|e| format!("{e:?}"))?;
            view.complete_props_bytes(task.id, &result)?;
        }
        for _ in 0..2 {
            let Some(task) = view.take_water_task()? else {
                break;
            };
            let bytes = view.encode_water_task(&task)?;
            let result = client::water_source::execute_water_request(&bytes, || false)
                .map_err(|e| e.to_string())?;
            view.complete_water_bytes(task.id, &result)?;
        }
        for _ in 0..2 {
            let Some(task) = view.take_gi_source_task()? else {
                break;
            };
            let bytes = view.encode_gi_source_task(&task)?;
            let result = client::gi_source::execute_gi_request(&bytes, || false)
                .map_err(|e| e.to_string())?;
            view.complete_gi_source_bytes(task.id, &result)?;
        }
        for _ in 0..4 {
            let Some(task) = view.take_horizon_task() else {
                break;
            };
            let bytes = client::view::horizon::encode_background_task(&task.payload)
                .map_err(|e| e.to_string())?;
            let result = client::view::horizon::execute_background_request(&bytes, || false)
                .map_err(|e| e.to_string())?;
            view.complete_horizon_bytes(task.id, &result)?;
        }
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))?;
    }
    Ok(())
}
fn build(
    gpu: &render::Gpu,
    temporal: bool,
    full: bool,
    samples: u32,
    committed_far: bool,
) -> Result<ClientView, String> {
    build_inner(gpu, temporal, full, samples, committed_far, false)
}
fn build_inner(
    gpu: &render::Gpu,
    temporal: bool,
    full: bool,
    samples: u32,
    committed_far: bool,
    recipe_boards: bool,
) -> Result<ClientView, String> {
    let atlas = atlas();
    let terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        &atlas,
        samples,
    )
    .map_err(|e| e.to_string())?;
    let mut view = ClientView::new(
        terrain,
        SceneConfig {
            center: BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 1,
                z: 14,
            },
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )?;
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.enabled = full;
    lighting.shadows.dynamic_casters = true;
    lighting.shadows.max_updates_per_frame = 3;
    lighting.shadows.sun_step_radians = 1e-6;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)?;
    let mut profile = EnvironmentProfile::product()
        .with_quality(u8::from(full) * 2, u8::from(full) * 2)
        .and_then(|p| p.with_contact_ao(u8::from(full) * 2))
        .and_then(|p| p.with_reflections(u8::from(full) * 2))
        .map_err(|e| e.to_string())?;
    profile.graphics.atmosphere = AtmosphereConfig {
        transmittance_size: [16, 8],
        sky_size: [16, 8],
        transmittance_steps: 16,
        sky_steps: 16,
    };
    if full {
        profile.graphics.gi = Some(GiConfig {
            world_pages: [3, 3, 3],
            probe_counts: [2, 1, 2],
            max_jobs: 4,
            rays_per_probe: 64,
            ..Default::default()
        });
        profile.graphics.post.bloom.as_mut().unwrap().levels = 6;
    } else {
        profile.graphics.post.exposure = render::post::Exposure::Manual(1.);
        profile.graphics.post.bloom = None;
        profile.graphics.water = None;
        profile.celestials = None;
        profile.aerial = None;
        profile.multiple_scattering = None;
    }
    if temporal {
        profile.temporal = Some(render::taa::TaaConfig {
            statistics: true,
            ..Default::default()
        });
    }
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        profile,
        &atlas,
    )?;
    if full {
        view.prepare_water(Default::default())?;
    }
    view.prepare_entities(&gpu.device, &gpu.queue)?;
    view.prepare_props(
        &gpu.device,
        &gpu.queue,
        client::props_source::PropsLimits {
            max_instances: 16,
            max_pages: 4,
            max_running: 2,
            max_ready: 2,
            max_age_ticks: 40,
        },
    )?;
    if committed_far {
        view.prepare_horizon(
            &gpu.device,
            &gpu.queue,
            client::view::horizon::Config {
                max_tiles: if committed_far { 64 } else { 16 },
                max_sources: if committed_far { 128 } else { 64 },
                max_proxy_pages: if committed_far { 16 } else { 8 },
                horizon_level: if committed_far { 0 } else { 9 },
                ..Default::default()
            },
        )?;
        if full {
            view.prepare_far_water(Default::default())?;
        }
    }
    if recipe_boards {
        view.prepare_recipe_boards_from_assets(&gpu.device, &gpu.queue)?;
    }
    view.resize(&gpu.device, W, H)?;
    if temporal {
        view.prepare_temporal(&gpu.device, &gpu.queue)?;
    }
    let center = view.scene.config().center.page();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if committed_far {
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        cells[x + 16 * (z + 16 * y)] = far::cell(
                            page.x * 16 + x as i32,
                            page.y * 16 + y as i32,
                            page.z * 16 + z as i32,
                            true,
                        );
                    }
                }
            }
            if page == center {
                cells[12 + 16 * (3 + 16)] = Cell::from_parts(rules::ids::CHEST, 0);
            }
        } else if page == center {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
            for y in 1..9 {
                for x in 0..16 {
                    cells[x + 16 * (16 * y)] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
            cells[12 + 16 * (3 + 16)] = Cell::from_parts(rules::ids::CHEST, 0);
            if full {
                for z in 4..7 {
                    for x in 11..14 {
                        cells[x + 16 * (z + 16)] = Cell::from_parts(rules::ids::WATER, 0);
                    }
                }
            }
        }
        view.scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page,
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(1),
            )
            .map_err(|e| e.to_string())?;
        view.scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page,
                        cell_view: 1,
                        version: 1,
                        source: Version(1),
                    },
                    values: Box::new([net::light::pack(15, 15, [15, 15, 15]).unwrap(); 4096]),
                }),
                Tick(1),
            )
            .map_err(|e| e.to_string())?;
    }
    // A raised, fixed camera puts the moving silhouettes over the flat floor.
    // Floor/wall and Chest AA edges have a different spatial history and are
    // tested separately; they are not ghost-color witnesses for this actor.
    let mut player = sim::Player::new([8., 4., 14.]);
    player.yaw = 180.;
    view.install_remote_player(&player, Tick(1))?;
    view.set_personal_time(Some(client::celestial_clock::PersonalTime::Day))?;
    view.set_public_clock(Tick(6000), 0.)?;
    if committed_far {
        far::bootstrap(&mut view, gpu, 2, true)?;
    }
    for _ in 0..32 {
        pump(&mut view, gpu)?;
        if view.initial_scene_inputs_ready()
            && view
                .props_stats()
                .is_some_and(|p| p.ready && p.instances == 1)
            && (!full
                || view.water_stats().is_some_and(|w| w.ready)
                    && view.environment_stats().is_some_and(|e| e.source_ready))
        {
            return Ok(view);
        }
    }
    Err(format!(
        "input preparation failed {:?} {:?} {:?}",
        view.stats(),
        view.props_stats(),
        view.environment_stats()
    ))
}
fn actor(view: &mut ClientView, x: Option<f32>, limb: f32) -> Result<(), String> {
    if let Some(x) = x {
        let row = rules::entities::mob::Instance {
            position: [x, 1., 7.],
            kind: 5,
            scale: 1.,
            id_low: 7,
            id_high: 11,
            limb_amount: limb.min(1.),
            limb_phase: limb,
            ..Default::default()
        };
        if view.temporal_entity_identities_prepared() {
            let key = client::model_motion_identity::human(0, net::PlayerId([23; 16])).unwrap();
            view.set_remote_entities_with_identities(&[row], &[key], SpaceId(0), [0; 3])?;
        } else {
            view.set_remote_entities(&[row], SpaceId(0), [0; 3])?;
        }
    } else if view.temporal_entity_identities_prepared() {
        view.set_remote_entities_with_identities(&[], &[], SpaceId(0), [0; 3])?;
    } else {
        view.set_remote_entities(&[], SpaceId(0), [0; 3])?;
    }
    Ok(())
}
async fn done(gpu: &render::Gpu) -> Result<(), String> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    gpu.queue.on_submitted_work_done(move || {
        let _ = sender.send(());
    });
    #[cfg(not(target_arch = "wasm32"))]
    gpu.device
        .poll(wgpu::PollType::Wait)
        .map_err(|e| e.to_string())?;
    receiver.await.map_err(|e| e.to_string())
}
#[cfg(target_arch = "wasm32")]
async fn presentation_boundary() -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let (sender, receiver) = futures_channel::oneshot::channel();
    let callback = wasm_bindgen::closure::Closure::once(move |_: f64| {
        let _ = sender.send(());
    });
    web_sys::window()
        .ok_or("window")?
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .map_err(|e| format!("{e:?}"))?;
    receiver.await.map_err(|e| e.to_string())?;
    drop(callback);
    Ok(())
}
async fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Result<Vec<u8>, String> {
    #[cfg(target_arch = "wasm32")]
    let _ = gpu;
    let (sender, receiver) = futures_channel::oneshot::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = sender.send(r);
    });
    #[cfg(not(target_arch = "wasm32"))]
    gpu.device
        .poll(wgpu::PollType::Wait)
        .map_err(|e| e.to_string())?;
    receiver
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    Ok(bytes)
}
struct Host {
    gpu: render::Gpu,
    surface: Option<wgpu::Surface<'static>>,
    fallback: wgpu::Texture,
    frame: u64,
    capture: client::capture::FrameGpuCapture,
    recorder: telemetry::flight::FlightRecorder,
}
impl Host {
    fn new(gpu: render::Gpu, surface: Option<wgpu::Surface<'static>>) -> Self {
        let fallback = fallback(&gpu);
        let capture = client::capture::FrameGpuCapture::new_with_queue(&gpu.device, &gpu.queue);
        Self {
            gpu,
            surface,
            fallback,
            frame: 0,
            capture,
            recorder: telemetry::flight::FlightRecorder::new(128, 16).unwrap(),
        }
    }
    async fn frame(
        &mut self,
        view: &mut ClientView,
        label: &str,
        cached: bool,
        discard: u8,
    ) -> Result<Value, String> {
        self.frame += 1;
        let frame = self.frame;
        view.upload_ready(&self.gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))?;
        view.set_presentation_frame_id(frame)?;
        let surface = self
            .surface
            .as_ref()
            .filter(|_| discard == 0)
            .map(|s| s.get_current_texture().map_err(|e| e.to_string()))
            .transpose()?;
        let target = surface.as_ref().map_or(&self.fallback, |s| &s.texture);
        let output = target.create_view(&Default::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.capture
            .begin_frame(&mut encoder, self.gpu.queue.get_timestamp_period());
        if cached {
            if !view.encode_cached_present(&mut encoder, &output)? {
                return Err("cached HDR unavailable".into());
            }
        } else {
            view.encode(&self.gpu.queue, &mut encoder, &output, W, H)?;
        }
        let shape = view.gpu_timing_shape();
        let ticket = view
            .encode_gpu_capture(&mut self.capture, &mut encoder, frame)
            .ok_or("complete GPU capture unavailable")?;
        let bytes = u64::from(W) * u64::from(H) * 4;
        let image = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("diagnostic actual surface RGBA"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &image,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(W * 4),
                    rows_per_image: Some(H),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
        let motion = view.temporal_motion_views().map(|(motion, identity)| {
            let bytes = u64::from(W) * u64::from(H) * 24;
            let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("actual motion and identity diagnostic"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            for (texture, width, offset) in [
                (motion.texture(), 8, 0),
                (identity.texture(), 16, u64::from(W) * u64::from(H) * 8),
            ] {
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset,
                            bytes_per_row: Some(W * width),
                            rows_per_image: Some(H),
                        },
                    },
                    wgpu::Extent3d {
                        width: W,
                        height: H,
                        depth_or_array_layers: 1,
                    },
                );
            }
            buffer
        });
        let stats = view
            .environment_mut()
            .and_then(|e| e.gpu.temporal())
            .and_then(|taa| taa.statistics_buffer())
            .map(|source| {
                let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("actual TAA counters diagnostic"),
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, 16);
                buffer
            });
        if discard == 1 {
            self.capture.cancel(ticket);
            drop(encoder);
            drop(surface);
            view.discard_presentation(frame)?;
            return Ok(json!({"label":label,"unsubmitted":true,"offscreenCandidate":true}));
        }
        self.gpu.queue.submit([encoder.finish()]);
        view.mark_presentation_submitted(frame)?;
        if discard == 2 {
            self.capture.cancel(ticket);
            drop(surface);
            done(&self.gpu).await?;
            view.discard_presentation(frame)?;
            return Ok(json!({"label":label,"submittedDiscard":true,"offscreenCandidate":true}));
        }
        if let Some(surface) = surface {
            surface.present();
        }
        self.capture.submitted(ticket);
        self.recorder
            .record_frame(telemetry::flight::Sample {
                frame,
                monotonic_ns: frame,
                interval_ns: 1,
                ..Default::default()
            })
            .map_err(|e| format!("{e:?}"))?;
        done(&self.gpu).await?;
        #[cfg(target_arch = "wasm32")]
        if self.surface.is_some() {
            presentation_boundary().await?;
        }
        view.commit_presentation(frame)?;
        let image = read(&self.gpu, &image).await?;
        let motion = if let Some(buffer) = &motion {
            read(&self.gpu, buffer).await?
        } else {
            vec![]
        };
        let counters = if let Some(buffer) = &stats {
            read(&self.gpu, buffer)
                .await?
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let presented = view
            .environment_mut()
            .and_then(|e| e.gpu.temporal())
            .and_then(|t| t.presented());
        for _ in 0..8 {
            self.capture.collect(&mut self.recorder);
            done(&self.gpu).await?;
            if self
                .recorder
                .frames()
                .last()
                .is_some_and(|s| s.gpu_total_complete)
            {
                break;
            }
        }
        let complete = self
            .recorder
            .frames()
            .last()
            .is_some_and(|s| s.gpu_total_complete);
        if !complete {
            return Err("whole GPU interval did not complete".into());
        }
        Ok(
            json!({"label":label,"frame":frame,"image":image,"motion":motion,"counters":counters,"sources":shape.0,"queries":shape.1,
            "presented":presented.map(|p| json!({"frame":p.state.frame,"samples":p.samples,"geometryEpoch":p.state.geometry_epoch,"sourceEpoch":p.state.source_epoch})),
            "surfacePresented":self.surface.is_some(),"gpuTotalComplete":complete,
            "boards":view.recipe_board_stats().map(|s|json!({"active":s.active_boards,"draws":s.draw_calls}))}),
        )
    }
}
fn fallback(gpu: &render::Gpu) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual View native output"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
async fn run(mut host: Host) -> Result<Value, String> {
    host.gpu
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let mut results = vec![];
    let mut moving = build(&host.gpu, true, false, 1, false)?;
    let mut background = build(&host.gpu, true, false, 1, false)?;
    let mut reference = build(&host.gpu, false, false, 1, false)?;
    let mut multisample = build(&host.gpu, false, false, 4, false)?;
    actor(&mut multisample, Some(6.), 0.)?;
    results.push(
        host.frame(&mut multisample, "reference-msaa4", false, 0)
            .await?,
    );
    drop(multisample);
    actor(&mut reference, None, 0.)?;
    results.push(
        host.frame(&mut reference, "reference-background", false, 0)
            .await?,
    );
    actor(&mut reference, Some(6.), 0.)?;
    results.push(
        host.frame(&mut reference, "reference-old", false, 0)
            .await?,
    );
    actor(&mut reference, Some(10.), 0.)?;
    results.push(
        host.frame(&mut reference, "reference-new", false, 0)
            .await?,
    );
    for phase in 0..8 {
        actor(&mut moving, Some(6.), 0.)?;
        actor(&mut background, None, 0.)?;
        let actual = host.frame(&mut moving, "warmup", false, 0).await?;
        let control = host
            .frame(&mut background, "background-warmup", false, 0)
            .await?;
        if phase == 7 {
            results.push(actual);
            results.push(control);
        }
    }
    actor(&mut moving, Some(10.), 0.)?;
    actor(&mut background, None, 0.)?;
    results.push(host.frame(&mut moving, "moved", false, 0).await?);
    results.push(
        host.frame(&mut background, "background-moved", false, 0)
            .await?,
    );
    actor(&mut moving, Some(8.), 2.)?;
    results.push(host.frame(&mut moving, "abandoned", false, 1).await?);
    results.push(host.frame(&mut moving, "discarded", false, 2).await?);
    results.push(host.frame(&mut moving, "restored", true, 0).await?);
    actor(&mut moving, Some(10.), 1.)?;
    results.push(host.frame(&mut moving, "deformed", false, 0).await?);
    actor(&mut moving, Some(10.), 0.)?;
    results.push(host.frame(&mut moving, "shape-reset", false, 0).await?);
    actor(&mut moving, None, 0.)?;
    actor(&mut background, None, 0.)?;
    results.push(host.frame(&mut moving, "removed", false, 0).await?);
    results.push(
        host.frame(&mut background, "background-removed", false, 0)
            .await?,
    );
    drop(moving);
    drop(background);
    drop(reference);
    let mut full = build_inner(&host.gpu, true, true, 1, false, true)?;
    full.set_recipe_board_landmark(Some(boards::landmark()))?;
    actor(&mut full, Some(6.), 1.)?;
    results.push(host.frame(&mut full, "full-graph", false, 0).await?);
    results.push(host.frame(&mut full, "full-cached", true, 0).await?);
    actor(&mut full, Some(10.), 2.)?;
    results.push(host.frame(&mut full, "full-discard", false, 2).await?);
    results.push(host.frame(&mut full, "full-restored", true, 0).await?);
    if let Some(error) = host.gpu.device.pop_error_scope().await {
        return Err(error.to_string());
    }
    Ok(
        json!({"extent":[W,H],"results":results,"backend":format!("{:?}",host.gpu.adapter.get_info().backend),"source":"production Rust ClientView; near/models/full optics and boards; synchronous diagnostic job codec execution; committed Far uses its separate route"}),
    )
}

async fn run_far(mut host: Host) -> Result<Value, String> {
    host.gpu
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let mut temporal =
        build(&host.gpu, true, false, 1, true).map_err(|e| format!("initial temporal Far: {e}"))?;
    let mut reference = build(&host.gpu, false, false, 1, true)
        .map_err(|e| format!("initial reference Far: {e}"))?;
    let mut results = vec![];
    for phase in 0..8 {
        let frame = host.frame(&mut temporal, "far-static", false, 0).await?;
        if phase == 7 {
            results.push(frame);
        }
    }
    results.push(
        host.frame(&mut reference, "far-reference", false, 0)
            .await?,
    );
    far::camera(&mut temporal, 8.5)?;
    far::camera(&mut reference, 8.5)?;
    results.push(host.frame(&mut temporal, "far-pan", false, 0).await?);
    results.push(
        host.frame(&mut reference, "far-pan-reference", false, 0)
            .await?,
    );
    let committed = far::metadata(&temporal);
    far::bootstrap(&mut temporal, &host.gpu, 3, false)?;
    far::bootstrap(&mut reference, &host.gpu, 3, false)?;
    results.push(host.frame(&mut temporal, "far-removed", false, 0).await?);
    results.push(
        host.frame(&mut reference, "far-removed-reference", false, 0)
            .await?,
    );
    let replaced = far::metadata(&temporal);
    if let Some(error) = host.gpu.device.pop_error_scope().await {
        return Err(error.to_string());
    }
    Ok(
        json!({"extent":[W,H],"results":results,"committed":committed,"replaced":replaced,"partialCutoutAlpha":128,
        "backend":format!("{:?}",host.gpu.adapter.get_info().backend),
        "source":"actual production Rust ClientView; committed public Height+Proxy sources; explicit authored world; synchronous diagnostic job codecs"}),
    )
}

#[cfg(target_arch = "wasm32")]
async fn browser_host(canvas: web_sys::HtmlCanvasElement) -> Result<Host, wasm_bindgen::JsValue> {
    console_error_panic_hook::set_once();
    canvas.set_width(W);
    canvas.set_height(H);
    let instance = render::instance();
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
    let gpu = render::Gpu::request(&instance, Some(&surface), false)
        .await
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e))?;
    if !surface
        .get_capabilities(&gpu.adapter)
        .formats
        .contains(&wgpu::TextureFormat::Rgba8Unorm)
    {
        return Err(wasm_bindgen::JsValue::from_str("RGBA8 canvas required"));
    }
    let mut config = surface
        .get_default_config(&gpu.adapter, W, H)
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("surface config"))?;
    config.format = wgpu::TextureFormat::Rgba8Unorm;
    // WebGPU allows COPY_SRC, despite wgpu26's conservative capability mask.
    config.usage |= wgpu::TextureUsages::COPY_SRC;
    surface.configure(&gpu.device, &config);
    Ok(Host::new(gpu, Some(surface)))
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn run_temporal_view_probe(
    canvas: web_sys::HtmlCanvasElement,
) -> Result<String, wasm_bindgen::JsValue> {
    run(browser_host(canvas).await?)
        .await
        .map(|v| v.to_string())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn run_temporal_far_probe(
    canvas: web_sys::HtmlCanvasElement,
) -> Result<String, wasm_bindgen::JsValue> {
    run_far(browser_host(canvas).await?)
        .await
        .map(|v| v.to_string())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn run_temporal_boards_probe(
    canvas: web_sys::HtmlCanvasElement,
) -> Result<String, wasm_bindgen::JsValue> {
    boards::run(browser_host(canvas).await?)
        .await
        .map(|v| v.to_string())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #[test]
    fn actual_view_probe_executes_the_same_rust_graph_on_native() {
        let result = pollster::block_on(super::run_native_probe()).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/taa-view-native.json");
        std::fs::write(path, result.to_string()).unwrap();
    }
    #[test]
    fn actual_recipe_boards_view_uses_presented_identity_and_motion() {
        let result = pollster::block_on(super::run_native_boards_probe()).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/taa-boards-native.json");
        std::fs::write(path, result.to_string()).unwrap();
    }
    #[test]
    fn actual_committed_far_view_executes_the_production_motion_graph() {
        let gpu =
            pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
        let result = pollster::block_on(super::run_far(super::Host::new(gpu, None))).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/taa-far-native.json");
        std::fs::write(path, result.to_string()).unwrap();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn run_native_probe() -> Result<Value, String> {
    let gpu = render::Gpu::request(&render::instance(), None, true).await?;
    run(Host::new(gpu, None)).await
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn run_native_far_probe() -> Result<Value, String> {
    let gpu = render::Gpu::request(&render::instance(), None, true).await?;
    run_far(Host::new(gpu, None)).await
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn run_native_boards_probe() -> Result<Value, String> {
    let gpu = render::Gpu::request(&render::instance(), None, true).await?;
    boards::run(Host::new(gpu, None)).await
}
