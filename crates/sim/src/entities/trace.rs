//! Cross-runtime scenario; independent donor numeric goldens live in tests.
use super::{
    item::plan_item,
    path::{NavSearch, NavSpec, PathBudget},
    *,
};
use core_types::{Cell, SpaceId, Tick};
use rules::{
    entities::{
        EntityId,
        item::{ItemEntity, PickupPlan},
        mob::{AiBudget, Environment, PlayerIndex, PlayerProbe},
    },
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{Inventory, health::DamageKind},
};

/// Executes five AI/physics bodies, item physics, inventory plans and incremental
/// navigation. Full private codecs are hashed, exposing drift in continuation.
pub fn determinism_trace() -> [u64; 256] {
    let world = |p: [i32; 3]| {
        world::CellRead::Known(Cell::from_parts(
            if p[1] <= 0 || (p[2] == 7 && (-3..=3).contains(&p[0]) && p[1] < 3) {
                STONE
            } else if (4..=6).contains(&p[0]) && (1..=3).contains(&p[1]) && (2..=5).contains(&p[2])
            {
                WATER
            } else {
                AIR
            },
            0,
        ))
    };
    let mut mobs: [Mob; 5] = std::array::from_fn(|index| {
        Mob::spawned(
            EntityId(index as u64),
            MobKind::ALL[index],
            SpaceId(0),
            [index as f64 - 4.0, 1.0, 0.5],
            1337,
        )
        .expect("trace mob")
    });
    let mut players = PlayerIndex::new(1).expect("trace player index");
    let mut item = Some(
        ItemEntity::new(
            EntityId(99),
            SpaceId(0),
            ItemStack::new(DIRT, 5).expect("trace stack"),
            [5.5, 5.0, 3.5],
            1337,
            10,
        )
        .expect("trace item"),
    );
    let mut inventory = Inventory::empty();
    let mut bufs = Bufs::default();
    let mut search = NavSearch::new(
        [0, 1, 0],
        [0, 1, 12],
        NavSpec::for_mob(MobKind::Zombie, false),
        1,
    )
    .expect("trace nav");
    std::array::from_fn(|tick| {
        let now = Tick(tick as u64 + 1);
        let player_position = [((tick / 32) % 3) as f64 - 1.0, 1.0, 12.5];
        players
            .rebuild(&[PlayerProbe {
                id: EntityId(100),
                space: SpaceId(0),
                position: player_position,
                eye: [player_position[0], 2.62, player_position[2]],
                hands: [if tick < 128 { I_WHEAT } else { I_CARROT }, 0],
                alive: true,
                creative: false,
                spectator: false,
                protected: tick < 20,
            }])
            .expect("trace player");
        let mut hash = 0xcbf29ce484222325u64;
        let mut add = |bytes: &[u8]| {
            for &byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for mob in &mut mobs {
            if tick == 64 && mob.kind == MobKind::Chicken {
                mob.body.position[1] = 8.0;
                mob.body.previous = mob.body.position;
                mob.body.velocity = [0.0; 3];
                mob.body.on_ground = false;
            }
            mob.begin_tick();
            let environment = sample_environment(mob, &world).expect("known trace fluids");
            mob.body.in_water = environment.water;
            mob.body.in_lava = environment.lava;
            if tick == 96 && mob.kind == MobKind::Pig {
                mob.life.damage(DamageKind::Generic, 2.0, (0, 0.0), false);
                mob.ai.panic_until = Tick(140);
            }
            let (next, control) = mob
                .plan_ai(
                    now,
                    1337,
                    &players,
                    Environment {
                        grass: true,
                        ..Environment::default()
                    },
                    &mut AiBudget::default(),
                )
                .expect("trace ai");
            *mob = plan_body(&next, &world, control, &mut bufs)
                .expect("trace body")
                .after;
            mob.life.base_tick_end();
            mob.last_tick = now;
            add(&mob.encode().expect("trace codec"));
        }
        if let Some(current) = item {
            let mut current = plan_item(&current, &world, 1337, &mut bufs)
                .expect("trace item physics")
                .after
                .expect("young trace item");
            current.last_tick = now;
            item = if tick == 180 {
                let pickup = PickupPlan::prepare(
                    current,
                    true,
                    &inventory,
                    current.space,
                    current.position,
                    false,
                    true,
                )
                .expect("trace pickup");
                pickup
                    .inventory
                    .apply(&mut inventory)
                    .expect("trace inventory commit");
                pickup.item.after
            } else {
                Some(current)
            };
        }
        add(&[u8::from(item.is_some())]);
        if let Some(current) = item {
            add(&current.encode().expect("trace item codec"));
        }
        add(&inventory.encode());
        let work = search.advance(
            &world,
            PathBudget {
                expansions: 2,
                cells: 1024,
            },
        );
        for value in [work.expanded, work.reads, work.popped] {
            add(&value.to_le_bytes());
        }
        if let Some(path) = search.result() {
            for point in path.points() {
                for value in point {
                    add(&value.to_le_bytes());
                }
            }
        }
        hash
    })
}
