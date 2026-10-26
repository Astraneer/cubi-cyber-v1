//! Fixed public landmark presentation. No seed, generation, private machine state,
//! or inventory is consulted. Unsupported or uncertified backing hides the card.
use super::{ClientView, Resident};
use crate::{ClientScene, recipe_board_images};
use core_types::PageId;
use render::{
    light_volume::PackedLight,
    recipe_boards::{self, Instance, RecipeBoardGpu},
    terrain_gpu::{Camera, UploadBudget},
};
use rules::crossroads::{self, BOARD_COUNT, BoardPlan};
use std::{cell::Cell, collections::BTreeMap};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub allocation_bytes: u64,
    pub texture_bytes: u64,
    pub buffer_bytes: u64,
    pub startup_pipelines: u32,
    pub upload_bytes: u64,
    pub active_boards: u32,
    pub draw_calls: u64,
    pub recipes: u32,
    pub icons: u32,
    pub glyphs: u32,
}
pub(super) struct ViewRecipeBoards {
    gpu: RecipeBoardGpu,
    landmark: Option<net::spawn::Landmark>,
    plans: Option<[BoardPlan; BOARD_COUNT]>,
    stats: Stats,
    draws: Cell<u64>,
    ready_mask: u16,
    place_consumed: bool,
    pending_time: Option<i32>,
}
impl ViewRecipeBoards {
    fn set_landmark(&mut self, landmark: Option<net::spawn::Landmark>) -> Result<bool, String> {
        if self.landmark == landmark {
            return Ok(false);
        }
        let plans = landmark
            .map(|value| crossroads::boards(value.center))
            .transpose()
            .map_err(|e| e.to_string())?;
        self.landmark = landmark;
        self.plans = plans;
        self.ready_mask = 0;
        self.pending_time = None;
        self.place_consumed = false;
        Ok(true)
    }
    pub(super) fn prepare_frame(
        &mut self,
        scene: &ClientScene,
        resident: &BTreeMap<PageId, Resident>,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<bool, String> {
        let mut records = [Instance::default(); BOARD_COUNT];
        if let Some(plans) = &self.plans {
            for (index, plan) in plans.iter().enumerate() {
                if !backing_ready(scene, plan, |page| {
                    resident.get(&page).is_some_and(|r| {
                        r.version == scene.world().input_version(page, crate::MESH_CHANNELS)
                            && scene.world().matches_inputs(&r.inputs)
                    })
                }) {
                    continue;
                }
                let sample = plan.light_sample();
                let light = if scene.config().authoritative_remote {
                    scene.read_authoritative_light(sample)
                } else {
                    scene
                        .world()
                        .page(sample.page())
                        .map(|p| p.light(sample.local_index()))
                };
                let Some(light) = light else {
                    continue;
                };
                let right = plan.facing.right().map(|v| v as f32);
                records[index] = Instance {
                    center: plan.relative_center([0; 3]),
                    right,
                    up: [0., 1., 0.],
                    size: plan
                        .size_200
                        .map(|v| f32::from(v) / crossroads::SUBBLOCKS as f32),
                    layer: index as u32,
                    light: PackedLight::new(light.sky, light.block, light.rgb).unwrap_or_default(),
                    enabled: true,
                };
            }
        }
        let work = self
            .gpu
            .prepare_records(queue, &records, budget)
            .map_err(|e| e.to_string())?;
        self.stats.active_boards = self.gpu.active_boards();
        self.ready_mask = records
            .iter()
            .enumerate()
            .fold(0, |mask, (i, r)| mask | (u16::from(r.enabled) << i));
        self.stats.upload_bytes = self.stats.upload_bytes.saturating_add(work.upload_bytes);
        Ok(work.upload_bytes != 0)
    }
    pub(super) fn prepare_camera(
        &mut self,
        queue: &wgpu::Queue,
        camera: Camera,
        budget: &mut UploadBudget,
    ) -> Result<u64, String> {
        let work = self
            .gpu
            .prepare_camera(queue, camera, budget)
            .map_err(|e| e.to_string())?;
        self.stats.upload_bytes = self.stats.upload_bytes.saturating_add(work.upload_bytes);
        Ok(work.upload_bytes)
    }
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        let count = self.gpu.draw(pass);
        self.draws
            .set(self.draws.get().saturating_add(u64::from(count)));
        count
    }
    fn stats(&self) -> Stats {
        Stats {
            draw_calls: self.draws.get(),
            ..self.stats
        }
    }
}
/// Every backing cell must be a currently certified, visible, GPU-resident opaque
/// wall. A halo page or an old mesh is not a wall certificate. Fixed <=15 cells.
fn backing_ready(
    scene: &ClientScene,
    plan: &BoardPlan,
    mut resident: impl FnMut(PageId) -> bool,
) -> bool {
    plan.backing_cells().all(|position| {
        let page = position.page();
        if !scene.is_visible(page)
            || !resident(page)
            || (scene.config().authoritative_remote && scene.authoritative_version(page).is_none())
        {
            return false;
        }
        let world::CellRead::Known(cell) = scene.world().read(position) else {
            return false;
        };
        rules::block(cell.id())
            .is_some_and(|def| def.render == rules::blocks::Render::Cube && def.fluid == 0)
    })
}
impl ClientView {
    pub fn prepare_recipe_boards_from_assets(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), String> {
        let atlas = ui::atlas::UiAtlas::load().map_err(str::to_string)?;
        let recipes = rules::crafting::RecipeBook::standard()
            .map_err(|e| format!("recipe board book: {e:?}"))?;
        let images = recipe_board_images::prepare(&atlas, &recipes).map_err(|e| e.to_string())?;
        self.prepare_recipe_boards(device, queue, &images)
    }
    /// Prepare all immutable artwork and the one grouped pipeline at loading.
    /// The cards remain hidden until a server landmark and public wall arrive.
    pub fn prepare_recipe_boards(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &recipe_board_images::BoardImages,
    ) -> Result<(), String> {
        if self.recipe_boards.is_some() || self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("recipe boards must be prepared once before scene uploads".into());
        }
        let gpu = RecipeBoardGpu::new(
            device,
            queue,
            self.renderer.geometry(),
            recipe_boards::Images {
                width: recipe_board_images::WIDTH,
                height: recipe_board_images::HEIGHT,
                layers: recipe_board_images::LAYERS,
                rgba: &images.rgba,
            },
            recipe_boards::Config::default(),
            recipe_boards::Lighting {
                shadows: self.lighting.as_ref().map(|l| l.owner.shadows()),
                atmosphere: self.environment.as_ref().map(|e| e.owner.gpu.atmosphere()),
                gi: self.environment.as_ref().and_then(|e| e.owner.gpu.gi()),
            },
        )
        .map_err(|e| e.to_string())?;
        let stats = Stats {
            allocation_bytes: gpu.allocation_bytes(),
            texture_bytes: gpu.texture_bytes(),
            buffer_bytes: gpu.allocation_bytes() - gpu.texture_bytes(),
            startup_pipelines: 1,
            upload_bytes: gpu.texture_bytes(),
            recipes: images.recipes,
            icons: images.icons,
            glyphs: images.glyphs,
            ..Default::default()
        };
        self.pipeline_compilations = self.pipeline_compilations.saturating_add(1);
        self.upload_bytes_total = self.upload_bytes_total.saturating_add(stats.texture_bytes);
        self.retained_foreground_work.record(telemetry::WorkCounts {
            upload_bytes: stats.texture_bytes,
            ..telemetry::WorkCounts::ZERO
        });
        self.recipe_boards = Some(Box::new(ViewRecipeBoards {
            gpu,
            landmark: None,
            plans: None,
            stats,
            draws: Cell::new(0),
            ready_mask: 0,
            place_consumed: false,
            pending_time: None,
        }));
        self.invalidate();
        Ok(())
    }
    pub fn set_recipe_board_landmark(
        &mut self,
        landmark: Option<net::spawn::Landmark>,
    ) -> Result<(), String> {
        if let Some(boards) = &mut self.recipe_boards
            && boards.set_landmark(landmark)?
        {
            self.renderer.invalidate();
            self.invalidate();
        }
        Ok(())
    }
    pub fn recipe_board_stats(&self) -> Option<Stats> {
        self.recipe_boards.as_ref().map(|b| b.stats())
    }
    /// One local input event for the existing platform settings/persistence path.
    /// No server action or public clock change is generated by these buttons.
    pub fn take_recipe_board_time(&mut self) -> Option<i32> {
        self.recipe_boards.as_mut()?.pending_time.take()
    }
    pub(super) fn recipe_board_place(&mut self, down: bool, rising: bool) -> bool {
        let Some(boards) = self.recipe_boards.as_mut() else {
            return false;
        };
        if !down {
            boards.place_consumed = false;
            return false;
        }
        if boards.place_consumed {
            return true;
        }
        if !rising || !self.active {
            return false;
        }
        let Some(plans) = boards.plans else {
            return false;
        };
        let sim::RayResult::Hit(hit) = self.interactions.target(self.scene.world(), &self.player)
        else {
            return false;
        };
        let Some((_, plan)) = plans
            .iter()
            .enumerate()
            .skip(6)
            .take(2)
            .find(|(index, plan)| {
                boards.ready_mask & (1 << index) != 0
                    && button_hit(plan, self.player.eye(1.), self.player.look(), &hit)
                    && backing_ready(&self.scene, plan, |page| {
                        self.resident.get(&page).is_some_and(|r| {
                            r.version
                                == self.scene.world().input_version(page, crate::MESH_CHANNELS)
                                && self.scene.world().matches_inputs(&r.inputs)
                        })
                    })
                    && (!self.scene.config().authoritative_remote
                        || self
                            .scene
                            .authoritative_light_version(plan.light_sample().page())
                            .is_some())
            })
        else {
            return false;
        };
        let rules::crossroads::Sheet::Sky(day) = plan.sheet else {
            return false;
        };
        let value = if day { 1 } else { 2 };
        boards.place_consumed = true;
        boards.pending_time = Some(value);
        if let Some(settings) = &mut self.requested_settings {
            settings
                .set(ui::settings::Setting::ViewTime, value)
                .expect("canonical personal time");
        }
        self.set_personal_time(Some(if day {
            crate::celestial_clock::PersonalTime::Day
        } else {
            crate::celestial_clock::PersonalTime::Night
        }))
        .expect("canonical personal time");
        true
    }
}
fn button_hit(plan: &BoardPlan, eye: [f64; 3], look: [f64; 3], hit: &sim::BlockHit) -> bool {
    if hit.cell != [plan.backing.x, plan.backing.y, plan.backing.z] {
        return false;
    }
    let n = plan.facing.normal().map(f64::from);
    let right = plan.facing.right().map(f64::from);
    let center = plan
        .center_200
        .map(|v| v as f64 / crossroads::SUBBLOCKS as f64);
    let dot = |a: [f64; 3], b: [f64; 3]| a.into_iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    let denominator = dot(look, n);
    if denominator >= -1e-9 {
        return false;
    }
    let distance = dot(std::array::from_fn(|i| center[i] - eye[i]), n) / denominator;
    if !(0. ..=hit.distance + 1e-6).contains(&distance) {
        return false;
    }
    let local = std::array::from_fn(|i| eye[i] + look[i] * distance - center[i]);
    dot(local, right).abs() <= f64::from(plan.size_200[0]) / 400.
        && local[1].abs() <= f64::from(plan.size_200[1]) / 400.
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
    fn fixture() -> (ClientScene, BoardPlan) {
        let center = BlockPos {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let plan = crossroads::boards(center).unwrap()[6];
        let scene = ClientScene::new(crate::SceneConfig {
            center: plan.backing,
            horizontal_radius: 0,
            vertical_radius: 0,
            authoritative_remote: true,
            ..crate::SceneConfig::forest()
        })
        .unwrap();
        (scene, plan)
    }
    fn install(scene: &mut ClientScene, plan: BoardPlan, id: u16, version: u64) {
        let mut cells = Box::new([Cell::AIR; 4096]);
        cells[usize::from(plan.backing.local_index().get())] = Cell::from_parts(id, 0);
        scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page: plan.backing.page(),
                    version,
                    source_version: Version(version),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(0),
            )
            .unwrap();
    }
    #[test]
    fn backing_needs_current_public_cell_current_mesh_and_actual_near_interest() {
        let (mut scene, plan) = fixture();
        assert!(!backing_ready(&scene, &plan, |_| true));
        install(&mut scene, plan, rules::ids::STONE, 1);
        assert!(!backing_ready(&scene, &plan, |_| false));
        assert!(backing_ready(&scene, &plan, |_| true));
        install(&mut scene, plan, rules::ids::AIR, 2);
        assert!(!backing_ready(&scene, &plan, |_| true));
        install(&mut scene, plan, rules::ids::STONE, 3);
        scene
            .set_center(BlockPos {
                x: 64,
                ..plan.backing
            })
            .unwrap();
        assert!(
            !backing_ready(&scene, &plan, |_| true),
            "old retained wall outside actual near must not certify a card"
        );
    }
    #[test]
    fn sixteenish_cells_are_all_required_for_a_large_card() {
        let center = BlockPos {
            space: SpaceId(0),
            x: 32,
            y: 4,
            z: 32,
        };
        let plan = crossroads::boards(center).unwrap()[0];
        let mut scene = ClientScene::new(crate::SceneConfig {
            center: plan.backing,
            horizontal_radius: 1,
            vertical_radius: 1,
            authoritative_remote: true,
            ..crate::SceneConfig::forest()
        })
        .unwrap();
        let positions = plan.backing_cells().collect::<Vec<_>>();
        let pages = positions
            .iter()
            .map(|p| p.page())
            .collect::<std::collections::BTreeSet<_>>();
        for page in pages {
            let mut cells = Box::new([Cell::AIR; 4096]);
            for p in &positions {
                if p.page() == page {
                    cells[usize::from(p.local_index().get())] =
                        Cell::from_parts(rules::ids::STONE, 0);
                }
            }
            scene
                .import_authoritative_page(
                    &net::PageUpdate::Snapshot(net::PageSnapshot {
                        page,
                        version: 1,
                        source_version: Version(1),
                        applied_action: Default::default(),
                        cells,
                    }),
                    Tick(0),
                )
                .unwrap();
        }
        assert!(backing_ready(&scene, &plan, |_| true));
        let excluded = positions[0].page();
        assert!(!backing_ready(&scene, &plan, |p| p != excluded));
    }
    #[test]
    fn real_shared_block_ray_and_exact_button_plane_reject_occlusion_and_border_clicks() {
        let (mut scene, plan) = fixture();
        install(&mut scene, plan, rules::ids::STONE, 1);
        let center = plan.center_200.map(|v| v as f64 / 200.);
        let eye = [center[0], center[1], center[2] + 2.];
        let look = [0., 0., -1.];
        let query = sim::WorldQuery::new(scene.world(), SpaceId(0));
        let sim::RayResult::Hit(hit) = sim::raycast(&query, eye, look, 4.5) else {
            panic!("real public backing must be ray-visible");
        };
        assert!(button_hit(&plan, eye, look, &hit));
        assert!(
            !button_hit(&plan, [eye[0] + 0.45, eye[1], eye[2]], look, &hit),
            "hit on surrounding wall is not a button click"
        );
        assert!(!button_hit(&plan, eye, [0., 0., 1.], &hit));
        assert!(
            !button_hit(
                &plan,
                eye,
                look,
                &sim::BlockHit {
                    distance: 0.5,
                    ..hit
                }
            ),
            "closer opaque occluder wins"
        );
        assert!(!button_hit(
            &plan,
            eye,
            look,
            &sim::BlockHit {
                cell: [hit.cell[0] + 1, hit.cell[1], hit.cell[2]],
                ..hit
            }
        ));
    }
}
