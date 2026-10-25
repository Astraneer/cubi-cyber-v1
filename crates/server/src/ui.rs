//! Detached authoritative window plans. The server owner must validate reach,
//! permissions and the bound block component before planning, then durably commit
//! inventory + window + changed container together before publishing any reply.
use rules::{
    blocks::MekKind,
    crafting::{CraftPlan, Grid, RecipeBook, RecipeId, RecipeKind, RemainderPolicy},
    stack::ItemStack,
    survival::Inventory,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Inventory,
    Workbench,
    Chest(u8),
    Furnace,
    Machine(MekKind),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGuard {
    pub id: u64,
    pub revision: u64,
    pub inventory_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Click {
    pub slot: u16,
    pub right: bool,
    pub shift: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    Stale,
    Closed,
    Slot,
    Role,
    Capacity,
    Ingredients,
    Recipe,
    Encoding,
    Overflow,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowState {
    id: u64,
    revision: u64,
    kind: WindowKind,
    cursor: ItemStack,
    craft: Grid,
    container: [ItemStack; 54],
    closed: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowWork {
    pub slots_scanned: u64,
    pub crafts: u16,
}
#[must_use = "Durably commit inventory, window, container and explicit drops together before applying"]
pub struct WindowPlan {
    before: WindowState,
    after: WindowState,
    before_inventory: Inventory,
    after_inventory: Inventory,
    drops: [ItemStack; 10],
    drop_count: usize,
    pub work: WindowWork,
}
impl WindowPlan {
    pub fn window_after(&self) -> &WindowState {
        &self.after
    }
    pub fn inventory_after(&self) -> &Inventory {
        &self.after_inventory
    }
    pub fn drops(&self) -> &[ItemStack] {
        &self.drops[..self.drop_count]
    }
    pub fn container_changed(&self) -> bool {
        self.before.container != self.after.container
    }
    /// Consume only after the owner's durable transaction completed. All inputs
    /// are compared first, preventing a partial application or reuse after ABA.
    pub fn apply(
        self,
        window: &mut WindowState,
        inventory: &mut Inventory,
    ) -> Result<(), WindowError> {
        if *window != self.before || *inventory != self.before_inventory {
            return Err(WindowError::Stale);
        }
        *window = self.after;
        *inventory = self.after_inventory;
        Ok(())
    }
}
impl WindowState {
    pub fn inventory() -> Self {
        Self {
            id: 0,
            revision: 0,
            kind: WindowKind::Inventory,
            cursor: ItemStack::EMPTY,
            craft: Grid::new(2, 2).unwrap(),
            container: [ItemStack::EMPTY; 54],
            closed: false,
        }
    }
    pub fn open(id: u64, kind: WindowKind, slots: &[ItemStack]) -> Result<Self, WindowError> {
        if matches!(kind,WindowKind::Chest(n) if n!=27&&n!=54)
            || id == 0
            || slots.len() != container_len(kind)
            || slots.len() > 54
        {
            return Err(WindowError::Slot);
        }
        let mut state = Self {
            id,
            revision: 0,
            kind,
            cursor: ItemStack::EMPTY,
            craft: Grid::new(
                if kind == WindowKind::Workbench { 3 } else { 2 },
                if kind == WindowKind::Workbench { 3 } else { 2 },
            )
            .unwrap(),
            container: [ItemStack::EMPTY; 54],
            closed: false,
        };
        state.container[..slots.len()].copy_from_slice(slots);
        Ok(state)
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn kind(&self) -> WindowKind {
        self.kind
    }
    pub fn cursor(&self) -> ItemStack {
        self.cursor
    }
    pub fn craft(&self) -> &Grid {
        &self.craft
    }
    pub fn container(&self) -> &[ItemStack] {
        &self.container[..container_len(self.kind)]
    }
    pub fn closed(&self) -> bool {
        self.closed
    }
    pub fn guard(&self, inventory: &Inventory) -> WindowGuard {
        WindowGuard {
            id: self.id,
            revision: self.revision,
            inventory_revision: inventory.revision(),
        }
    }
    pub fn result(&self, book: &RecipeBook) -> ItemStack {
        book.result(&self.craft)
    }
    fn plan(&self, inventory: &Inventory, guard: WindowGuard) -> Result<WindowPlan, WindowError> {
        if guard != self.guard(inventory) {
            return Err(WindowError::Stale);
        }
        if self.closed {
            return Err(WindowError::Closed);
        }
        let mut after = self.clone();
        after.revision = after.revision.checked_add(1).ok_or(WindowError::Overflow)?;
        Ok(WindowPlan {
            before: self.clone(),
            after,
            before_inventory: *inventory,
            after_inventory: *inventory,
            drops: [ItemStack::EMPTY; 10],
            drop_count: 0,
            work: WindowWork::default(),
        })
    }
    pub fn plan_click(
        &self,
        inventory: &Inventory,
        guard: WindowGuard,
        click: Click,
    ) -> Result<WindowPlan, WindowError> {
        let mut plan = self.plan(inventory, guard)?;
        let slot = slot_ref(&plan.after, click.slot)?;
        let current = read_slot(&plan.after, &plan.after_inventory, slot);
        plan.work.slots_scanned = 1;
        if click.shift {
            let mut remaining = current;
            if current.is_empty() {
                return Ok(plan);
            }
            match slot {
                Slot::Inventory(_) => {
                    if container_len(self.kind) == 0 {
                        return Err(WindowError::Slot);
                    }
                    insert_container(&mut plan.after, &mut remaining, &mut plan.work);
                }
                _ => {
                    let pickup = plan
                        .after_inventory
                        .plan_pickup(current)
                        .map_err(|_| WindowError::Capacity)?;
                    remaining = pickup.remainder();
                    pickup
                        .apply(&mut plan.after_inventory)
                        .map_err(|_| WindowError::Stale)?;
                    plan.work.slots_scanned += 72;
                }
            }
            write_slot(&mut plan.after, &mut plan.after_inventory, slot, remaining)?;
            return Ok(plan);
        }
        let held = plan.after.cursor;
        let (next, held_after) = if held.is_empty() {
            let take = if click.right {
                current.count().div_ceil(2)
            } else {
                current.count()
            };
            (
                count(current, current.count() - take)?,
                count(current, take)?,
            )
        } else if current.is_empty() {
            let placed = if click.right { 1 } else { held.count() };
            (count(held, placed)?, count(held, held.count() - placed)?)
        } else if current.same_kind(held) {
            let placed = (if click.right { 1 } else { held.count() })
                .min(current.maximum() - current.count());
            (
                count(current, current.count() + placed)?,
                count(held, held.count() - placed)?,
            )
        } else {
            (held, current)
        };
        let inserting =
            !held.is_empty() && (!next.same_kind(current) || next.count() > current.count());
        if inserting && !next.is_empty() && !accepts(&plan.after, slot, next.item()) {
            return Err(WindowError::Role);
        }
        write_slot(&mut plan.after, &mut plan.after_inventory, slot, next)?;
        plan.after.cursor = held_after;
        Ok(plan)
    }
    /// Auto-fill an existing registered recipe from owned main inventory slots.
    /// Existing craft ingredients return first; failure leaves both values intact.
    pub fn plan_fill(
        &self,
        inventory: &Inventory,
        guard: WindowGuard,
        book: &RecipeBook,
        recipe: RecipeId,
        all: bool,
    ) -> Result<WindowPlan, WindowError> {
        if !matches!(self.kind, WindowKind::Inventory | WindowKind::Workbench) {
            return Err(WindowError::Role);
        }
        let mut plan = self.plan(inventory, guard)?;
        let recipe = book.get(recipe).ok_or(WindowError::Recipe)?;
        let mut ingredients = [None; 9];
        match *recipe.kind() {
            RecipeKind::Shaped(pattern) => {
                if pattern.width() > self.craft.width() || pattern.height() > self.craft.height() {
                    return Err(WindowError::Recipe);
                }
                for y in 0..pattern.height() {
                    for x in 0..pattern.width() {
                        ingredients[y * self.craft.width() + x] =
                            pattern.cells()[y * pattern.width() + x];
                    }
                }
            }
            RecipeKind::Shapeless {
                ingredients: source,
                count,
            } => {
                if usize::from(count) > self.craft.cells().len() {
                    return Err(WindowError::Recipe);
                }
                ingredients[..usize::from(count)].copy_from_slice(&source[..usize::from(count)]);
            }
            RecipeKind::Repair => return Err(WindowError::Recipe),
        }
        for y in 0..self.craft.height() {
            for x in 0..self.craft.width() {
                let value = plan.after.craft.get(x, y).unwrap();
                if !value.is_empty() {
                    let pickup = plan
                        .after_inventory
                        .plan_pickup(value)
                        .map_err(|_| WindowError::Capacity)?;
                    if !pickup.remainder().is_empty() {
                        return Err(WindowError::Capacity);
                    }
                    pickup
                        .apply(&mut plan.after_inventory)
                        .map_err(|_| WindowError::Stale)?;
                    plan.work.slots_scanned += 72;
                    plan.after
                        .craft
                        .set(x, y, ItemStack::EMPTY)
                        .map_err(|_| WindowError::Slot)?;
                }
            }
        }
        let maximum = if all { 64 } else { 1 };
        for _ in 0..maximum {
            let mut candidate_inventory = plan.after_inventory;
            let mut candidate_grid = plan.after.craft;
            let mut complete = true;
            for (target, ingredient) in ingredients
                .iter()
                .enumerate()
                .take(self.craft.cells().len())
            {
                let Some(ingredient) = ingredient else {
                    continue;
                };
                let x = target % self.craft.width();
                let y = target / self.craft.width();
                let current = candidate_grid.get(x, y).unwrap();
                if !current.is_empty() && current.count() == current.maximum() {
                    complete = false;
                    break;
                }
                let mut source = None;
                for (index, value) in candidate_inventory.main().iter().copied().enumerate() {
                    plan.work.slots_scanned += 1;
                    if ingredient.accepts(value) && (current.is_empty() || current.same_kind(value))
                    {
                        source = Some((index, value));
                        break;
                    }
                }
                let Some((index, value)) = source else {
                    complete = false;
                    break;
                };
                candidate_inventory
                    .set_trusted(index, count(value, value.count() - 1)?)
                    .map_err(|_| WindowError::Slot)?;
                let next = if current.is_empty() {
                    count(value, 1)?
                } else {
                    count(current, current.count() + 1)?
                };
                candidate_grid
                    .set(x, y, next)
                    .map_err(|_| WindowError::Slot)?;
            }
            if !complete {
                break;
            }
            plan.after_inventory = candidate_inventory;
            plan.after.craft = candidate_grid;
            plan.work.crafts += 1;
        }
        if plan.work.crafts == 0 {
            return Err(WindowError::Ingredients);
        }
        Ok(plan)
    }
    pub fn plan_take(
        &self,
        inventory: &Inventory,
        guard: WindowGuard,
        book: &RecipeBook,
        all: bool,
    ) -> Result<WindowPlan, WindowError> {
        if !matches!(self.kind, WindowKind::Inventory | WindowKind::Workbench) {
            return Err(WindowError::Role);
        }
        let mut plan = self.plan(inventory, guard)?;
        if all {
            for _ in 0..64 {
                let Ok(craft) = plan.after_inventory.plan_craft(book, &plan.after.craft, 1) else {
                    break;
                };
                craft
                    .apply(&mut plan.after_inventory, &mut plan.after.craft)
                    .map_err(|_| WindowError::Stale)?;
                plan.work.crafts += 1;
                plan.work.slots_scanned += 72;
            }
            if plan.work.crafts == 0 {
                return Err(WindowError::Ingredients);
            }
        } else {
            let mut cursor = [plan.after.cursor];
            let craft = CraftPlan::prepare_exact(
                book,
                &plan.after.craft,
                &cursor,
                1,
                RemainderPolicy::CollectDrops,
            )
            .map_err(|_| WindowError::Capacity)?;
            let drops = craft
                .apply(&mut plan.after.craft, &mut cursor)
                .map_err(|_| WindowError::Stale)?;
            plan.after.cursor = cursor[0];
            for stack in drops {
                return_owned(&mut plan, stack)?;
            }
            plan.work.crafts = 1;
            plan.work.slots_scanned += 9;
        }
        Ok(plan)
    }
    pub fn plan_close(
        &self,
        inventory: &Inventory,
        guard: WindowGuard,
    ) -> Result<WindowPlan, WindowError> {
        let mut plan = self.plan(inventory, guard)?;
        let cursor = plan.after.cursor;
        plan.after.cursor = ItemStack::EMPTY;
        return_owned(&mut plan, cursor)?;
        for y in 0..self.craft.height() {
            for x in 0..self.craft.width() {
                let stack = plan.after.craft.get(x, y).unwrap();
                plan.after
                    .craft
                    .set(x, y, ItemStack::EMPTY)
                    .map_err(|_| WindowError::Slot)?;
                return_owned(&mut plan, stack)?;
            }
        }
        plan.after.closed = true;
        Ok(plan)
    }
    /// WND1 contains cursor/crafting/container only. The corresponding guarded
    /// Inventory bytes belong to the same private SavedPlayer/WAL transaction.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = b"WND1".to_vec();
        bytes.extend_from_slice(&self.id.to_le_bytes());
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        let (kind, param) = match self.kind {
            WindowKind::Inventory => (0, 0),
            WindowKind::Workbench => (1, 0),
            WindowKind::Chest(slots) => (2, slots),
            WindowKind::Furnace => (3, 0),
            WindowKind::Machine(kind) => (4, mek_code(kind)),
        };
        bytes.extend_from_slice(&[kind, param, u8::from(self.closed)]);
        for stack in std::iter::once(self.cursor)
            .chain((0..9).map(|index| {
                if index % 3 < self.craft.width() && index / 3 < self.craft.height() {
                    self.craft.get(index % 3, index / 3).unwrap()
                } else {
                    ItemStack::EMPTY
                }
            }))
            .chain(self.container)
        {
            bytes.extend_from_slice(&stack.item().to_le_bytes());
            bytes.extend_from_slice(&stack.count().to_le_bytes());
            bytes.extend_from_slice(&stack.damage().to_le_bytes());
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WindowError> {
        if bytes.len() != 407 || bytes.get(..4) != Some(b"WND1") {
            return Err(WindowError::Encoding);
        }
        let id = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        let revision = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let kind = match (bytes[20], bytes[21]) {
            (0, 0) => WindowKind::Inventory,
            (1, 0) if id != 0 => WindowKind::Workbench,
            (2, slots @ 27) | (2, slots @ 54) if id != 0 => WindowKind::Chest(slots),
            (3, 0) if id != 0 => WindowKind::Furnace,
            (4, code) if id != 0 => {
                WindowKind::Machine(mek_decode(code).ok_or(WindowError::Encoding)?)
            }
            _ => return Err(WindowError::Encoding),
        };
        if bytes[22] > 1 {
            return Err(WindowError::Encoding);
        }
        let mut state = if kind == WindowKind::Inventory {
            Self::inventory()
        } else {
            Self::open(id, kind, &vec![ItemStack::EMPTY; container_len(kind)])?
        };
        state.revision = revision;
        state.id = id;
        state.closed = bytes[22] != 0;
        for (index, raw) in bytes[23..].chunks_exact(6).enumerate() {
            let read = |at| u16::from_le_bytes([raw[at], raw[at + 1]]);
            let stack = ItemStack::from_parts(read(0), read(2), read(4))
                .map_err(|_| WindowError::Encoding)?;
            match index {
                0 => state.cursor = stack,
                1..=9 => {
                    let at = index - 1;
                    if at % 3 < state.craft.width() && at / 3 < state.craft.height() {
                        state
                            .craft
                            .set(at % 3, at / 3, stack)
                            .map_err(|_| WindowError::Encoding)?;
                    } else if !stack.is_empty() {
                        return Err(WindowError::Encoding);
                    }
                }
                _ => {
                    let slot = index - 10;
                    if slot >= container_len(kind) && !stack.is_empty() {
                        return Err(WindowError::Encoding);
                    }
                    state.container[slot] = stack;
                }
            }
        }
        if !matches!(state.kind, WindowKind::Inventory | WindowKind::Workbench)
            && state.craft.cells().iter().any(|value| !value.is_empty())
        {
            return Err(WindowError::Encoding);
        }
        if state.closed
            && (!state.cursor.is_empty() || state.craft.cells().iter().any(|v| !v.is_empty()))
        {
            return Err(WindowError::Encoding);
        }
        Ok(state)
    }
}
#[derive(Clone, Copy)]
enum Slot {
    Inventory(usize),
    Craft(usize, usize),
    Container(usize),
}
fn slot_ref(window: &WindowState, index: u16) -> Result<Slot, WindowError> {
    match index {
        0..=40 => Ok(Slot::Inventory(index as usize)),
        41..=94 if usize::from(index - 41) < container_len(window.kind) => {
            Ok(Slot::Container(usize::from(index - 41)))
        }
        100..=108 if matches!(window.kind, WindowKind::Inventory | WindowKind::Workbench) => {
            let x = usize::from(index - 100) % 3;
            let y = usize::from(index - 100) / 3;
            if x < window.craft.width() && y < window.craft.height() {
                Ok(Slot::Craft(x, y))
            } else {
                Err(WindowError::Slot)
            }
        }
        _ => Err(WindowError::Slot),
    }
}
fn read_slot(window: &WindowState, inventory: &Inventory, slot: Slot) -> ItemStack {
    match slot {
        Slot::Inventory(i) => inventory.get(i).unwrap(),
        Slot::Craft(x, y) => window.craft.get(x, y).unwrap(),
        Slot::Container(i) => window.container[i],
    }
}
fn write_slot(
    window: &mut WindowState,
    inventory: &mut Inventory,
    slot: Slot,
    value: ItemStack,
) -> Result<(), WindowError> {
    match slot {
        Slot::Inventory(i) => inventory
            .set_trusted(i, value)
            .map_err(|_| WindowError::Role),
        Slot::Craft(x, y) => window.craft.set(x, y, value).map_err(|_| WindowError::Slot),
        Slot::Container(i) => {
            window.container[i] = value;
            Ok(())
        }
    }
}
fn count(stack: ItemStack, amount: u16) -> Result<ItemStack, WindowError> {
    stack.with_count(amount).map_err(|_| WindowError::Capacity)
}
fn accepts(window: &WindowState, slot: Slot, item: rules::ItemId) -> bool {
    match slot {
        Slot::Inventory(i) if (36..40).contains(&i) => {
            rules::items::equip_slot(item) == Some((i - 36) as u8)
        }
        Slot::Container(index) => match window.kind {
            WindowKind::Furnace => match index {
                0 => rules::survival::furnace::smelt(item).is_some(),
                1 => rules::survival::furnace::fuel_ticks(item) > 0,
                _ => false,
            },
            WindowKind::Machine(kind) => rules::machines::roles(kind)
                .get(index)
                .is_some_and(|role| rules::machines::role_accepts(kind, *role, item)),
            _ => true,
        },
        _ => true,
    }
}
fn insert_container(window: &mut WindowState, remaining: &mut ItemStack, work: &mut WindowWork) {
    for empty in [false, true] {
        for index in 0..container_len(window.kind) {
            work.slots_scanned += 1;
            if remaining.is_empty() {
                return;
            }
            let slot = window.container[index];
            if !accepts(window, Slot::Container(index), remaining.item())
                || slot.is_empty() != empty
                || (!slot.is_empty() && !slot.same_kind(*remaining))
            {
                continue;
            }
            let amount = remaining.count().min(if slot.is_empty() {
                remaining.maximum()
            } else {
                slot.maximum() - slot.count()
            });
            window.container[index] = count(
                *remaining,
                if slot.is_empty() {
                    amount
                } else {
                    slot.count() + amount
                },
            )
            .unwrap();
            *remaining = count(*remaining, remaining.count() - amount).unwrap();
        }
    }
}
fn return_owned(plan: &mut WindowPlan, stack: ItemStack) -> Result<(), WindowError> {
    if stack.is_empty() {
        return Ok(());
    }
    let pickup = plan
        .after_inventory
        .plan_pickup(stack)
        .map_err(|_| WindowError::Capacity)?;
    let remaining = pickup.remainder();
    pickup
        .apply(&mut plan.after_inventory)
        .map_err(|_| WindowError::Stale)?;
    plan.work.slots_scanned += 72;
    if !remaining.is_empty() {
        if plan.drop_count == plan.drops.len() {
            return Err(WindowError::Capacity);
        }
        plan.drops[plan.drop_count] = remaining;
        plan.drop_count += 1;
    }
    Ok(())
}
fn container_len(kind: WindowKind) -> usize {
    match kind {
        WindowKind::Inventory | WindowKind::Workbench => 0,
        WindowKind::Chest(n) => usize::from(n),
        WindowKind::Furnace => 3,
        WindowKind::Machine(kind) => rules::machines::roles(kind).len(),
    }
}
fn mek_code(kind: MekKind) -> u8 {
    match kind {
        MekKind::Heat => 0,
        MekKind::Infuser => 1,
        MekKind::Cube => 2,
        MekKind::Enrich => 3,
        MekKind::Smelter => 4,
        MekKind::Wind => 5,
        MekKind::Crusher => 6,
        MekKind::Purifier => 7,
        MekKind::Separator => 8,
        MekKind::Pump => 9,
    }
}
fn mek_decode(code: u8) -> Option<MekKind> {
    Some(match code {
        0 => MekKind::Heat,
        1 => MekKind::Infuser,
        2 => MekKind::Cube,
        3 => MekKind::Enrich,
        4 => MekKind::Smelter,
        5 => MekKind::Wind,
        6 => MekKind::Crusher,
        7 => MekKind::Purifier,
        8 => MekKind::Separator,
        9 => MekKind::Pump,
        _ => return None,
    })
}
