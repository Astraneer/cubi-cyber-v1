//! Local standalone loadout used until the inventory/authority milestones.
//! Only this explicit demo owner may publish locally; multiplayer must submit
//! intents to its authoritative session instead of using this controller.
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use sim::{EditAction, EditError, InteractionController, Player, RayResult, WorldQuery};
use terrain_gen::{Generator, TerrainSeed};
use world::{CellRead, Transaction, World, WorldChange};

pub const DEMO_TOOL: u16 = rules::items::I_WOODEN_PICKAXE;
pub const DEMO_BLOCK: Cell = Cell::from_parts(rules::ids::STONE, 0);
#[derive(Clone, Copy, Debug, Default)]
pub struct DemoInteractionStats {
    pub broken: u64,
    pub placed: u64,
    pub rolled_back: u64,
    pub denied: u64,
    pub last_error: Option<EditError>,
    pub last_position: Option<BlockPos>,
}
pub struct PreparedDemoEdit {
    pub position: BlockPos,
    pub transaction: Transaction,
}
#[derive(Clone, Copy)]
struct Undo {
    position: BlockPos,
    before: Cell,
    after: Cell,
    version: Version,
}
pub struct DemoInteractions {
    generator: Generator,
    breaking: InteractionController,
    place_delay: u8,
    last_tick: Option<Tick>,
    undo: Option<Undo>,
    stats: DemoInteractionStats,
}
impl DemoInteractions {
    pub fn new(seed: TerrainSeed) -> Self {
        Self {
            generator: Generator::new(seed),
            breaking: InteractionController::default(),
            place_delay: 0,
            last_tick: None,
            undo: None,
            stats: DemoInteractionStats::default(),
        }
    }
    pub fn stats(&self) -> DemoInteractionStats {
        self.stats
    }
    pub fn mining_progress(&self) -> f32 {
        self.breaking.mining.progress
    }
    pub fn target(&self, world: &World, player: &Player) -> RayResult {
        let reach = if player.mode == sim::GameMode::Creative || player.noclip {
            5.0
        } else {
            4.5
        };
        sim::raycast(
            &WorldQuery::new(world, SpaceId(0)),
            player.eye(1.0),
            player.look(),
            reach,
        )
    }
    /// Runs once per fixed tick; transactions still need resource preflight and
    /// the scene's version-checked commit before `published` is called.
    pub fn prepare_tick(
        &mut self,
        world: &World,
        player: &Player,
        tick: Tick,
        attack: bool,
        place: bool,
    ) -> Result<Option<PreparedDemoEdit>, EditError> {
        if self.last_tick.is_some_and(|old| tick.0 <= old.0) {
            return Err(EditError::DuplicateTick);
        }
        self.last_tick = Some(tick);
        self.place_delay = self.place_delay.saturating_sub(1);
        let result =
            self.breaking
                .tick_break(world, SpaceId(0), player, tick, DEMO_TOOL, 0, attack, |p| {
                    self.generator.protects_spawn(p[0], p[2])
                });
        match result {
            Ok(Some(transaction)) => return Ok(Self::prepared(transaction)),
            Err(error) => return self.denied(error),
            Ok(None) => {}
        }
        if attack || !place || self.place_delay != 0 {
            return Ok(None);
        }
        self.place_delay = 4;
        let hit = match self.target(world, player) {
            RayResult::Hit(hit) => hit,
            RayResult::Unknown(_) => return self.denied(EditError::Unknown),
            RayResult::Miss => return Ok(None),
        };
        let direction = rules::cell::DIRS[hit.face as usize];
        let position = if rules::blocks::replaceable_by(
            hit.block.raw(),
            rules::items::block_item(DEMO_BLOCK.id()),
        ) {
            hit.cell
        } else {
            std::array::from_fn(|axis| hit.cell[axis] + direction[axis])
        };
        let block = BlockPos {
            space: SpaceId(0),
            x: position[0],
            y: position[1],
            z: position[2],
        };
        let current = match world.read(block) {
            CellRead::Known(cell) => cell,
            CellRead::Unknown => return self.denied(EditError::Unknown),
        };
        match sim::prepare_edit(
            world,
            SpaceId(0),
            player,
            EditAction::Place {
                position,
                expected: current,
                cell: DEMO_BLOCK,
            },
            tick,
            |p| self.generator.protects_spawn(p[0], p[2]),
        ) {
            Ok(transaction) => Ok(Self::prepared(transaction)),
            Err(error) => self.denied(error),
        }
    }
    fn denied<T>(&mut self, error: EditError) -> Result<T, EditError> {
        self.stats.denied = self.stats.denied.saturating_add(1);
        self.stats.last_error = Some(error);
        Err(error)
    }
    fn prepared(transaction: Transaction) -> Option<PreparedDemoEdit> {
        let (position, _) = transaction.cell_updates().next()?;
        Some(PreparedDemoEdit {
            position,
            transaction,
        })
    }
    pub fn published(&mut self, change: &WorldChange) {
        let Some(edit) = change.cells.first() else {
            return;
        };
        let (x, y, z) = edit.index.xyz();
        let position = BlockPos {
            space: edit.page.space,
            x: edit.page.x * 16 + i32::from(x),
            y: edit.page.y * 16 + i32::from(y),
            z: edit.page.z * 16 + i32::from(z),
        };
        self.stats.last_position = Some(position);
        self.stats.last_error = None;
        if edit.new == Cell::AIR {
            self.stats.broken = self.stats.broken.saturating_add(1);
        } else {
            self.stats.placed = self.stats.placed.saturating_add(1);
        }
        let version = change
            .pages
            .iter()
            .find(|p| p.page == edit.page)
            .map_or(Version(0), |p| p.after);
        self.undo = Some(Undo {
            position,
            before: edit.old,
            after: edit.new,
            version,
        });
    }
    /// Demo reconciliation probe: CAS rejects ABA or an intervening edit. This
    /// does not authorize a multiplayer rollback or bypass mining validation.
    pub fn prepare_rollback(
        &self,
        world: &World,
        tick: Tick,
    ) -> Result<Option<PreparedDemoEdit>, EditError> {
        let Some(undo) = self.undo else {
            return Ok(None);
        };
        if world.page_version(undo.position.page()) != undo.version
            || world.read(undo.position) != CellRead::Known(undo.after)
        {
            return Err(EditError::Stale);
        }
        let mut transaction = Transaction::new(tick, undo.position.space);
        transaction
            .expect_page(undo.position.page(), undo.version)
            .set_cell(undo.position, undo.before);
        Ok(Some(PreparedDemoEdit {
            position: undo.position,
            transaction,
        }))
    }
    pub fn rolled_back(&mut self) {
        self.undo = None;
        self.breaking.mining.cancel();
        self.stats.rolled_back = self.stats.rolled_back.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::PageId;
    fn fixture() -> (World, Player, DemoInteractions) {
        let mut world = World::new(rules::properties, 128);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for y in 3..=5 {
            for z in 5..=7 {
                for x in 5..=7 {
                    tx.create_page(
                        PageId {
                            space: SpaceId(0),
                            x,
                            y,
                            z,
                        },
                        vec![Cell::AIR; 4096],
                    );
                }
            }
        }
        world.commit(tx).unwrap();
        let mut player = Player::new([96.5, 73.0, 96.5]);
        player.on_ground = true;
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        tx.set_cell(
            BlockPos {
                space: SpaceId(0),
                x: 96,
                y: 74,
                z: 99,
            },
            DEMO_BLOCK,
        );
        world.commit(tx).unwrap();
        (world, player, DemoInteractions::new(TerrainSeed(1337)))
    }
    #[test]
    fn wooden_pick_stone_uses_23_fixed_ticks_then_cas_rollback() {
        let (mut world, player, mut demo) = fixture();
        for tick in 1..23 {
            assert!(
                demo.prepare_tick(&world, &player, Tick(tick), true, false)
                    .unwrap()
                    .is_none()
            );
        }
        let edit = demo
            .prepare_tick(&world, &player, Tick(23), true, false)
            .unwrap()
            .expect("reference wood pick stone requires23ticks");
        let p = edit.position;
        let change = world.commit(edit.transaction).unwrap();
        demo.published(&change);
        assert_eq!(world.read(p), CellRead::Known(Cell::AIR));
        assert_eq!(demo.stats().broken, 1);
        let undo = demo.prepare_rollback(&world, Tick(24)).unwrap().unwrap();
        world.commit(undo.transaction).unwrap();
        demo.rolled_back();
        assert_eq!(world.read(p), CellRead::Known(DEMO_BLOCK));
        assert_eq!(demo.stats().rolled_back, 1);
    }
    #[test]
    fn break_cooldown_advances_while_attack_is_aimed_at_air() {
        let (mut world, mut player, mut demo) = fixture();
        for tick in 1..=23 {
            if let Some(edit) = demo
                .prepare_tick(&world, &player, Tick(tick), true, false)
                .unwrap()
            {
                let change = world.commit(edit.transaction).unwrap();
                demo.published(&change);
            }
        }
        let mut tx = Transaction::new(Tick(23), SpaceId(0));
        tx.set_cell(
            BlockPos {
                space: SpaceId(0),
                x: 96,
                y: 74,
                z: 100,
            },
            DEMO_BLOCK,
        );
        world.commit(tx).unwrap();
        player.yaw = 90.0;
        for tick in 24..=28 {
            assert!(
                demo.prepare_tick(&world, &player, Tick(tick), true, false)
                    .unwrap()
                    .is_none()
            );
        }
        player.yaw = 0.0;
        assert!(
            demo.prepare_tick(&world, &player, Tick(29), true, false)
                .unwrap()
                .is_none()
        );
        assert!(
            demo.mining_progress() > 0.04,
            "the five tick delay already elapsed while aiming at air"
        );
    }
    #[test]
    fn held_placement_waits_four_ticks_and_never_intersects_player() {
        let (mut world, player, mut demo) = fixture();
        let edit = demo
            .prepare_tick(&world, &player, Tick(1), false, true)
            .unwrap()
            .unwrap();
        assert_eq!(edit.position.z, 98);
        let change = world.commit(edit.transaction).unwrap();
        demo.published(&change);
        for tick in 2..5 {
            assert!(
                demo.prepare_tick(&world, &player, Tick(tick), false, true)
                    .unwrap()
                    .is_none()
            );
        }
        let edit = demo
            .prepare_tick(&world, &player, Tick(5), false, true)
            .unwrap()
            .unwrap();
        assert_eq!(edit.position.z, 97);
        let change = world.commit(edit.transaction).unwrap();
        demo.published(&change);
        for tick in 6..9 {
            assert!(
                demo.prepare_tick(&world, &player, Tick(tick), false, true)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(matches!(
            demo.prepare_tick(&world, &player, Tick(9), false, true),
            Err(EditError::Occupied)
        ));
        assert_eq!(demo.stats().placed, 2);
    }
    #[test]
    fn duplicate_tick_and_aba_rollback_are_rejected() {
        let (mut world, player, mut demo) = fixture();
        let edit = demo
            .prepare_tick(&world, &player, Tick(1), false, true)
            .unwrap()
            .unwrap();
        let p = edit.position;
        let change = world.commit(edit.transaction).unwrap();
        demo.published(&change);
        assert!(matches!(
            demo.prepare_tick(&world, &player, Tick(1), false, true),
            Err(EditError::DuplicateTick)
        ));
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(p, Cell::AIR);
        world.commit(tx).unwrap();
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.set_cell(p, DEMO_BLOCK);
        world.commit(tx).unwrap();
        assert!(matches!(
            demo.prepare_rollback(&world, Tick(4)),
            Err(EditError::Stale)
        ));
    }
    #[test]
    fn spawn_protection_comes_from_the_public_generator_rule() {
        let (mut world, mut player, mut demo) = fixture();
        let spawn = demo.generator.spawn().center;
        player
            .teleport([
                spawn[0] as f64 + 0.5,
                spawn[1] as f64 + 2.0,
                spawn[2] as f64 + 0.5,
            ])
            .unwrap();
        let base = BlockPos {
            space: SpaceId(0),
            x: spawn[0],
            y: spawn[1] + 3,
            z: spawn[2] + 2,
        };
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        if world.page(base.page()).is_none() {
            tx.create_page(base.page(), vec![Cell::AIR; 4096]);
        }
        tx.set_cell(base, DEMO_BLOCK);
        world.commit(tx).unwrap();
        // Eye74.62-equivalent y+3.62 intersects the protected target.
        assert!(matches!(
            demo.prepare_tick(&world, &player, Tick(2), true, false),
            Err(EditError::Protected)
        ));
    }
}
