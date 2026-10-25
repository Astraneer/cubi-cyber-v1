//! Explicit PvP/mob goldens from approved server/pvp.rs, pvp_tests.rs,
//! mob/tests.rs and mob/mod.rs. Spatial checks use an independent brute force.
use core_types::{DeterministicRng, Tick};
use rules::{
    entities::{
        EntityError, EntityId, MobKind,
        combat::{self, AttackPlan, AttackPose, CombatStatus, Combatant},
        mob::{
            self, AiBudget, EntityStore, Environment, Goal, Instance, Mob, PlayerIndex, PlayerProbe,
        },
        spatial::{QueryBudget, SpatialHash},
    },
    items::*,
    stack::ItemStack,
    survival::{Inventory, health::Health},
};
use std::{hint::black_box, time::Instant};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}
fn actor(id: u64, position: [f64; 3]) -> Combatant {
    let mut health = Health::new();
    health.set_spawn_protection(0);
    Combatant {
        id: EntityId(id),
        position,
        velocity: [0.0; 3],
        inventory: Inventory::empty(),
        health,
        status: CombatStatus {
            attack_ticks: 40,
            ..CombatStatus::default()
        },
        crouching: false,
        pose: AttackPose {
            on_ground: true,
            ..AttackPose::default()
        },
    }
}
fn probe(id: u64, position: [f64; 3], held: u16) -> PlayerProbe {
    PlayerProbe {
        id: EntityId(id),
        space: core_types::SpaceId(0),
        position,
        eye: [position[0], position[1] + 1.62, position[2]],
        hands: [held, 0],
        alive: true,
        creative: false,
        spectator: false,
        protected: false,
    }
}

#[test]
fn strike_full_weak_critical_sprint_and_fist_golden() {
    let sword = stack(I_WOODEN_PICKAXE + 14, 1);
    let ground = AttackPose {
        on_ground: true,
        ..AttackPose::default()
    };
    assert_eq!(combat::strike(sword, 40, ground).damage, 6.0);
    assert!((combat::strike(sword, 0, ground).damage - 1.20768).abs() < 1e-6);
    let falling = AttackPose {
        fall: 1.0,
        ..AttackPose::default()
    };
    let strike = combat::strike(sword, 40, falling);
    assert_eq!(strike.damage, 9.0);
    assert!(strike.critical);
    let running = AttackPose {
        sprinting: true,
        ..falling
    };
    let strike = combat::strike(sword, 40, running);
    assert_eq!(strike.damage, 6.0);
    assert!(!strike.critical);
    assert!(strike.sprint_knockback);
    assert_eq!(combat::strike(ItemStack::EMPTY, 40, ground).damage, 1.0);
}
#[test]
fn pvp_iron_armor_reduces_six_to_312_and_wears_all_pieces() {
    let mut a = actor(1, [0.5, 1.0, 0.5]);
    a.inventory
        .set_trusted(0, stack(I_WOODEN_PICKAXE + 14, 1))
        .unwrap();
    let mut b = actor(2, [0.5, 1.0, 2.5]);
    for offset in 0..4 {
        let item = I_LEATHER_HELMET + 4 + offset;
        let ItemKind::Armor { slot, .. } = idef(item).kind else {
            panic!("armor")
        };
        b.inventory
            .set_trusted(36 + usize::from(slot), stack(item, 1))
            .unwrap();
    }
    let plan = AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(true)).unwrap();
    assert!((plan.hit.unwrap().taken - 3.12).abs() < 1e-5);
    assert!(
        plan.target.inventory.slots()[36..40]
            .iter()
            .all(|value| value.damage() == 1)
    );
    plan.apply(&mut a, &mut b).unwrap();
    assert!((b.health.hp() - 16.88).abs() < 1e-5);
    assert_eq!(a.status.combat_tag, 200);
    assert!(b.velocity[2] > 0.39);
    assert_eq!(a.inventory.hand().damage(), 1);
}
#[test]
fn spawn_grace_200_ticks_and_attacker_tag_defeats_running_into_spawn() {
    let mut status = CombatStatus::default();
    status.tick(true);
    assert!(status.shielded());
    for _ in 0..199 {
        status.tick(false);
    }
    assert!(status.shielded());
    status.tick(false);
    assert!(!status.shielded());
    status.combat_tag = 200;
    status.tick(true);
    assert!(!status.shielded());
    for _ in 0..199 {
        status.tick(true);
    }
    assert!(status.shielded());
    let mut a = actor(1, [0.5, 1.0, 0.5]);
    let mut b = actor(2, [0.5, 1.0, 2.5]);
    b.status.zone_grace = 200;
    let plan = AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(true)).unwrap();
    assert!(plan.hit.is_none());
    plan.apply(&mut a, &mut b).unwrap();
    assert_eq!(b.health.hp(), 20.0);
    assert_eq!(a.status.combat_tag, 0);
}
#[test]
fn pvp_rejects_far_wall_unknown_self_and_stale_actor_without_mutation() {
    let mut a = actor(1, [0.5, 1.0, 0.5]);
    let mut b = actor(2, [0.5, 1.0, 12.5]);
    assert!(matches!(
        AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(true)),
        Err(EntityError::NoEntitlement)
    ));
    b.position[2] = 4.5;
    assert!(matches!(
        AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(false)),
        Err(EntityError::NoEntitlement)
    ));
    assert!(matches!(
        AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Err(
            EntityError::UnknownInput
        )),
        Err(EntityError::UnknownInput)
    ));
    assert!(matches!(
        AttackPlan::prepare(&a, &a, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(true)),
        Err(EntityError::NoEntitlement)
    ));
    let plan = AttackPlan::prepare(&a, &b, [0.5, 2.62, 0.5], [0.0, 1.0], |_, _| Ok(true)).unwrap();
    a.status.attack_ticks = 1;
    let before = (a, b);
    assert_eq!(plan.apply(&mut a, &mut b), Err(EntityError::StalePlan));
    assert_eq!((a, b), before);
}
#[test]
fn knockback_java_half_velocity_and_sprint_second_impulse() {
    let normal = combat::knockback([0.2, -0.0784, 0.2], true, 0.4, [0.0, -2.0]);
    assert_eq!(normal[0], 0.1);
    assert_eq!(normal[2], 0.5);
    assert_eq!(normal[1], 0.3608);
    let sprint = combat::knockback(normal, true, 0.5, [0.0, -1.0]);
    assert_eq!(sprint[2], 0.75);
    assert_eq!(sprint[1], 0.4);
    assert_eq!(
        combat::knockback([0.0, 0.1, 0.0], false, 0.4, [0.0, -1.0])[1],
        0.1
    );
}
#[test]
fn spatial_queries_match_brute_force_through_churn_and_dense_swaps() {
    let mut hash = SpatialHash::new(64, 8.0).unwrap();
    let mut points = [[0.0; 3]; 64];
    let mut rng = DeterministicRng::from_key(8, 7, 6, 5);
    for (index, point) in points.iter_mut().enumerate() {
        *point = [index as f64 * 2.0 - 64.0, 0.0, index as f64 * 0.5];
        hash.update(index, *point).unwrap();
    }
    for iteration in 0..5000 {
        let index = rng.below(64).unwrap() as usize;
        points[index] = [
            rng.below(201).unwrap() as f64 - 100.0,
            0.0,
            rng.below(201).unwrap() as f64 - 100.0,
        ];
        hash.update(index, points[index]).unwrap();
        if iteration % 23 == 0 {
            let low = [-25.0, -1.0, -25.0];
            let high = [25.0, 1.0, 25.0];
            let mut actual = Vec::new();
            let work = hash
                .query(low, high, QueryBudget::default(), |i| {
                    if (0..3)
                        .all(|axis| points[i][axis] >= low[axis] && points[i][axis] <= high[axis])
                    {
                        actual.push(i);
                    }
                })
                .unwrap();
            assert!(!work.overflow);
            actual.sort_unstable();
            let expected: Vec<_> = points
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    (0..3)
                        .all(|axis| p[axis] >= low[axis] && p[axis] <= high[axis])
                        .then_some(i)
                })
                .collect();
            assert_eq!(actual, expected);
        }
    }
    hash.remove(7).unwrap();
    hash.move_index(63, 7).unwrap();
    let mut seen = Vec::new();
    hash.query(
        [-110.0, -1.0, -110.0],
        [110.0, 1.0, 110.0],
        QueryBudget {
            buckets: 2_000_000,
            probes: 8_000_000,
            candidates: 100,
        },
        |i| seen.push(i),
    )
    .unwrap();
    assert_eq!(seen.len(), 63);
    assert!(seen.contains(&7));
    assert!(!seen.contains(&63));
}
#[test]
fn sparse_spatial_work_is_linear_and_query_caps_are_visible() {
    let mut hash = SpatialHash::new(1000, 8.0).unwrap();
    for index in 0..1000 {
        hash.update(index, [index as f64 * 16.0, 1.0, 1.0]).unwrap();
    }
    let mut candidates = 0;
    let mut probes = 0;
    for index in 0..1000 {
        let x = index as f64 * 16.0;
        let work = hash
            .query(
                [x - 1.0, 0.0, 0.0],
                [x + 1.0, 2.0, 2.0],
                QueryBudget::default(),
                |_| {},
            )
            .unwrap();
        assert!(!work.overflow);
        candidates += work.candidates;
        probes += work.probes;
    }
    assert_eq!(candidates, 1000);
    assert!(probes < 10_000);
    let work = hash
        .query([-1e5; 3], [1e5; 3], QueryBudget::default(), |_| {
            panic!("outside budget")
        })
        .unwrap();
    assert!(work.overflow);
    assert_eq!(work.buckets, 0);
}
#[test]
fn kind_dimensions_health_and_acceleration_table_are_donor_values() {
    let expected = [
        (MobKind::Pig, 0.9, 0.9, 10.0, 0.25, 0.134912),
        (MobKind::Cow, 0.9, 1.4, 10.0, 0.2, 0.086344),
        (MobKind::Sheep, 0.9, 1.3, 8.0, 0.23, 0.114189),
        (MobKind::Chicken, 0.4, 0.7, 4.0, 0.25, 0.134912),
        (MobKind::Zombie, 0.6, 1.95, 20.0, 0.23, 0.114189),
    ];
    for (kind, width, height, hp, speed, per_tick) in expected {
        let mob = Mob::new(EntityId(1), kind, [0.0; 3]).unwrap();
        assert_eq!(mob.dimensions(), (width, height));
        assert_eq!(mob.life.hp(), hp);
        assert_eq!(kind.definition().speed, speed);
        let terminal = 0.98 * f64::from(speed) * f64::from(speed) / 0.454;
        assert!((terminal - per_tick).abs() < 2e-6);
    }
}
#[test]
fn temptation_species_and_creative_filter_use_spatial_nearest_ties() {
    let mut players = PlayerIndex::new(10).unwrap();
    players
        .rebuild(&[
            probe(3, [4.0, 0.0, 0.0], I_CARROT),
            probe(2, [-4.0, 0.0, 0.0], I_CARROT),
        ])
        .unwrap();
    let mut pig = Mob::new(EntityId(8), MobKind::Pig, [0.0; 3]).unwrap();
    pig.age = 2;
    let (mut after, control) = pig
        .plan_ai(
            Tick(1),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(after.ai.goal, Goal::Tempt);
    assert_eq!(after.ai.target, Some(EntityId(2)));
    assert_eq!(control.speed_multiplier, 1.2);
    after.age = 3;
    let (_, control) = after
        .plan_ai(
            Tick(2),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(control.destination, Some([-4.0, 0.0, 0.0]));
    assert_eq!(control.speed_multiplier, 1.2);
    let cow = Mob::new(EntityId(8), MobKind::Cow, [0.0; 3]).unwrap();
    let (after, _) = cow
        .plan_ai(
            Tick(1),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_ne!(after.ai.goal, Goal::Tempt);
    let mut creative = probe(1, [3.0, 0.0, 0.0], I_WHEAT);
    creative.creative = true;
    players.rebuild(&[creative]).unwrap();
    let (after, _) = cow
        .plan_ai(
            Tick(1),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_ne!(after.ai.goal, Goal::Tempt);
}
#[test]
fn zombie_targets_survival_skips_spectator_and_respects_protected_attack() {
    let zombie = Mob::new(EntityId(9), MobKind::Zombie, [0.0; 3]).unwrap();
    let mut players = PlayerIndex::new(5).unwrap();
    let mut spectator = probe(1, [0.0, 0.0, 0.5], 0);
    spectator.spectator = true;
    let mut protected = probe(2, [0.0, 0.0, 1.0], 0);
    protected.protected = true;
    players.rebuild(&[spectator, protected]).unwrap();
    let (after, control) = zombie
        .plan_ai(
            Tick(1),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(after.ai.target, Some(EntityId(2)));
    assert!(control.attack.is_none());
    protected.protected = false;
    players.rebuild(&[protected]).unwrap();
    let (after, control) = zombie
        .plan_ai(
            Tick(1),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(control.attack, Some(EntityId(2)));
    assert_eq!(after.ai.attack_cooldown, 20);
}
#[test]
fn ai_budget_failure_leaves_actor_unmodified_and_empty_players_cost_zero() {
    let mob = Mob::new(EntityId(1), MobKind::Zombie, [0.0; 3]).unwrap();
    let mut players = PlayerIndex::new(1).unwrap();
    players.rebuild(&[probe(2, [0.0, 0.0, 1.0], 0)]).unwrap();
    assert!(matches!(
        mob.plan_ai(
            Tick(1),
            2,
            &players,
            Environment::default(),
            &mut AiBudget {
                buckets: 0,
                probes: 0,
                candidates: 0
            }
        ),
        Err(EntityError::Capacity)
    ));
    assert_eq!(mob.ai.event, 0);
    let players = PlayerIndex::new(0).unwrap();
    let mut budget = AiBudget {
        buckets: 0,
        probes: 0,
        candidates: 0,
    };
    assert!(
        mob.plan_ai(Tick(1), 2, &players, Environment::default(), &mut budget)
            .is_ok()
    );
    assert_eq!(budget.probes, 0);
}
#[test]
fn dense_store_active_admission_and_remove_preserve_stable_ids() {
    let mut store = EntityStore::new(100).unwrap();
    for index in 0..100 {
        store
            .insert(
                Mob::new(EntityId(index), MobKind::Pig, [index as f64, 0.0, 0.0]).unwrap(),
                true,
            )
            .unwrap();
    }
    for id in [3, 8, 99, 30] {
        store.remove(EntityId(id)).unwrap();
    }
    assert_eq!(store.len(), 96);
    assert_eq!(store.active_len(), 96);
    let mut admitted = [EntityId(0); 96];
    assert_eq!(store.active_batch(96, &mut admitted), 96);
    admitted.sort_unstable();
    assert!(admitted.windows(2).all(|pair| pair[0] != pair[1]));
    for id in [3, 8, 99, 30] {
        assert!(store.get(EntityId(id)).is_none());
    }
    let mut out = [EntityId(0); 100];
    let (count, work) = store
        .query_ids(
            [-10.0, -1.0, -1.0],
            [200.0, 1.0, 1.0],
            QueryBudget::default(),
            &mut out,
        )
        .unwrap();
    assert_eq!(count, 96);
    assert!(!work.overflow);
}
#[test]
fn thousand_visible_actors_instance_generation_no_vertices_or_allocations() {
    assert_eq!(std::mem::size_of::<Instance>(), 64);
    let mut store = EntityStore::new(1000).unwrap();
    let visible: Vec<_> = (0..1000).map(EntityId).collect();
    for &id in &visible {
        store
            .insert(
                Mob::new(id, MobKind::ALL[id.0 as usize % 5], [id.0 as f64, 1.0, 0.0]).unwrap(),
                true,
            )
            .unwrap();
    }
    let mut instances = vec![Instance::default(); 1000];
    let control = AllocationScope::begin().unwrap();
    let bytes = Vec::<u8>::with_capacity(black_box(7));
    black_box(&bytes);
    assert!(control.finish().allocating_calls() > 0);
    drop(bytes);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..100 {
        assert_eq!(
            store.instances(&visible, black_box(0.5), &mut instances),
            Ok(1000)
        );
        black_box(&instances);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert!(
        instances
            .iter()
            .enumerate()
            .all(|(index, instance)| instance.position[0] == index as f32)
    );
    let start = Instant::now();
    for _ in 0..100 {
        black_box(store.instances(&visible, 0.5, &mut instances).unwrap());
    }
    println!(
        "1000 instances: {} us/iteration; 64000 bytes; 0 CPU vertices",
        start.elapsed().as_micros() / 100
    );
}
#[test]
fn animal_loot_domains_babies_burning_and_rare_zombie() {
    let mut rng = DeterministicRng::from_key(1, 2, 3, 4);
    let mut totals = BTreeMap::new();
    for kind in MobKind::ALL {
        let mut mob = Mob::new(EntityId(1), kind, [0.0; 3]).unwrap();
        for _ in 0..2000 {
            for stack in mob::mob_loot(&mob, true, &mut rng) {
                if !stack.is_empty() {
                    *totals.entry((kind, stack.item())).or_insert(0u32) += u32::from(stack.count());
                }
            }
        }
        if kind != MobKind::Zombie {
            mob.baby = true;
            assert!(
                mob::mob_loot(&mob, true, &mut rng)
                    .iter()
                    .all(|stack| stack.is_empty())
            );
        }
    }
    assert!((f64::from(totals[&(MobKind::Pig, I_PORKCHOP)]) / 2000.0 - 2.0).abs() < 0.1);
    assert!((f64::from(totals[&(MobKind::Cow, I_LEATHER)]) / 2000.0 - 1.0).abs() < 0.1);
    assert!(
        totals[&(MobKind::Zombie, I_IRON_INGOT)] > 0
            && totals[&(MobKind::Zombie, I_IRON_INGOT)] < 40
    );
    let mut pig = Mob::new(EntityId(1), MobKind::Pig, [0.0; 3]).unwrap();
    pig.life.ignite(100);
    assert!(
        mob::mob_loot(&pig, true, &mut rng)
            .iter()
            .any(|stack| stack.item() == I_COOKED_PORKCHOP)
    );
}

#[test]
fn entity_checkpoint_roundtrip_preserves_dense_active_order_ai_and_health() {
    use rules::entities::mob::{EntitySnapshot, MOB_BYTES};
    let mut store = EntityStore::new(10).unwrap();
    for id in 0..10 {
        let mut mob = Mob::new_in_space(
            EntityId(id),
            MobKind::ALL[id as usize % 5],
            core_types::SpaceId(7),
            [id as f64, 1.0, 0.0],
        )
        .unwrap();
        mob.ai.destination = Some([id as f64 + 3.0, 2.0, 1.0]);
        mob.ai.target = Some(EntityId(99));
        mob.body.jump_cooldown = 3;
        store.insert(mob, id % 2 == 0).unwrap();
    }
    let mut out = [EntityId(0); 2];
    store.active_batch(2, &mut out);
    let snapshot = store.snapshot();
    let bytes = snapshot.encode().unwrap();
    assert_eq!(EntitySnapshot::decode(&bytes, 10).unwrap(), snapshot);
    let restored = EntityStore::restore(10, &snapshot).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    let value = store.get(EntityId(1)).unwrap();
    assert_eq!(Mob::decode(&value.encode().unwrap()), Ok(*value));
    let mut corrupt = value.encode().unwrap();
    corrupt[MOB_BYTES - 1] = 1;
    assert_eq!(Mob::decode(&corrupt), Err(EntityError::InvalidEncoding));
    let mut corrupt = value.encode().unwrap();
    corrupt[16..24].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    assert_eq!(Mob::decode(&corrupt), Err(EntityError::InvalidPosition));
    assert!(matches!(
        EntitySnapshot::decode(&bytes, 9),
        Err(EntityError::Capacity)
    ));
}
#[test]
fn instance_origin_is_subtracted_before_f32_and_active_cursor_is_detached() {
    let mut store = EntityStore::new(2).unwrap();
    let mob = Mob::new(EntityId(1), MobKind::Pig, [1_000_000_000.125, 1.0, 0.0]).unwrap();
    store.insert(mob, true).unwrap();
    let mut poses = [Instance::default(); 1];
    store
        .instances_at(&[mob.id], 0.5, [1_000_000_000, 0, 0], &mut poses)
        .unwrap();
    assert_eq!(poses[0].position[0], 0.125);
    let before = store.active_cursor();
    let mut ids = [EntityId(0); 1];
    let (count, next) = store.peek_active_batch(1, &mut ids);
    assert_eq!(count, 1);
    assert_eq!(store.active_cursor(), before);
    store.publish_active_cursor(before, next).unwrap();
}
use std::collections::BTreeMap;
