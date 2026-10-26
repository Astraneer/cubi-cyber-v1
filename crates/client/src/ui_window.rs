//! Validated public window DTO to shared UI, with no local item mutation.
use net::window::{Stack, WindowKind, WindowSnapshot};
use rules::stack::ItemStack;
use ui::app::{Screen, UiApp};
fn stack(raw: Stack) -> Result<ItemStack, &'static str> {
    let value = ItemStack::from_parts(raw.item, raw.count, raw.damage)
        .map_err(|_| "invalid authoritative window stack")?;
    if raw.payload == 0 {
        Ok(value)
    } else {
        value
            .with_payload(rules::item_payload::PayloadId(raw.payload))
            .map_err(|_| "invalid authoritative window payload")
    }
}
pub fn apply_window(ui: &mut UiApp, state: &WindowSnapshot) -> Result<bool, &'static str> {
    if state.guard.inventory_revision != ui.snapshot.inventory_revision {
        return Err("window awaits matching inventory");
    }
    net::window::validate_snapshot(state).map_err(|_| "invalid authoritative window")?;
    let mut snapshot = ui.snapshot.clone();
    let old_id = snapshot.window_id;
    let mut target = match state.kind {
        WindowKind::Inventory => Screen::Inventory,
        WindowKind::Workbench => Screen::Workbench,
        WindowKind::Chest(_) => Screen::Container,
        WindowKind::Furnace => Screen::Furnace,
        WindowKind::Machine(_) => Screen::Machine,
    };
    if state.closed {
        snapshot.window_id = 0;
        snapshot.window_revision = 0;
        snapshot.cursor = ItemStack::EMPTY;
        snapshot.craft = [ItemStack::EMPTY; 9];
        snapshot.craft_result = ItemStack::EMPTY;
        snapshot.container = [ItemStack::EMPTY; 54];
        snapshot.container_slots = 0;
        target = Screen::Game;
    } else {
        snapshot.window_id = state.guard.id;
        snapshot.window_revision = state.guard.revision;
        snapshot.cursor = stack(state.cursor)?;
        for (index, raw) in state.craft.iter().copied().enumerate() {
            snapshot.craft[index] = stack(raw)?;
        }
        snapshot.craft_result = stack(state.craft_result)?;
        snapshot.container = [ItemStack::EMPTY; 54];
        for (index, raw) in state.container.iter().copied().enumerate() {
            snapshot.container[index] = stack(raw)?;
        }
        snapshot.container_slots = state.container.len() as u8;
        snapshot.machine_progress = state.progress;
        snapshot.furnace_fuel = state.fuel;
        snapshot.energy = state.energy;
        snapshot.sides = state.sides;
    }
    let changed = ui.snapshot != snapshot;
    ui.set_snapshot(snapshot)?;
    let window_screen = matches!(
        ui.screen,
        Screen::Inventory
            | Screen::Workbench
            | Screen::Container
            | Screen::Furnace
            | Screen::Machine
            | Screen::Recipe
    );
    if (!state.closed && old_id != state.guard.id) || (state.closed && window_screen) {
        ui.open(target);
    }
    Ok(changed)
}
