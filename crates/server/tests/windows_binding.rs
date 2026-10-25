use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick};
use net::window::{WindowGuard, WindowIntent};
use rules::{
    blocks::MekKind,
    crafting::RecipeBook,
    ids::*,
    machines::{Machine, SideMode, Slot},
    stack::ItemStack,
    survival::{
        Inventory,
        furnace::{Chest, Furnace},
    },
};
use server::{survival, ui, windows};
use windows::{BoundWindow, Context, Error};
use world::{Transaction, World};
fn at(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn cell(id: u16) -> Cell {
    Cell::from_parts(id, 0)
}
fn target() -> BlockPos {
    at(8, 2, 11)
}
fn fixture(id: u16, component: Option<world::ComponentData>) -> World {
    let mut world = World::new(rules::properties, 1024);
    let mut cells = vec![Cell::AIR; 4096];
    for x in 0..16 {
        for z in 0..16 {
            cells[x + z * 16] = cell(STONE);
        }
    }
    cells[target().local_index().get() as usize] = cell(id);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(at(0, 0, 0).page(), cells);
    if let Some(data) = component {
        tx.set_component(ComponentId(500), data, ChannelMask::CONTENTS);
    }
    world.commit(tx).unwrap();
    world
}
fn player() -> sim::Player {
    sim::Player::new([8.5, 1.0, 8.5])
}
fn context<'a>(player: &'a sim::Player, inventory: &'a Inventory) -> Context<'a> {
    Context {
        player,
        inventory,
        space: SpaceId(0),
        permitted: true,
    }
}
fn guard(window: &BoundWindow, inventory: &Inventory) -> WindowGuard {
    let g = window.state.guard(inventory);
    WindowGuard {
        id: g.id,
        revision: g.revision,
        inventory_revision: g.inventory_revision,
    }
}
fn apply(
    world: &mut World,
    window: &mut BoundWindow,
    inventory: &mut Inventory,
    mut plan: windows::WindowPlan,
) {
    plan.validate(world, window, inventory).unwrap();
    let before = window.clone();
    let inv = *inventory;
    if let Some(tx) = plan.take_transaction() {
        world.commit(tx).unwrap();
    }
    assert_eq!(*window, before);
    assert_eq!(
        *inventory, inv,
        "world staging does not silently publish private inventory"
    );
    plan.apply_after_durable(window, inventory).unwrap();
}
fn open(world: &mut World, window: &mut BoundWindow, inventory: &mut Inventory, id: u16) {
    let p = player();
    let book = RecipeBook::standard().unwrap();
    let plan = windows::plan(
        world,
        &context(&p, inventory),
        window,
        WindowIntent::OpenBlock {
            position: target(),
            expected: cell(id),
        },
        window.state.id() + 1,
        Tick(2),
        &book,
    )
    .unwrap();
    apply(world, window, inventory, plan);
}
#[test]
fn lazy_block_creation_and_real_chest_click_commit_contents_and_cursor_once() {
    let book = RecipeBook::standard().unwrap();
    let p = player();
    let mut inv = Inventory::empty();
    let mut window = BoundWindow::inventory(1).unwrap();
    let mut world = fixture(CHEST, None);
    open(&mut world, &mut window, &mut inv, CHEST);
    let id = window.binding.unwrap().component.unwrap();
    assert_eq!(
        survival::CHEST_COMPONENT,
        world.component(id).unwrap().data.kind
    );
    assert_eq!(BoundWindow::decode(&window.encode()).unwrap(), window);
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(STONE, 23).unwrap())
        .unwrap();
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_component(
        id,
        survival::chest_component(target(), &chest),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    assert!(window.refresh(&world).unwrap());
    let g = guard(&window, &inv);
    let intent = WindowIntent::Click {
        guard: g,
        slot: 41,
        right: true,
        shift: false,
    };
    let plan = windows::plan(
        &world,
        &context(&p, &inv),
        &window,
        intent,
        0,
        Tick(4),
        &book,
    )
    .unwrap();
    assert_eq!(plan.window_after().state.cursor().count(), 12);
    assert_eq!(plan.inventory_after(), &inv);
    assert!(plan.drops().is_empty());
    apply(&mut world, &mut window, &mut inv, plan);
    let chest = Chest::decode(&world.component(id).unwrap().data.payload).unwrap();
    assert_eq!(chest.slots()[0].count(), 11);
    assert!(matches!(
        windows::plan(
            &world,
            &context(&p, &inv),
            &window,
            intent,
            0,
            Tick(5),
            &book
        ),
        Err(Error::Window(ui::WindowError::Stale))
    ));
    let snapshot = windows::snapshot(&world, &window, &inv, &book).unwrap();
    assert_eq!(snapshot.cursor.count, 12);
    assert_eq!(snapshot.container[0].count, 11);
}
#[test]
fn component_changes_and_binding_aba_reject_stale_plans_without_private_mutation() {
    let mut world = fixture(CHEST, None);
    let mut inv = Inventory::empty();
    let mut window = BoundWindow::inventory(1).unwrap();
    let p = player();
    let book = RecipeBook::standard().unwrap();
    open(&mut world, &mut window, &mut inv, CHEST);
    inv.set_trusted(0, ItemStack::new(STONE, 4).unwrap())
        .unwrap();
    let plan = windows::plan(
        &world,
        &context(&p, &inv),
        &window,
        WindowIntent::Click {
            guard: guard(&window, &inv),
            slot: 0,
            right: false,
            shift: true,
        },
        0,
        Tick(3),
        &book,
    )
    .unwrap();
    let before = window.clone();
    let before_inv = inv;
    let id = window.binding.unwrap().component.unwrap();
    let mut chest = Chest::default();
    chest
        .set_trusted(1, ItemStack::new(DIRT, 8).unwrap())
        .unwrap();
    let mut tx = Transaction::new(Tick(4), SpaceId(0));
    tx.set_component(
        id,
        survival::chest_component(target(), &chest),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    assert_eq!(plan.validate(&world, &window, &inv), Err(Error::Stale));
    assert_eq!(window, before);
    assert_eq!(inv, before_inv);
    let mut tx = Transaction::new(Tick(5), SpaceId(0));
    tx.remove_component(id, ChannelMask::CONTENTS);
    tx.set_component(
        ComponentId(501),
        survival::chest_component(target(), &Chest::default()),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    assert_eq!(window.refresh(&world), Err(Error::Stale));
}
#[test]
fn furnace_progress_and_charged_machine_items_survive_unrelated_configuration() {
    let book = RecipeBook::standard().unwrap();
    let p = player();
    let mut inv = Inventory::empty();
    let mut window = BoundWindow::inventory(1).unwrap();
    let mut furnace = Furnace::default();
    furnace
        .set_trusted(0, ItemStack::new(rules::items::I_RAW_IRON, 2).unwrap())
        .unwrap();
    furnace
        .set_trusted(1, ItemStack::new(rules::items::I_COAL, 1).unwrap())
        .unwrap();
    for _ in 0..43 {
        furnace.tick();
    }
    let mut world = fixture(
        FURNACE,
        Some(survival::furnace_component(target(), &furnace)),
    );
    open(&mut world, &mut window, &mut inv, FURNACE);
    let state = windows::snapshot(&world, &window, &inv, &book).unwrap();
    assert!(state.progress > 0 && state.fuel > 0);
    let plan = windows::plan(
        &world,
        &context(&p, &inv),
        &window,
        WindowIntent::Click {
            guard: guard(&window, &inv),
            slot: 41,
            right: true,
            shift: false,
        },
        0,
        Tick(4),
        &book,
    )
    .unwrap();
    apply(&mut world, &mut window, &mut inv, plan);
    let stored = Furnace::decode(&world.component(ComponentId(500)).unwrap().data.payload).unwrap();
    assert_eq!(stored.cook(), furnace.cook());
    assert_eq!(stored.burn(), furnace.burn());
    let mut machine = Machine::new(MekKind::Cube);
    machine.slots[0] = Slot::tablet(4000);
    machine.energy = 1234;
    let mut bytes = [0; rules::machines::MACHINE_BYTES];
    let used = machine.encode(&mut bytes);
    let data = world::ComponentData {
        anchor: target(),
        kind: 0x8201,
        payload: bytes[..used].to_vec(),
    };
    let mut world = fixture(ENERGY_CUBE, Some(data));
    let mut window = BoundWindow::inventory(1).unwrap();
    open(&mut world, &mut window, &mut inv, ENERGY_CUBE);
    let intent = WindowIntent::Click {
        guard: guard(&window, &inv),
        slot: 41,
        right: false,
        shift: false,
    };
    assert!(matches!(
        windows::plan(
            &world,
            &context(&p, &inv),
            &window,
            intent,
            0,
            Tick(4),
            &book
        ),
        Err(Error::UnsupportedTypedItem)
    ));
    let plan = windows::plan(
        &world,
        &context(&p, &inv),
        &window,
        WindowIntent::MachineSide {
            guard: guard(&window, &inv),
            face: 0,
            mode: SideMode::Both as u8,
        },
        0,
        Tick(4),
        &book,
    )
    .unwrap();
    apply(&mut world, &mut window, &mut inv, plan);
    let after = Machine::decode(&world.component(ComponentId(500)).unwrap().data.payload).unwrap();
    assert_eq!(after.energy, 1234);
    assert_eq!(after.slots, machine.slots);
    assert_eq!(after.sides[0], SideMode::Both);
}
#[test]
fn hidden_blocks_do_not_leak_expected_ids_and_close_returns_owned_cursor_out_of_reach() {
    let book = RecipeBook::standard().unwrap();
    let p = player();
    let inv = Inventory::empty();
    let window = BoundWindow::inventory(1).unwrap();
    for hidden in [STONE, IRON_ORE] {
        let mut world = fixture(hidden, None);
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(at(8, 2, 10), cell(STONE));
        world.commit(tx).unwrap();
        assert!(matches!(
            windows::plan(
                &world,
                &context(&p, &inv),
                &window,
                WindowIntent::OpenBlock {
                    position: target(),
                    expected: cell(CHEST)
                },
                2,
                Tick(3),
                &book
            ),
            Err(Error::Occluded)
        ));
    }
    let mut world = fixture(CHEST, None);
    let mut inv = Inventory::empty();
    inv.set_trusted(0, ItemStack::new(STONE, 11).unwrap())
        .unwrap();
    let mut window = BoundWindow::inventory(1).unwrap();
    open(&mut world, &mut window, &mut inv, CHEST);
    let plan = windows::plan(
        &world,
        &context(&p, &inv),
        &window,
        WindowIntent::Click {
            guard: guard(&window, &inv),
            slot: 0,
            right: false,
            shift: false,
        },
        0,
        Tick(3),
        &book,
    )
    .unwrap();
    apply(&mut world, &mut window, &mut inv, plan);
    let mut far = p;
    far.teleport([1000.0, 1.0, 1000.0]).unwrap();
    let plan = windows::plan(
        &world,
        &context(&far, &inv),
        &window,
        WindowIntent::Close {
            guard: guard(&window, &inv),
        },
        0,
        Tick(4),
        &book,
    )
    .unwrap();
    apply(&mut world, &mut window, &mut inv, plan);
    assert_eq!(inv.get(0).unwrap().count(), 11);
    assert!(window.state.cursor().is_empty());
    assert!(window.state.closed());
    assert!(
        windows::snapshot(&world, &window, &inv, &book)
            .unwrap()
            .closed
    );
}
