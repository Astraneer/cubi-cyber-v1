//! Explicit donor golden expectations: health.rs/survival_tests.rs ASP12,
//! inventory.rs ASP08, blockentity.rs E01..E15, farm.rs/g7_tests.rs ASP19.
use core_types::{Cell, DeterministicRng};
use rules::{
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{
        Inventory, SurvivalError, farm,
        furnace::{self, Furnace},
        health::{self, DamageKind, EatOutcome, Eating, Health},
        loot,
    },
};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn stack(item: u16, count: u16) -> ItemStack {
    ItemStack::new(item, count).unwrap()
}
fn unprotected() -> Health {
    let mut health = Health::new();
    health.set_spawn_protection(0);
    health
}

#[test]
fn pickup_selected_then_offhand_then_main_then_empty_golden() {
    let mut inv = Inventory::empty();
    inv.select(4).unwrap();
    inv.set_trusted(4, stack(COBBLE, 10)).unwrap();
    inv.set_trusted(0, stack(COBBLE, 20)).unwrap();
    inv.plan_pickup(stack(COBBLE, 60))
        .unwrap()
        .apply(&mut inv)
        .unwrap();
    assert_eq!(inv.get(4), Some(stack(COBBLE, 64)));
    assert_eq!(inv.get(0), Some(stack(COBBLE, 26)));
    let mut inv = Inventory::empty();
    inv.select(5).unwrap();
    inv.set_trusted(40, stack(COBBLE, 60)).unwrap();
    inv.set_trusted(0, stack(COBBLE, 1)).unwrap();
    inv.plan_pickup(stack(COBBLE, 3))
        .unwrap()
        .apply(&mut inv)
        .unwrap();
    assert_eq!(inv.get(40), Some(stack(COBBLE, 63)));
    assert_eq!(inv.get(0), Some(stack(COBBLE, 1)));
    inv.plan_pickup(stack(DIRT, 5))
        .unwrap()
        .apply(&mut inv)
        .unwrap();
    assert_eq!(inv.get(1), Some(stack(DIRT, 5)));
    assert!(inv.hand().is_empty());
}
#[test]
fn damaged_tools_do_not_merge_and_full_inventory_returns_exact_remainder() {
    let pick = ItemStack::from_parts(I_WOODEN_PICKAXE, 1, 3).unwrap();
    let mut inv = Inventory::empty();
    inv.set_trusted(0, pick).unwrap();
    inv.set_trusted(1, stack(DIRT, 1)).unwrap();
    inv.plan_pickup(pick).unwrap().apply(&mut inv).unwrap();
    assert_eq!(inv.get(2), Some(pick));
    for index in 0..36 {
        inv.set_trusted(index, stack(COBBLE, 64)).unwrap();
    }
    inv.set_trusted(7, stack(COBBLE, 62)).unwrap();
    let plan = inv.plan_pickup(stack(COBBLE, 5)).unwrap();
    assert_eq!(plan.remainder(), stack(COBBLE, 3));
    plan.apply(&mut inv).unwrap();
    assert_eq!(inv.get(7), Some(stack(COBBLE, 64)));
    let plan = inv.plan_pickup(stack(DIRT, 1)).unwrap();
    assert_eq!(plan.remainder(), stack(DIRT, 1));
    assert_eq!(plan.before(), plan.after());
}
#[test]
fn selection_and_drop_match_donor_one_step_wrap() {
    let mut inv = Inventory::empty();
    inv.scroll(3).unwrap();
    assert_eq!(inv.selected(), 8);
    inv.scroll(-1).unwrap();
    assert_eq!(inv.selected(), 0);
    assert_eq!(inv.select(9), Err(SurvivalError::InvalidSlot));
    inv.set_trusted(0, stack(COBBLE, 4)).unwrap();
    let plan = inv.plan_drop_selected(false).unwrap();
    assert_eq!(plan.remainder(), stack(COBBLE, 1));
    plan.apply(&mut inv).unwrap();
    assert_eq!(inv.hand(), stack(COBBLE, 3));
    let plan = inv.plan_drop_selected(true).unwrap();
    assert_eq!(plan.remainder(), stack(COBBLE, 3));
    plan.apply(&mut inv).unwrap();
    assert!(inv.hand().is_empty());
}
#[test]
fn placement_requires_owned_selected_matching_block_and_stale_plan_is_atomic() {
    let mut inv = Inventory::empty();
    assert_eq!(
        inv.plan_place(stack(STONE, 1), Cell::from_parts(STONE, 0)),
        Err(SurvivalError::NoEntitlement)
    );
    inv.set_trusted(0, stack(COBBLE, 3)).unwrap();
    assert_eq!(
        inv.plan_place(inv.hand(), Cell::from_parts(STONE, 0)),
        Err(SurvivalError::NoEntitlement)
    );
    let plan = inv
        .plan_place(inv.hand(), Cell::from_parts(COBBLE, 0))
        .unwrap();
    let original = inv;
    inv.select(1).unwrap();
    let changed = inv;
    assert_eq!(plan.apply(&mut inv), Err(SurvivalError::StalePlan));
    assert_eq!(inv, changed);
    inv = original;
    let plan = inv
        .plan_place(inv.hand(), Cell::from_parts(COBBLE, 0))
        .unwrap();
    let replay = plan;
    plan.apply(&mut inv).unwrap();
    assert_eq!(inv.hand(), stack(COBBLE, 2));
    assert_eq!(replay.apply(&mut inv), Err(SurvivalError::StalePlan));
}
#[test]
fn codec_roundtrip_rejects_forged_counts_armor_and_invalid_hotbar() {
    let mut inv = Inventory::empty();
    inv.set_trusted(3, ItemStack::from_parts(I_WOODEN_PICKAXE, 1, 58).unwrap())
        .unwrap();
    inv.select(3).unwrap();
    let bytes = inv.encode();
    assert_eq!(Inventory::decode(&bytes), Ok(inv));
    let mut forged = bytes;
    forged[4] = 9;
    assert_eq!(
        Inventory::decode(&forged),
        Err(SurvivalError::InvalidEncoding)
    );
    let mut forged = bytes;
    forged[14 + 3 * 6 + 2..14 + 3 * 6 + 4].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(Inventory::decode(&forged), Err(SurvivalError::InvalidStack));
    assert_eq!(
        inv.set_trusted(36, stack(COBBLE, 1)),
        Err(SurvivalError::InvalidArmorSlot)
    );
    assert_eq!(
        Inventory::decode(&bytes[..bytes.len() - 1]),
        Err(SurvivalError::InvalidEncoding)
    );
}
#[test]
fn mine_checks_harvest_before_last_tool_break_and_preserves_loot() {
    let mut inv = Inventory::empty();
    inv.set_trusted(0, ItemStack::from_parts(I_WOODEN_PICKAXE, 1, 58).unwrap())
        .unwrap();
    let mut rng = DeterministicRng::from_key(7, 8, 9, 10);
    let plan = inv
        .plan_mine(inv.hand(), Cell::from_parts(STONE, 0), &mut rng)
        .unwrap();
    assert!(plan.inventory.broken());
    assert_eq!(plan.drops.stacks(), &[stack(COBBLE, 1)]);
    plan.inventory.apply(&mut inv).unwrap();
    assert!(inv.hand().is_empty());
    let plan = inv
        .plan_mine(ItemStack::EMPTY, Cell::from_parts(STONE, 0), &mut rng)
        .unwrap();
    assert!(plan.drops.stacks().is_empty());
    assert_eq!(
        inv.plan_mine(ItemStack::EMPTY, Cell::from_parts(BEDROCK, 0), &mut rng),
        Err(SurvivalError::NoEntitlement)
    );
}
#[test]
fn death_clears_all_slots_including_armor_and_offhand_once() {
    let mut inv = Inventory::empty();
    inv.set_trusted(0, stack(COBBLE, 64)).unwrap();
    inv.set_trusted(40, stack(TORCH, 3)).unwrap();
    inv.set_trusted(39, stack(I_LEATHER_HELMET, 1)).unwrap();
    let plan = inv.plan_death().unwrap();
    assert_eq!(
        plan.before()
            .slots()
            .iter()
            .map(|v| u32::from(v.count()))
            .sum::<u32>(),
        68
    );
    assert!(plan.after().slots().iter().all(|v| v.is_empty()));
    let replay = plan;
    plan.apply(&mut inv).unwrap();
    assert_eq!(replay.apply(&mut inv), Err(SurvivalError::StalePlan));
}
#[test]
fn fall_damage_golden_rounding_and_hay_multiplier() {
    for (distance, expected) in [
        (3.0, 0),
        (3.0000002, 1),
        (4.0, 1),
        (23.0, 20),
        (2.6945615, 0),
        (3.3462703, 1),
        (9.667067, 7),
        (21.833506, 19),
        (23.435236, 21),
        (4.0363092, 2),
        (4.815027, 2),
        (10.763565, 8),
        (23.374851, 21),
    ] {
        assert_eq!(health::fall_damage(distance, 1.0), expected);
    }
    assert_eq!(health::fall_damage(23.0, 0.2), 4);
}
#[test]
fn damage_window_golden_and_void_ignores_protection() {
    let mut hp = unprotected();
    assert_eq!(
        hp.damage(DamageKind::Generic, 6.0, (0, 0.0), false)
            .unwrap()
            .taken,
        6.0
    );
    for _ in 0..5 {
        hp.begin_tick();
        hp.end_tick();
    }
    assert!(
        hp.damage(DamageKind::Generic, 4.0, (0, 0.0), false)
            .is_none()
    );
    let hit = hp
        .damage(DamageKind::Generic, 9.0, (0, 0.0), false)
        .unwrap();
    assert!(!hit.full);
    assert_eq!(hit.taken, 3.0);
    for _ in 0..5 {
        hp.begin_tick();
        hp.end_tick();
    }
    assert!(
        hp.damage(DamageKind::Generic, 4.0, (0, 0.0), false)
            .unwrap()
            .full
    );
    assert_eq!(hp.hp(), 7.0);
    let mut hp = Health::new();
    assert!(
        hp.damage(DamageKind::Generic, 5.0, (0, 0.0), false)
            .is_none()
    );
    assert!(
        hp.damage(DamageKind::OutOfWorld, 4.0, (30, 8.0), true)
            .is_some()
    );
    assert_eq!(hp.hp(), 16.0);
}
#[test]
fn armor_golden_cactus_wears_each_piece_fall_bypasses() {
    let mut inv = Inventory::empty();
    for offset in 0..4 {
        let id = I_LEATHER_HELMET + 4 + offset;
        let ItemKind::Armor { slot, .. } = idef(id).kind else {
            panic!("armor")
        };
        inv.set_trusted(36 + usize::from(slot), stack(id, 1))
            .unwrap();
    }
    assert_eq!(inv.armor(), (15, 0.0));
    let mut hp = unprotected();
    let hit = hp
        .damage(DamageKind::Cactus, 1.0, inv.armor(), false)
        .unwrap();
    assert!((hit.taken - 0.42).abs() < 1e-5);
    assert_eq!(hit.armor_wear, 1);
    inv.plan_armor_wear(hit.armor_wear)
        .unwrap()
        .apply(&mut inv)
        .unwrap();
    assert!(inv.slots()[36..40].iter().all(|value| value.damage() == 1));
    let mut hp = unprotected();
    let hit = hp
        .damage(DamageKind::Fall, 7.0, inv.armor(), false)
        .unwrap();
    assert_eq!((hit.taken, hit.armor_wear), (7.0, 0));
}
#[test]
fn drowning_320_then_every20_and_water_extinguishes() {
    let mut hp = unprotected();
    let mut times = Vec::new();
    for tick in 1..=360 {
        hp.begin_tick();
        if let Some(damage) = hp.tick_air(true) {
            times.push(tick);
            hp.damage(DamageKind::Drown, damage, (0, 0.0), false);
        }
        hp.end_tick();
    }
    assert_eq!(times, [320, 340, 360]);
    assert_eq!(hp.hp(), 14.0);
    hp.ignite(300);
    assert_eq!(hp.tick_fire(true, false), None);
    assert_eq!(hp.fire(), 0);
    hp.tick_air(false);
    assert_eq!(hp.air(), 4);
}
#[test]
fn regen_first_at140_and_every40_without_damage() {
    let mut hp = unprotected();
    hp.damage(DamageKind::Generic, 10.0, (0, 0.0), false);
    let mut times = Vec::new();
    for tick in 1..=220 {
        hp.begin_tick();
        if hp.end_tick() {
            times.push(tick);
        }
    }
    assert_eq!(times, [140, 180, 220]);
    assert_eq!(hp.hp(), 13.0);
}
#[test]
fn food_heals_only_after32_held_ticks_and_damage_does_not_cancel() {
    let mut hp = unprotected();
    assert!(Eating::start(&hp, &Inventory::empty()).is_none());
    hp.damage(DamageKind::Generic, 10.0, (0, 0.0), false);
    let mut inv = Inventory::empty();
    inv.set_trusted(0, stack(I_APPLE, 2)).unwrap();
    let mut eat = Eating::start(&hp, &inv).unwrap();
    for _ in 0..31 {
        assert_eq!(eat.tick(true, &mut hp, &mut inv), Ok(EatOutcome::Eating));
    }
    assert_eq!(hp.hp(), 10.0);
    assert_eq!(inv.hand().count(), 2);
    assert_eq!(eat.tick(true, &mut hp, &mut inv), Ok(EatOutcome::Finished));
    assert_eq!(hp.hp(), 14.0);
    assert_eq!(inv.hand().count(), 1);
    assert_eq!(eat.tick(true, &mut hp, &mut inv), Ok(EatOutcome::Cancelled));
    assert_eq!(eat.tick(true, &mut hp, &mut inv), Ok(EatOutcome::Cancelled));
    let mut eat = Eating::start(&hp, &inv).unwrap();
    assert_eq!(
        eat.tick(false, &mut hp, &mut inv),
        Ok(EatOutcome::Cancelled)
    );
    assert_eq!(inv.hand().count(), 1);
    let mut eat = Eating::start(&hp, &inv).unwrap();
    hp.begin_tick();
    hp.damage(DamageKind::Generic, 12.0, (0, 0.0), false);
    assert_eq!(eat.tick(true, &mut hp, &mut inv), Ok(EatOutcome::Eating));
}

#[test]
fn chest_transfer_is_conservative_and_guards_both_owners() {
    use rules::survival::furnace::Chest;
    let mut inv = Inventory::empty();
    inv.set_trusted(0, stack(COBBLE, 10)).unwrap();
    let mut chest = Chest::default();
    chest.set_trusted(4, stack(COBBLE, 60)).unwrap();
    assert_eq!(
        chest.plan_transfer(&inv, 4, 0, 5, true),
        Err(SurvivalError::NoEntitlement)
    );
    let plan = chest.plan_transfer(&inv, 4, 0, 4, true).unwrap();
    let replay = plan;
    plan.apply(&mut inv, &mut chest).unwrap();
    assert_eq!(inv.hand(), stack(COBBLE, 6));
    assert_eq!(chest.slots()[4], stack(COBBLE, 64));
    let original = (inv, chest);
    assert_eq!(
        replay.apply(&mut inv, &mut chest),
        Err(SurvivalError::StalePlan)
    );
    assert_eq!((inv, chest), original);
    let plan = chest.plan_transfer(&inv, 4, 0, 3, false).unwrap();
    chest.set_trusted(7, stack(DIRT, 1)).unwrap();
    let original = (inv, chest);
    assert_eq!(
        plan.apply(&mut inv, &mut chest),
        Err(SurvivalError::StalePlan)
    );
    assert_eq!((inv, chest), original);
    assert_eq!(Chest::decode(&chest.encode()), Ok(chest));
}
#[test]
fn inventory_craft_owns_output_and_grid_as_one_transaction() {
    use rules::crafting::{Grid, RecipeBook};
    let book = RecipeBook::standard().unwrap();
    let mut grid = Grid::new(2, 2).unwrap();
    grid.set(0, 0, stack(LOG, 1)).unwrap();
    let mut inv = Inventory::empty();
    let plan = inv.plan_craft(&book, &grid, 1).unwrap();
    assert_eq!(plan.inventory_after().get(0), Some(stack(PLANKS, 4)));
    plan.apply(&mut inv, &mut grid).unwrap();
    assert!(grid.cells().iter().all(|value| value.is_empty()));
    assert_eq!(inv.hand(), stack(PLANKS, 4));
    grid.set(0, 0, stack(LOG, 1)).unwrap();
    let plan = inv.plan_craft(&book, &grid, 1).unwrap();
    grid.set(1, 1, stack(DIRT, 1)).unwrap();
    let original = (inv, grid);
    assert_eq!(
        plan.apply(&mut inv, &mut grid),
        Err(SurvivalError::StalePlan)
    );
    assert_eq!((inv, grid), original);
}
#[test]
fn health_codec_roundtrip_and_nonfinite_rejected_death_respawn_delay() {
    let mut hp = unprotected();
    hp.damage(DamageKind::Generic, 100.0, (0, 0.0), false);
    for _ in 0..19 {
        hp.begin_tick();
        hp.end_tick();
    }
    assert!(!hp.may_respawn());
    hp.begin_tick();
    hp.end_tick();
    assert!(hp.may_respawn());
    assert_eq!(Health::decode(&hp.encode()), Ok(hp));
    let mut bytes = hp.encode();
    bytes[4..8].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert_eq!(Health::decode(&bytes), Err(SurvivalError::InvalidEncoding));
}
#[test]
fn furnace_coal_eight_ingots_exact_golden_ticks() {
    let mut f = Furnace::default();
    f.set_trusted(0, stack(I_RAW_IRON, 8)).unwrap();
    f.set_trusted(1, stack(I_COAL, 1)).unwrap();
    let mut lit = Vec::new();
    let mut products = Vec::new();
    for tick in 0..1700 {
        let step = f.tick();
        if let Some(lit_state) = step.lit_change {
            lit.push((tick, lit_state));
        }
        if step.smelted.is_some() {
            products.push(tick);
        }
        if tick == 0 {
            assert_eq!((f.burn(), f.fuel_total(), f.cook()), (1600, 1600, 1));
            assert!(f.slots()[1].is_empty());
        }
    }
    assert_eq!(lit, [(0, true), (1600, false)]);
    assert_eq!(products, [199, 399, 599, 799, 999, 1199, 1399, 1599]);
    assert_eq!(f.slots()[2], stack(I_IRON_INGOT, 8));
    assert!(!f.needs_tick());
}
#[test]
fn furnace_blocked_output_no_fuel_and_lava_returns_bucket() {
    let mut f = Furnace::default();
    f.set_trusted(0, stack(I_RAW_IRON, 2)).unwrap();
    f.set_trusted(1, stack(I_COAL, 1)).unwrap();
    f.set_trusted(2, stack(I_CHARCOAL, 1)).unwrap();
    assert!(!f.needs_tick());
    for _ in 0..100 {
        f.tick();
    }
    assert_eq!(f.slots()[1], stack(I_COAL, 1));
    assert_eq!(f.burn(), 0);
    f.set_trusted(2, ItemStack::EMPTY).unwrap();
    f.set_trusted(1, stack(I_LAVA_BUCKET, 1)).unwrap();
    f.tick();
    assert_eq!((f.burn(), f.slots()[1]), (20_000, stack(I_BUCKET, 1)));
    assert_eq!(Furnace::decode(&f.encode()), Ok(f));
    let mut bytes = f.encode();
    bytes[26..28].copy_from_slice(&200u16.to_le_bytes());
    assert_eq!(Furnace::decode(&bytes), Err(SurvivalError::InvalidEncoding));
}
#[test]
fn furnace_progress_cools_two_per_tick_and_input_change_resets() {
    let mut f = Furnace::default();
    f.set_trusted(0, stack(I_RAW_IRON, 2)).unwrap();
    f.set_trusted(1, stack(I_STICK, 1)).unwrap();
    for _ in 0..100 {
        f.tick();
    }
    assert_eq!(f.cook(), 100);
    f.tick();
    assert_eq!(f.cook(), 98);
    for _ in 0..49 {
        f.tick();
    }
    assert_eq!(f.cook(), 0);
    f.set_trusted(1, stack(I_COAL, 1)).unwrap();
    for _ in 0..20 {
        f.tick();
    }
    assert_eq!(f.cook(), 20);
    f.set_trusted(0, stack(I_RAW_IRON, 3)).unwrap();
    assert_eq!(f.cook(), 20);
    f.set_trusted(0, stack(SAND, 3)).unwrap();
    assert_eq!(f.cook(), 0);
    assert_eq!(
        [
            furnace::fuel_ticks(I_WOODEN_PICKAXE),
            furnace::fuel_ticks(I_STICK),
            furnace::fuel_ticks(LOG),
            furnace::fuel_ticks(I_COAL),
            furnace::fuel_ticks(I_BUCKET)
        ],
        [200, 100, 300, 1600, 0]
    );
}
#[test]
fn farm_moisture_spacing_and_growth_golden() {
    let position = [0, 1, 0];
    let query = |p: [i32; 3]| {
        Some(if p[1] == 0 && p[0].abs() <= 1 && p[2].abs() <= 1 {
            Cell::from_parts(FARMLAND, 7)
        } else {
            Cell::AIR
        })
    };
    let (factor, reads) = farm::moisture_factor(query, position, WHEAT).unwrap();
    assert_eq!((factor, reads), (10.0, 17));
    assert_eq!(farm::growth_denominator(factor), 3);
    let crowded = |p: [i32; 3]| {
        Some(if p[1] == 0 {
            Cell::from_parts(FARMLAND, 7)
        } else if p == [1, 1, 1] {
            Cell::from_parts(WHEAT, 0)
        } else {
            Cell::AIR
        })
    };
    assert_eq!(
        farm::moisture_factor(crowded, position, WHEAT).unwrap().0,
        5.0
    );
    let query = |p: [i32; 3]| {
        Some(if p == position {
            Cell::from_parts(WHEAT, 0)
        } else if p[1] == 0 {
            Cell::from_parts(FARMLAND, 7)
        } else {
            Cell::AIR
        })
    };
    let step = farm::random_tick(query, position, 9, 0).unwrap();
    assert_eq!(step.change.unwrap().after, Cell::from_parts(WHEAT, 1));
    assert_eq!(step.reads, 18);
    assert!(
        farm::random_tick(query, position, 8, 0)
            .unwrap()
            .change
            .is_none()
    );
}
#[test]
fn farmland_water_radius_four_drying_and_unknown_defer() {
    let p = [0, 0, 0];
    let query = |q: [i32; 3]| {
        Some(if q == p {
            Cell::from_parts(FARMLAND, 0)
        } else if q == [4, 1, 4] {
            Cell::from_parts(WATER, 7)
        } else {
            Cell::AIR
        })
    };
    let step = farm::random_tick(query, p, 0, 0).unwrap();
    assert_eq!(step.change.unwrap().after, Cell::from_parts(FARMLAND, 7));
    assert!(step.reads <= 163);
    let query = |q: [i32; 3]| {
        Some(if q == p {
            Cell::from_parts(FARMLAND, 1)
        } else {
            Cell::AIR
        })
    };
    assert_eq!(
        farm::random_tick(query, p, 0, 0)
            .unwrap()
            .change
            .unwrap()
            .after,
        Cell::from_parts(FARMLAND, 0)
    );
    assert_eq!(
        farm::random_tick(
            |q| if q == p {
                Some(Cell::from_parts(FARMLAND, 7))
            } else {
                None
            },
            p,
            0,
            0
        ),
        Err(SurvivalError::UnknownInput)
    );
}
#[test]
fn farm_till_plant_and_trample_are_inventory_guarded() {
    let p = [0; 3];
    let mut inv = Inventory::empty();
    let hoe = stack(I_WOODEN_PICKAXE + 3, 1);
    inv.set_trusted(0, hoe).unwrap();
    let plan = farm::till(
        &inv,
        hoe,
        p,
        Cell::from_parts(DIRT, 0),
        Some(Cell::AIR),
        false,
    )
    .unwrap();
    assert_eq!(plan.cell.after, Cell::from_parts(FARMLAND, 0));
    plan.inventory.apply(&mut inv).unwrap();
    assert_eq!(inv.hand().damage(), 1);
    inv.set_trusted(0, stack(I_WHEAT_SEEDS, 2)).unwrap();
    let plan = farm::plant(
        &inv,
        inv.hand(),
        [0, 1, 0],
        Cell::AIR,
        Some(Cell::from_parts(FARMLAND, 0)),
        8,
    )
    .unwrap();
    assert_eq!(plan.cell.after, Cell::from_parts(WHEAT, 0));
    plan.inventory.apply(&mut inv).unwrap();
    assert_eq!(inv.hand().count(), 1);
    assert!(farm::trample(p, Cell::from_parts(FARMLAND, 7), 1.0, true, 0.49).is_some());
    assert!(farm::trample(p, Cell::from_parts(FARMLAND, 7), 1.0, true, 0.5).is_none());
    assert!(farm::trample(p, Cell::from_parts(FARMLAND, 7), 10.0, false, 0.0).is_none());
}
#[test]
fn crop_loot_matches_donor_histogram_and_table_domains() {
    let mut rng = DeterministicRng::from_key(9, 8, 7, 6);
    for age in 0..7 {
        assert_eq!(
            loot::block_drops(Cell::from_parts(WHEAT, age), true, false, &mut rng)
                .unwrap()
                .stacks(),
            &[stack(I_WHEAT_SEEDS, 1)]
        );
    }
    let mut hist = [0u32; 5];
    let mut carrots = 0u32;
    for _ in 0..100_000 {
        let drop = loot::block_drops(Cell::from_parts(WHEAT, 7), true, false, &mut rng).unwrap();
        assert_eq!(drop.stacks()[0], stack(I_WHEAT, 1));
        hist[drop.stacks()[1].count() as usize] += 1;
        carrots += loot::block_drops(Cell::from_parts(CARROTS, 7), true, false, &mut rng)
            .unwrap()
            .stacks()
            .iter()
            .map(|stack| u32::from(stack.count()))
            .sum::<u32>();
    }
    for (index, want) in [(1, 0.0787), (2, 0.3149), (3, 0.4198), (4, 0.1866)] {
        assert!((f64::from(hist[index]) / 100_000.0 - want).abs() < 0.008);
    }
    assert!((f64::from(carrots) / 100_000.0 - 3.7143).abs() < 0.03);
    assert_eq!(
        loot::block_drops(Cell::from_parts(CLAY, 0), true, false, &mut rng)
            .unwrap()
            .stacks(),
        &[stack(I_CLAY_BALL, 4)]
    );
}
#[test]
fn hot_value_paths_no_allocations_with_positive_control() {
    let control = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(43));
    black_box(&allocation);
    assert!(control.finish().allocating_calls() > 0);
    drop(allocation);
    let mut inv = Inventory::empty();
    inv.set_trusted(0, stack(COBBLE, 64)).unwrap();
    let hp = unprotected();
    let mut furnace = Furnace::default();
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let _ = black_box(
            inv.plan_place(inv.hand(), Cell::from_parts(COBBLE, 0))
                .unwrap(),
        );
        black_box(Inventory::decode(black_box(&inv.encode())).unwrap());
        black_box(Health::decode(black_box(&hp.encode())).unwrap());
        black_box(furnace.tick());
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
