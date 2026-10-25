use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{
    entities::{
        EntityError, EntityId, MobKind,
        combat::{AttackPose, CombatStatus, Combatant, mob::AttackContext},
        mob::Mob,
    },
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{Inventory, health::Health},
};
use server::{
    entities::{
        self, Budget, Config, MobDelta, MobRuntime, interactions::Request, items::ItemRuntime,
    },
    survival,
};
use world::{Transaction, World};
fn scene() -> World {
    let mut world = World::new(rules::properties, 100);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        (0..PAGE_VOLUME)
            .map(|i| Cell::from_parts(if i / 256 == 0 { STONE } else { AIR }, 0))
            .collect::<Vec<_>>(),
    );
    world.commit(tx).unwrap();
    world
}
fn actor(held: u16) -> Combatant {
    let mut health = Health::new();
    health.set_spawn_protection(0);
    let mut inventory = Inventory::empty();
    if held != 0 {
        inventory
            .set_trusted(0, ItemStack::new(held, 1).unwrap())
            .unwrap();
    }
    Combatant {
        id: EntityId(99),
        position: [0.5, 1.0, 0.5],
        velocity: [0.0; 3],
        inventory,
        health,
        status: CombatStatus {
            attack_ticks: 100,
            ..Default::default()
        },
        crouching: false,
        pose: AttackPose {
            on_ground: true,
            ..Default::default()
        },
    }
}
fn request(actor: Combatant) -> Request {
    Request {
        actor,
        context: AttackContext {
            space: SpaceId(0),
            eye: [0.5, 2.62, 0.5],
            look_horizontal: [0.0, 1.0],
            expected_held: actor.inventory.hand(),
            now: Tick(1),
        },
        target: EntityId(1),
        action: 7,
    }
}
fn runtime(kind: MobKind) -> MobRuntime {
    let mut runtime = MobRuntime::new(Config {
        maximum_entities: 2,
        path_workers: 1,
        maximum_path_jobs: 1,
        seed: 1337,
    })
    .unwrap();
    runtime
        .insert_initial(Mob::new(EntityId(1), kind, [0.5, 1.0, 2.5]).unwrap(), true)
        .unwrap();
    runtime
}
#[test]
fn real_owner_attack_death_tool_wear_and_durable_drop_are_one_guarded_action() {
    let mut world = scene();
    let mut runtime = runtime(MobKind::Chicken);
    let actor = actor(I_WOODEN_PICKAXE + 14);
    let plan = runtime.plan_attack(&world, request(actor)).unwrap();
    assert!(runtime.entities().get(EntityId(1)).unwrap().life.alive());
    assert_eq!(plan.actor.inventory.hand().damage(), 1);
    assert_eq!(plan.actor.status.attack_ticks, 0);
    assert_eq!(plan.drops.len(), 1);
    assert!(!plan.delta.patches[0].after.unwrap().life.alive());
    assert!(plan.delta.patches[0].after.unwrap().loot_emitted);
    runtime.validate_interaction(&world, &actor, &plan).unwrap();
    let mut changed = actor;
    changed.status.attack_ticks = 0;
    assert_eq!(
        runtime.validate_interaction(&world, &changed, &plan),
        Err(EntityError::StalePlan)
    );
    let mut batch = save::Batch::empty(Tick(1));
    for intent in &plan.drops {
        let [x, y, z] = intent.position.map(|v| v.floor() as i32);
        let drop = survival::plan_drops(
            &world,
            BlockPos {
                space: intent.space,
                x,
                y,
                z,
            },
            [9; 16],
            intent.action,
            Tick(1),
            &intent.stacks,
            10,
            None,
        )
        .unwrap();
        let mut tx = Transaction::new(Tick(1), intent.space);
        drop.append_to(&mut tx);
        batch.changes.push(world.commit(tx).unwrap());
    }
    batch.runtimes.push(save::RuntimeRecord {
        kind: save::RUNTIME_MOBS,
        payload: plan.delta.encode().unwrap(),
    });
    batch.players.push(save::SavedPlayer {
        state: net::PlayerState {
            identity: net::PlayerId([9; 16]),
            health: 20.0,
            ..Default::default()
        },
        place_ready_at: Tick(0),
        break_ready_at: Tick(0),
        inventory: plan.actor.inventory.encode().to_vec(),
        window: Vec::new(),
        simulation: Vec::new(),
    });
    let batch = save::decode_batch(&save::encode_batch(&batch).unwrap()).unwrap();
    assert_eq!(
        Inventory::decode(&batch.players[0].inventory)
            .unwrap()
            .hand()
            .damage(),
        1
    );
    runtime
        .acknowledge(MobDelta::decode(&batch.runtimes[0].payload, 2).unwrap())
        .unwrap();
    let next = runtime
        .plan_tick(&world, Tick(2), &[], Budget::default())
        .unwrap();
    assert!(next.drops.is_empty());
    assert_eq!(
        world
            .components()
            .filter(|(_, c)| c.data.kind == survival::DROP_COMPONENT)
            .count(),
        1
    );
}
#[test]
fn real_owner_shear_requires_owned_tool_real_los_and_unchanged_inputs() {
    let mut world = scene();
    let runtime = runtime(MobKind::Sheep);
    let actor = actor(I_SHEARS);
    let plan = runtime.plan_shear(&world, request(actor)).unwrap();
    assert_eq!(plan.actor.inventory.hand().damage(), 1);
    assert!(plan.delta.patches[0].after.unwrap().sheared);
    assert!((1..=3).contains(&plan.drops[0].stacks[0].count()));
    runtime.validate_interaction(&world, &actor, &plan).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for y in 1..=3 {
        tx.set_cell(
            BlockPos {
                space: SpaceId(0),
                x: 0,
                y,
                z: 1,
            },
            Cell::from_parts(STONE, 0),
        );
    }
    world.commit(tx).unwrap();
    assert_eq!(
        runtime.validate_interaction(&world, &actor, &plan),
        Err(EntityError::StalePlan)
    );
    assert!(matches!(
        runtime.plan_shear(&world, request(actor)),
        Err(EntityError::NoEntitlement)
    ));
    assert!(matches!(
        runtime.plan_attack(&World::new(rules::properties, 1), request(actor)),
        Err(EntityError::UnknownInput)
    ));
    assert!(!runtime.entities().get(EntityId(1)).unwrap().sheared);
    drop(plan);
    assert!(!runtime.entities().get(EntityId(1)).unwrap().sheared);
}
#[test]
fn explicit_activity_changes_are_detached_bounded_durable_and_replay_guarded() {
    let mut runtime = runtime(MobKind::Cow);
    let delta = runtime.plan_activity(&[(EntityId(1), false)]).unwrap();
    assert!(runtime.entities().is_active(EntityId(1)));
    let encoded = delta.encode().unwrap();
    runtime.acknowledge(delta).unwrap();
    assert!(!runtime.entities().is_active(EntityId(1)));
    assert_eq!(
        runtime.acknowledge(MobDelta::decode(&encoded, 2).unwrap()),
        Err(EntityError::StalePlan)
    );
    assert!(
        runtime
            .plan_activity(&[(EntityId(1), true), (EntityId(1), false)])
            .is_err()
    );
    let mut items = ItemRuntime::new(1, 1).unwrap();
    items
        .insert_initial(
            rules::entities::item::ItemEntity::new(
                EntityId(1),
                SpaceId(0),
                ItemStack::new(DIRT, 1).unwrap(),
                [0.5, 1.0, 0.5],
                1,
                10,
            )
            .unwrap(),
            false,
        )
        .unwrap();
    let delta = items.plan_activity(&[(EntityId(1), true)]).unwrap();
    assert!(!items.entities().is_active(EntityId(1)));
    items.acknowledge(delta).unwrap();
    assert!(items.entities().is_active(EntityId(1)));
    let plan = runtime
        .plan_tick(
            &World::new(rules::properties, 1),
            Tick(2),
            &[],
            entities::Budget::default(),
        )
        .unwrap();
    assert_eq!(plan.work.actors, 0);
}
