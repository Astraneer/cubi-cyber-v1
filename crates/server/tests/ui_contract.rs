#[path = "../src/ui.rs"]
pub mod window;
use rules::{crafting::RecipeBook, ids::*, items::*, stack::ItemStack, survival::Inventory};
use window::{Click, WindowError, WindowKind, WindowState};
fn stack(item: u16, count: u16) -> ItemStack {
    ItemStack::new(item, count).unwrap()
}
fn click(state: &mut WindowState, inventory: &mut Inventory, slot: u16, right: bool, shift: bool) {
    let plan = state
        .plan_click(
            inventory,
            state.guard(inventory),
            Click { slot, right, shift },
        )
        .unwrap();
    assert!(plan.drops().is_empty());
    plan.apply(state, inventory).unwrap();
}
fn total(state: &WindowState, inventory: &Inventory, item: u16) -> u32 {
    inventory
        .slots()
        .iter()
        .chain(state.craft().cells())
        .chain(state.container())
        .copied()
        .chain([state.cursor()])
        .filter(|s| s.item() == item)
        .map(|s| u32::from(s.count()))
        .sum()
}
#[test]
fn split_merge_cursor_and_stale_guard_are_atomic() {
    let mut inventory = Inventory::empty();
    inventory.set_trusted(0, stack(STONE, 11)).unwrap();
    let mut state = WindowState::inventory();
    let stale = state.guard(&inventory);
    click(&mut state, &mut inventory, 0, true, false);
    assert_eq!(state.cursor().count(), 6);
    assert_eq!(inventory.get(0).unwrap().count(), 5);
    assert!(matches!(
        state.plan_click(
            &inventory,
            stale,
            Click {
                slot: 0,
                right: false,
                shift: false
            }
        ),
        Err(WindowError::Stale)
    ));
    click(&mut state, &mut inventory, 1, true, false);
    assert_eq!(state.cursor().count(), 5);
    assert_eq!(inventory.get(1).unwrap().count(), 1);
    click(&mut state, &mut inventory, 0, false, false);
    assert!(state.cursor().is_empty());
    assert_eq!(total(&state, &inventory, STONE), 11);
}
#[test]
fn no_armor_or_machine_output_insertion_but_partial_output_extraction_works() {
    let mut inventory = Inventory::empty();
    inventory.set_trusted(0, stack(STONE, 3)).unwrap();
    let mut state = WindowState::inventory();
    click(&mut state, &mut inventory, 0, false, false);
    let before = state.encode();
    assert!(matches!(
        state.plan_click(
            &inventory,
            state.guard(&inventory),
            Click {
                slot: 36,
                right: false,
                shift: false
            }
        ),
        Err(WindowError::Role)
    ));
    assert_eq!(before, state.encode());
    let mut furnace = WindowState::open(
        1,
        WindowKind::Furnace,
        &[ItemStack::EMPTY, ItemStack::EMPTY, stack(I_IRON_INGOT, 10)],
    )
    .unwrap();
    let mut inventory = Inventory::empty();
    click(&mut furnace, &mut inventory, 43, true, false);
    assert_eq!(furnace.cursor().count(), 5);
    assert_eq!(furnace.container()[2].count(), 5);
    assert!(matches!(
        furnace.plan_click(
            &inventory,
            furnace.guard(&inventory),
            Click {
                slot: 43,
                right: true,
                shift: false
            }
        ),
        Err(WindowError::Role)
    ));
}
#[test]
fn chest_quick_move_preserves_all_items_and_guards_both_values() {
    let mut inventory = Inventory::empty();
    inventory.set_trusted(0, stack(STONE, 64)).unwrap();
    let mut slots = [ItemStack::EMPTY; 27];
    slots[0] = stack(STONE, 50);
    let mut state = WindowState::open(9, WindowKind::Chest(27), &slots).unwrap();
    click(&mut state, &mut inventory, 0, false, true);
    assert!(inventory.get(0).unwrap().is_empty());
    assert_eq!(state.container()[0].count(), 64);
    assert_eq!(state.container()[1].count(), 50);
    assert_eq!(total(&state, &inventory, STONE), 114);
    let plan = state
        .plan_click(
            &inventory,
            state.guard(&inventory),
            Click {
                slot: 41,
                right: false,
                shift: true,
            },
        )
        .unwrap();
    inventory.select(1).unwrap();
    let encoded = state.encode();
    assert_eq!(
        plan.apply(&mut state, &mut inventory),
        Err(WindowError::Stale)
    );
    assert_eq!(state.encode(), encoded);
}
#[test]
fn recipe_autofill_cannot_invent_ingredients_and_exact_take_uses_cursor() {
    let book = RecipeBook::standard().unwrap();
    let recipe = book.recipes_producing(PLANKS).next().unwrap().0;
    let mut inventory = Inventory::empty();
    let mut state = WindowState::inventory();
    assert!(matches!(
        state.plan_fill(&inventory, state.guard(&inventory), &book, recipe, false),
        Err(WindowError::Ingredients)
    ));
    inventory.set_trusted(0, stack(LOG, 3)).unwrap();
    let plan = state
        .plan_fill(&inventory, state.guard(&inventory), &book, recipe, true)
        .unwrap();
    assert_eq!(plan.work.crafts, 3);
    plan.apply(&mut state, &mut inventory).unwrap();
    assert_eq!(total(&state, &inventory, LOG), 3);
    let plan = state
        .plan_take(&inventory, state.guard(&inventory), &book, false)
        .unwrap();
    plan.apply(&mut state, &mut inventory).unwrap();
    assert_eq!(state.cursor(), stack(PLANKS, 4));
    assert_eq!(total(&state, &inventory, LOG), 2);
    let plan = state
        .plan_take(&inventory, state.guard(&inventory), &book, true)
        .unwrap();
    assert_eq!(plan.work.crafts, 2);
    plan.apply(&mut state, &mut inventory).unwrap();
    assert_eq!(total(&state, &inventory, PLANKS), 12);
    assert_eq!(total(&state, &inventory, LOG), 0);
}
#[test]
fn close_returns_owned_cursor_or_explicit_drop_and_codec_resumes_exactly() {
    let mut inventory = Inventory::empty();
    inventory.set_trusted(0, stack(STONE, 64)).unwrap();
    let mut state = WindowState::inventory();
    click(&mut state, &mut inventory, 0, false, false);
    for slot in 0..36 {
        inventory.set_trusted(slot, stack(DIRT, 64)).unwrap();
    }
    let restored = WindowState::decode(&state.encode()).unwrap();
    assert_eq!(restored, state);
    let plan = state
        .plan_close(&inventory, state.guard(&inventory))
        .unwrap();
    assert_eq!(plan.drops(), &[stack(STONE, 64)]);
    plan.apply(&mut state, &mut inventory).unwrap();
    assert!(state.closed());
    assert!(state.cursor().is_empty());
    assert!(matches!(
        state.plan_click(
            &inventory,
            state.guard(&inventory),
            Click {
                slot: 0,
                right: false,
                shift: false
            }
        ),
        Err(WindowError::Closed)
    ));
    assert_eq!(WindowState::decode(&state.encode()).unwrap(), state);
}
#[test]
fn window_decoder_rejects_truncations_corrupt_stacks_and_noncanonical_slots() {
    let state = WindowState::inventory();
    let bytes = state.encode();
    assert_eq!(bytes.len(), 407);
    for end in 0..bytes.len() {
        assert!(WindowState::decode(&bytes[..end]).is_err());
    }
    let mut bad = bytes.clone();
    bad[23..25].copy_from_slice(&u16::MAX.to_le_bytes());
    bad[25] = 1;
    assert_eq!(WindowState::decode(&bad), Err(WindowError::Encoding));
    assert!(WindowState::open(1, WindowKind::Chest(28), &[ItemStack::EMPTY; 28]).is_err());
    let mut bad = bytes;
    let outside = 23 + (1 + 2) * 6;
    bad[outside..outside + 2].copy_from_slice(&STONE.to_le_bytes());
    bad[outside + 2] = 1;
    assert_eq!(WindowState::decode(&bad), Err(WindowError::Encoding));
}
#[test]
fn reopening_inventory_uses_fresh_epoch_and_rejects_old_guard() {
    let inventory = Inventory::empty();
    let initial = WindowState::inventory();
    let stale = initial.guard(&inventory);
    let fresh = WindowState::open(1, WindowKind::Inventory, &[]).unwrap();
    assert!(matches!(
        fresh.plan_click(
            &inventory,
            stale,
            Click {
                slot: 0,
                right: false,
                shift: false
            }
        ),
        Err(WindowError::Stale)
    ));
    assert_eq!(WindowState::decode(&fresh.encode()).unwrap(), fresh);
}
