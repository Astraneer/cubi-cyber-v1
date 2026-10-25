//! Deterministic, allocation-free Mekanism value simulation.
//!
//! Behavioral constants are data from the SPEC-approved donor's MEK-01…14
//! references. This implementation neither executes nor links donor code. The
//! authoritative owner must publish the complete planned state and world edits
//! atomically after its durable journal commit.

mod codec;
pub mod events;
pub mod grid;
pub mod logistics;
pub mod sides;

use crate::{ItemId, blocks::MekKind, items::*, machine_recipes as recipes, stack::ItemStack};
pub use codec::{MACHINE_BYTES, MACHINE_HEADER_BYTES, MachineDecodeError};
pub use sides::{Face, RelativeSide, SideMode};

pub const OP_TICKS: [u32; 9] = [200, 149, 112, 84, 63, 47, 35, 26, 20];
pub const J50: [u64; 25] = [
    5, 7, 9, 12, 16, 22, 29, 38, 50, 67, 89, 119, 159, 211, 282, 375, 500, 667, 890, 1186, 1582,
    2109, 2812, 3750, 5000,
];
pub const J200: [u64; 25] = [
    20, 27, 36, 48, 64, 85, 113, 150, 200, 267, 356, 475, 633, 844, 1125, 1500, 2000, 2668, 3557,
    4743, 6325, 8434, 11247, 14998, 20000,
];
pub const CAP20: [u64; 9] = [
    20000, 26670, 35565, 47427, 63245, 84339, 112468, 149978, 200000,
];
pub const CAP80: [u64; 9] = [
    80000, 106681, 142262, 189709, 252982, 337357, 449873, 599915, 800000,
];
pub const CAP160: [u64; 9] = [
    160000, 213363, 284524, 379419, 505964, 674714, 899746, 1199830, 1600000,
];
pub const GAS_OPERATION: [u32; 17] = [
    20, 27, 36, 47, 63, 84, 112, 150, 200, 267, 356, 474, 632, 843, 1125, 1500, 2000,
];
pub const MAX_PUMP_NODES: usize = 256;
pub const TABLET_CAPACITY: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tank {
    pub substance: u8,
    pub amount: u32,
}
impl Tank {
    pub fn take(&mut self, amount: u32) -> u32 {
        let taken = amount.min(self.amount);
        self.amount -= taken;
        if self.amount == 0 {
            self.substance = recipes::SUB_NONE;
        }
        taken
    }
    pub fn insert(&mut self, substance: u8, amount: u32, capacity: u32, whole: bool) -> u32 {
        if substance == 0 || (self.amount > 0 && self.substance != substance) {
            return 0;
        }
        let free = capacity.saturating_sub(self.amount);
        if whole && amount > free {
            return 0;
        }
        let accepted = amount.min(free);
        if accepted > 0 {
            self.substance = substance;
            self.amount += accepted;
        }
        accepted
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Step {
    #[default]
    Idle,
    Reset,
    NoEnergy,
    NoRoom,
    WrongOutput,
    Worked,
    Finished,
    NoGas,
    NoWater,
    HydrogenFull,
    OxygenFull,
    Budget,
}
impl Step {
    pub fn working(self) -> bool {
        matches!(self, Self::Worked | Self::Finished)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveView {
    pub logic: bool,
    pub shown: bool,
    pub delay: u8,
}
impl ActiveView {
    pub fn countdown(&mut self) {
        if self.delay > 0 {
            self.delay -= 1;
            if self.delay == 0 {
                self.shown = self.logic;
            }
        }
    }
    pub fn set(&mut self, on: bool) {
        if on == self.logic {
            return;
        }
        self.logic = on;
        if self.shown == on {
            return;
        }
        if on {
            self.shown = true;
        } else {
            let immediate = self.delay == 0;
            self.delay = 60;
            if immediate {
                self.shown = false;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GasMode {
    #[default]
    Store,
    Excess,
    Dump,
}

/// Slots use typed charge rather than overloading tool durability. A slot's
/// charge is zero unless its item is an energy tablet; validated on decode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub stack: ItemStack,
    pub charge: u32,
}
impl Slot {
    pub fn new(stack: ItemStack) -> Self {
        Self { stack, charge: 0 }
    }
    pub fn tablet(joules: u32) -> Self {
        Self {
            stack: ItemStack::new(I_ENERGY_TABLET, 1).expect("defined tablet"),
            charge: joules.min(TABLET_CAPACITY) / 20 * 20,
        }
    }
    fn take(&mut self, count: u16) {
        self.stack = self
            .stack
            .with_count(self.stack.count().saturating_sub(count))
            .expect("existing valid count");
        if self.stack.is_empty() {
            self.charge = 0;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Definition {
    pub capacity: u64,
    pub joules_per_tick: u64,
    pub ticks: u32,
    pub tank_capacity: u32,
    pub energy_slot: Option<usize>,
    pub upgrade_slot: Option<usize>,
    pub input: Option<usize>,
    pub output: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotRole {
    Input,
    Output,
    Energy,
    Extra,
    Fuel,
    Charge,
    Discharge,
    Upgrade,
    Fluid,
}

pub fn roles(kind: MekKind) -> &'static [SlotRole] {
    use SlotRole::*;
    match kind {
        MekKind::Heat => &[Fuel, Charge],
        MekKind::Infuser => &[Extra, Input, Output, Energy, Upgrade],
        MekKind::Cube => &[Discharge, Charge],
        MekKind::Wind => &[Charge],
        MekKind::Purifier => &[Input, Extra, Output, Energy, Upgrade],
        MekKind::Separator => &[Fluid, Upgrade, Energy],
        MekKind::Pump => &[Energy],
        _ => &[Input, Output, Energy, Upgrade],
    }
}

pub fn role_accepts(kind: MekKind, role: SlotRole, item: ItemId) -> bool {
    use SlotRole::*;
    match role {
        Output => false,
        Energy | Discharge => item == I_ENERGY_TABLET || recipes::energy_of(item) > 0,
        Charge => item == I_ENERGY_TABLET,
        Fuel => crate::survival::furnace::fuel_ticks(item) > 0,
        Extra if kind == MekKind::Infuser => recipes::chem_of(item).is_some(),
        Extra if kind == MekKind::Purifier => recipes::oxygen_of(item) > 0,
        Extra => false,
        Upgrade => {
            [I_UPGRADE_SPEED, I_UPGRADE_ENERGY].contains(&item)
                || (item == I_UPGRADE_GAS && kind == MekKind::Purifier)
        }
        Fluid => item == I_WATER_BUCKET,
        Input => match kind {
            MekKind::Infuser => recipes::infusable(item),
            MekKind::Enrich => recipes::find_enrich(item).is_some(),
            MekKind::Crusher => recipes::find_crush(item).is_some(),
            MekKind::Purifier => recipes::find_purify(item).is_some(),
            MekKind::Smelter => crate::survival::furnace::smelt(item).is_some(),
            _ => false,
        },
    }
}
pub fn definition(kind: MekKind) -> Definition {
    let mut d = Definition {
        capacity: 20_000,
        joules_per_tick: 50,
        ticks: 200,
        tank_capacity: 0,
        energy_slot: Some(2),
        upgrade_slot: Some(3),
        input: Some(0),
        output: Some(1),
    };
    match kind {
        MekKind::Infuser => {
            d.tank_capacity = 1000;
            d.energy_slot = Some(3);
            d.upgrade_slot = Some(4);
            d.input = Some(1);
            d.output = Some(2);
        }
        MekKind::Purifier => {
            d.capacity = 80_000;
            d.joules_per_tick = 200;
            d.tank_capacity = 210;
            d.energy_slot = Some(3);
            d.upgrade_slot = Some(4);
            d.output = Some(2);
        }
        MekKind::Separator => {
            d.capacity = 160_000;
            d.joules_per_tick = 0;
            d.ticks = 0;
            d.tank_capacity = 24_000;
            d.energy_slot = Some(2);
            d.upgrade_slot = Some(1);
            d.input = None;
            d.output = None;
        }
        MekKind::Pump => {
            d.capacity = 40_000;
            d.joules_per_tick = 100;
            d.ticks = 19;
            d.tank_capacity = 10_000;
            d.energy_slot = Some(0);
            d.upgrade_slot = None;
            d.input = None;
            d.output = None;
        }
        MekKind::Heat | MekKind::Wind | MekKind::Cube => {
            d.capacity = match kind {
                MekKind::Heat => 160_000,
                MekKind::Wind => 200_000,
                _ => 4_000_000,
            };
            d.joules_per_tick = 0;
            d.ticks = 0;
            d.tank_capacity = if kind == MekKind::Heat { 24_000 } else { 0 };
            d.energy_slot = if kind == MekKind::Cube { Some(0) } else { None };
            d.upgrade_slot = None;
            d.input = None;
            d.output = None;
        }
        _ => {}
    }
    d
}

/// World access is bounded by the caller. Unknown samples must remain Unknown:
/// returning `None` never generates a page or treats it as an available source.
pub trait MachineWorld {
    fn sky_open(&mut self, _top: [i32; 3]) -> bool {
        false
    }
    fn lava_neighbors(&mut self, _position: [i32; 3]) -> u8 {
        0
    }
    fn source(&mut self, _position: [i32; 3]) -> Option<u8> {
        None
    }
    fn drain_lava(&mut self, _position: [i32; 3]) -> bool {
        false
    }
    fn push_energy(&mut self, _position: [i32; 3], _face: Face, _joules: u64) -> u64 {
        0
    }
    fn push_items(&mut self, _position: [i32; 3], _face: Face, _stack: ItemStack) -> u16 {
        0
    }
    fn push_substance(
        &mut self,
        _position: [i32; 3],
        _face: Face,
        _substance: u8,
        _amount: u32,
    ) -> u32 {
        0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub kind: MekKind,
    pub slots: [Slot; 5],
    pub energy: u64,
    pub progress: u32,
    pub recipe: Option<u16>,
    pub tank: Tank,
    pub gas: [Tank; 2],
    pub gas_mode: [GasMode; 2],
    pub gas_used: u32,
    pub upgrades: [u8; 3],
    pub upgrade_delay: u8,
    pub active: ActiveView,
    pub heat: f64,
    pub produced: u64,
    pub ticker: u64,
    pub last: Step,
    pub sides: [SideMode; 6],
    pub gas_sides: [u8; 6],
    pub eject: bool,
    pub eject_delay: u8,
    pub pump_substance: u8,
    pump_nodes: [[i32; 3]; MAX_PUMP_NODES],
    pump_node_count: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operation {
    pub key: u16,
    pub input_count: u16,
    pub output: ItemId,
    pub output_count: u16,
    pub substance_amount: u32,
}

impl Machine {
    pub fn new(kind: MekKind) -> Self {
        Self {
            kind,
            slots: [Slot::default(); 5],
            energy: 0,
            progress: 0,
            recipe: None,
            tank: Tank::default(),
            gas: [Tank::default(); 2],
            gas_mode: [GasMode::Store; 2],
            gas_used: 0,
            upgrades: [0; 3],
            upgrade_delay: 0,
            active: ActiveView::default(),
            heat: 0.0,
            produced: 0,
            ticker: 0,
            last: Step::Idle,
            sides: sides::defaults(kind),
            gas_sides: if kind == MekKind::Separator {
                [0, 1, 2, 0, 0, 0]
            } else {
                [0; 6]
            },
            eject: false,
            eject_delay: 0,
            pump_substance: 0,
            pump_nodes: [[0; 3]; MAX_PUMP_NODES],
            pump_node_count: 0,
        }
    }
    pub fn capacity(&self) -> u64 {
        let e = self.upgrades[1].min(8) as usize;
        let base = definition(self.kind).capacity;
        if definition(self.kind).upgrade_slot.is_none() {
            return base;
        }
        match base {
            20_000 => CAP20[e],
            80_000 => CAP80[e],
            160_000 => CAP160[e],
            _ => base * CAP20[e] / 20_000,
        }
    }
    pub fn ticks(&self) -> u32 {
        let base = definition(self.kind).ticks;
        if base == 0 {
            0
        } else {
            (base * OP_TICKS[self.upgrades[0].min(8) as usize] / 200).max(1)
        }
    }
    pub fn per_tick(&self) -> u64 {
        let k = 2 * usize::from(self.upgrades[0].min(8)) + 8 - usize::from(self.upgrades[1].min(8));
        match definition(self.kind).joules_per_tick {
            0 => 0,
            50 => J50[k],
            200 => J200[k],
            n => (n * J50[k]).div_ceil(50),
        }
    }
    pub fn install(&mut self, upgrade: usize, count: u8) -> u8 {
        if upgrade > 2
            || definition(self.kind).upgrade_slot.is_none()
            || (upgrade == 2 && self.kind != MekKind::Purifier)
        {
            return 0;
        }
        let n = count.min(8 - self.upgrades[upgrade].min(8));
        self.upgrades[upgrade] += n;
        self.energy = self.energy.min(self.capacity());
        n
    }
    pub fn insert_energy(&mut self, joules: u64) -> u64 {
        let n = joules.min(self.capacity().saturating_sub(self.energy));
        self.energy += n;
        n
    }
    /// Sided item insertion, intended for a cloned owner-side transaction plan.
    /// All externally supplied output slots are read-only.
    pub fn insert_item(&mut self, facing: Face, face: Face, item: Slot, whole: bool) -> u16 {
        if item.stack.is_empty()
            || item.charge > TABLET_CAPACITY
            || !item.charge.is_multiple_of(20)
            || (item.charge > 0 && item.stack.item() != I_ENERGY_TABLET)
        {
            return 0;
        }
        let mode = self.sides[sides::relative(facing, face) as usize];
        let role_allowed = |role| {
            matches!(
                (self.kind, mode, role),
                (
                    MekKind::Cube,
                    SideMode::Input | SideMode::Both,
                    SlotRole::Charge
                ) | (
                    MekKind::Cube,
                    SideMode::Output | SideMode::Both,
                    SlotRole::Discharge
                ) | (
                    _,
                    SideMode::Input | SideMode::Both,
                    SlotRole::Input | SlotRole::Fluid
                ) | (_, SideMode::Energy, SlotRole::Energy)
                    | (_, SideMode::Extra, SlotRole::Extra)
            )
        };
        let mut room = 0;
        for (i, &role) in roles(self.kind).iter().enumerate() {
            if !role_allowed(role) || !role_accepts(self.kind, role, item.stack.item()) {
                continue;
            }
            let current = self.slots[i];
            if !current.stack.is_empty()
                && (!current.stack.same_kind(item.stack) || current.charge != item.charge)
            {
                continue;
            }
            room += item.stack.maximum().saturating_sub(current.stack.count());
        }
        if whole && room < item.stack.count() {
            return 0;
        }
        let mut left = item.stack.count();
        for (i, &role) in roles(self.kind).iter().enumerate() {
            if !role_allowed(role) || !role_accepts(self.kind, role, item.stack.item()) {
                continue;
            }
            let current = self.slots[i];
            if !current.stack.is_empty()
                && (!current.stack.same_kind(item.stack) || current.charge != item.charge)
            {
                continue;
            }
            let n = left.min(item.stack.maximum().saturating_sub(current.stack.count()));
            if n > 0 {
                self.slots[i] = Slot {
                    stack: item
                        .stack
                        .with_count(current.stack.count() + n)
                        .expect("bounded stack"),
                    charge: item.charge,
                };
                left -= n;
            }
        }
        item.stack.count() - left
    }
    pub fn insert_substance(&mut self, substance: u8, amount: u32, whole: bool) -> u32 {
        let accepts = matches!(
            (self.kind, substance),
            (MekKind::Purifier, recipes::SUB_OXYGEN)
                | (MekKind::Separator, recipes::SUB_WATER)
                | (MekKind::Heat, recipes::SUB_LAVA)
        );
        if !accepts {
            return 0;
        }
        self.tank.insert(
            substance,
            amount,
            definition(self.kind).tank_capacity,
            whole,
        )
    }
    pub fn operation(&self) -> Option<Operation> {
        let slot = definition(self.kind).input?;
        let input = self.slots[slot].stack;
        let id = input.item();
        let op = match self.kind {
            MekKind::Infuser => {
                let i = recipes::find_infuse(id, self.tank.substance, self.tank.amount)?;
                let r = recipes::INFUSE[i as usize];
                Operation {
                    key: u16::from(i),
                    input_count: 1,
                    output: r.out,
                    output_count: 1,
                    substance_amount: r.amount,
                }
            }
            MekKind::Enrich | MekKind::Crusher => {
                let (i, r) = if self.kind == MekKind::Enrich {
                    let i = recipes::find_enrich(id)?;
                    (i, recipes::ENRICH[i as usize])
                } else {
                    let i = recipes::find_crush(id)?;
                    (i, recipes::CRUSH[i as usize])
                };
                Operation {
                    key: u16::from(i),
                    input_count: r.in_n as u16,
                    output: r.out,
                    output_count: r.out_n as u16,
                    substance_amount: 0,
                }
            }
            MekKind::Purifier => {
                let i = recipes::find_purify(id)?;
                let r = recipes::PURIFY[i as usize];
                Operation {
                    key: u16::from(i),
                    input_count: 1,
                    output: r.out,
                    output_count: r.out_n as u16,
                    substance_amount: r.rate,
                }
            }
            MekKind::Smelter => Operation {
                key: id,
                input_count: 1,
                output: crate::survival::furnace::smelt(id)?,
                output_count: 1,
                substance_amount: 0,
            },
            _ => return None,
        };
        (input.count() >= op.input_count).then_some(op)
    }
    fn prepare_operation(&mut self) -> Result<Operation, Step> {
        let op = self.operation();
        if op.map(|o| o.key) != self.recipe {
            if let Some(op) = op {
                if self.recipe.is_some() || self.progress >= self.ticks() {
                    self.progress = 0;
                    self.gas_used = 0;
                }
                self.recipe = Some(op.key);
            } else if self.recipe.is_some() {
                self.progress = 0;
                self.gas_used = 0;
                return Err(Step::Reset);
            } else {
                return Err(Step::Idle);
            }
        }
        let op = op.ok_or(Step::Idle)?;
        if self.kind == MekKind::Purifier {
            let (_, gas) = self.gas_need(op.substance_amount);
            if self.tank.substance != recipes::SUB_OXYGEN
                || self.tank.amount == 0
                || self.tank.amount < gas
            {
                return Err(Step::NoGas);
            }
        }
        if self.energy < self.per_tick() {
            return Err(Step::NoEnergy);
        }
        let out = self.slots[definition(self.kind).output.expect("recipe output")].stack;
        if !out.is_empty() && (out.item() != op.output || out.damage() != 0) {
            return Err(Step::WrongOutput);
        }
        if out.count() + op.output_count > u16::from(crate::items::idef(op.output).max_stack) {
            return Err(Step::NoRoom);
        }
        Ok(op)
    }
    fn gas_need(&self, rate: u32) -> (u32, u32) {
        let k = usize::from(self.upgrades[0].min(8)) + 8 - usize::from(self.upgrades[2].min(8));
        let remaining = GAS_OPERATION[k].saturating_sub(self.gas_used);
        let ticks = self.ticks().saturating_sub(self.progress).max(1);
        let units = remaining / ticks;
        (units, units * rate)
    }
    pub fn recipe_step(&mut self) -> Step {
        let op = match self.prepare_operation() {
            Ok(op) => op,
            Err(step) => return step,
        };
        self.energy -= self.per_tick();
        self.progress += 1;
        if self.kind == MekKind::Purifier {
            let (units, gas) = self.gas_need_at_previous(op.substance_amount);
            self.tank.take(gas);
            self.gas_used += units;
        }
        if self.progress < self.ticks() {
            return Step::Worked;
        }
        self.finish(op);
        Step::Finished
    }
    fn gas_need_at_previous(&self, rate: u32) -> (u32, u32) {
        let mut before = self.progress;
        before = before.saturating_sub(1);
        let k = usize::from(self.upgrades[0].min(8)) + 8 - usize::from(self.upgrades[2].min(8));
        let units = GAS_OPERATION[k].saturating_sub(self.gas_used)
            / self.ticks().saturating_sub(before).max(1);
        (units, units * rate)
    }
    fn finish(&mut self, op: Operation) {
        self.progress = 0;
        self.gas_used = 0;
        let d = definition(self.kind);
        let input = d.input.expect("recipe input");
        let output = d.output.expect("recipe output");
        self.slots[input].take(op.input_count);
        if self.kind == MekKind::Infuser {
            self.tank.take(op.substance_amount);
        }
        let old = self.slots[output].stack;
        self.slots[output] = Slot::new(
            ItemStack::new(op.output, old.count() + op.output_count).expect("capacity checked"),
        );
    }
    fn energy_slot(&mut self, index: usize, rate: u64) {
        let slot = self.slots[index];
        if slot.stack.item() == I_ENERGY_TABLET {
            let n = u64::from(slot.charge)
                .min(5_000)
                .min(rate)
                .min(self.capacity().saturating_sub(self.energy))
                / 20
                * 20;
            self.energy += n;
            self.slots[index].charge -= n as u32;
        } else {
            let n = recipes::energy_of(slot.stack.item());
            if n > 0 && n <= self.capacity().saturating_sub(self.energy) {
                self.energy += n;
                self.slots[index].take(1);
            }
        }
    }
    fn charge_slot(&mut self, index: usize, rate: u64) {
        let slot = &mut self.slots[index];
        if slot.stack.item() == I_ENERGY_TABLET {
            let n = self
                .energy
                .min(5_000)
                .min(rate)
                .min(u64::from(TABLET_CAPACITY.saturating_sub(slot.charge)))
                / 20
                * 20;
            self.energy -= n;
            slot.charge += n as u32;
        }
    }
    fn upgrade_tick(&mut self) {
        let Some(i) = definition(self.kind).upgrade_slot else {
            return;
        };
        let u = match self.slots[i].stack.item() {
            I_UPGRADE_SPEED => 0,
            I_UPGRADE_ENERGY => 1,
            I_UPGRADE_GAS if self.kind == MekKind::Purifier => 2,
            _ => {
                self.upgrade_delay = 0;
                return;
            }
        };
        if self.upgrades[u] >= 8 {
            self.upgrade_delay = 0;
            return;
        }
        if self.upgrade_delay < 20 {
            self.upgrade_delay += 1;
            return;
        }
        let n = self.install(u, self.slots[i].stack.count().min(8) as u8);
        self.slots[i].take(u16::from(n));
        self.upgrade_delay = 0;
    }
    fn eject_items(&mut self, p: [i32; 3], facing: Face, w: &mut impl MachineWorld) {
        if !self.eject {
            return;
        }
        let Some(output) = definition(self.kind).output else {
            return;
        };
        if self.eject_delay > 0 {
            self.eject_delay -= 1;
            return;
        }
        for mode in [SideMode::Output, SideMode::Both] {
            for face in Face::ALL {
                if self.sides[sides::relative(facing, face) as usize] != mode {
                    continue;
                }
                let s = self.slots[output].stack;
                if s.is_empty() {
                    break;
                }
                let n = w.push_items(p, face, s).min(s.count());
                self.slots[output].take(n);
            }
        }
        self.eject_delay = 10;
    }
    /// One owner-side 20 Hz phase. No allocation and no renderer/network access.
    pub fn tick(&mut self, p: [i32; 3], facing: Face, w: &mut impl MachineWorld) -> Step {
        self.active.countdown();
        self.upgrade_tick();
        let step = match self.kind {
            MekKind::Heat => self.heat_tick(p, facing, w),
            MekKind::Wind => self.wind_tick(p, facing, w),
            MekKind::Cube => {
                let n = w
                    .push_energy(p, facing, self.energy.min(4000))
                    .min(self.energy.min(4000));
                self.energy -= n;
                self.charge_slot(1, 4000);
                self.energy_slot(0, 4000);
                Step::Idle
            }
            MekKind::Separator => self.separator_tick(p, facing, w),
            MekKind::Pump => self.pump_tick(p, w),
            _ => {
                self.eject_items(p, facing, w);
                if let Some(i) = definition(self.kind).energy_slot {
                    self.energy_slot(i, u64::MAX);
                }
                if self.kind == MekKind::Infuser
                    && let Some((sub, n)) = recipes::chem_of(self.slots[0].stack.item())
                    && self.tank.insert(sub, n, 1000, true) == n
                {
                    self.slots[0].take(1);
                }
                if self.kind == MekKind::Purifier {
                    let n = recipes::oxygen_of(self.slots[1].stack.item());
                    if n > 0 && self.insert_substance(recipes::SUB_OXYGEN, n, true) == n {
                        self.slots[1].take(1);
                    }
                }
                self.recipe_step()
            }
        };
        self.last = step;
        self.active.set(step.working());
        step
    }
    fn heat_tick(&mut self, p: [i32; 3], facing: Face, w: &mut impl MachineWorld) -> Step {
        let n = w
            .push_energy(p, facing, self.energy.min(1020))
            .min(self.energy.min(1020));
        self.energy -= n;
        self.charge_slot(1, u64::MAX);
        let input = self.slots[0].stack;
        let mb = if input.item() == I_LAVA_BUCKET {
            1000
        } else {
            crate::survival::furnace::fuel_ticks(input.item()) / 20
        };
        if mb > 0 && mb <= 24_000 - self.tank.amount {
            self.tank.insert(recipes::SUB_LAVA, mb, 24_000, true);
            if input.item() == I_LAVA_BUCKET {
                self.slots[0] = Slot::new(ItemStack::new(I_BUCKET, 1).expect("bucket"));
            } else {
                self.slots[0].take(1);
            }
        }
        if self.heat == 0.0 {
            self.heat = 3000.0;
        }
        let t = self.heat / 10.0;
        let mut change = 30.0 * f64::from(w.lava_neighbors(p).min(6));
        let burning = self.energy < self.capacity() && self.tank.amount >= 10;
        if burning {
            self.tank.take(10);
            change += 200.0;
        }
        let lost = 0.5 * (t - 300.0);
        change -= lost;
        let efficiency = 1.0 - 300.0_f64.min(t) / 300.0_f64.max(t);
        let generated = ((lost.abs() * efficiency) as u64).min(510);
        self.produced = self.insert_energy(generated);
        change -= 10.0 * (5.0 * (t - 300.0) / 10105.0 + (t - 300.0) / 10005.0);
        self.heat += change;
        if burning { Step::Worked } else { Step::Idle }
    }
    fn wind_tick(&mut self, p: [i32; 3], facing: Face, w: &mut impl MachineWorld) -> Step {
        let mut left = self.energy.min(960);
        for face in [facing, Face::Down] {
            let n = w.push_energy(p, face, left).min(left);
            left -= n;
            self.energy -= n;
        }
        self.charge_slot(0, u64::MAX);
        if self.ticker.is_multiple_of(20) {
            let top = [p[0], p[1].saturating_add(4), p[2]];
            self.produced = if w.sky_open(top) { wind_power(p[1]) } else { 0 };
        }
        self.ticker = self.ticker.wrapping_add(1);
        self.insert_energy(self.produced);
        if self.produced > 0 {
            Step::Worked
        } else {
            Step::Idle
        }
    }
    fn separator_tick(&mut self, p: [i32; 3], facing: Face, w: &mut impl MachineWorld) -> Step {
        for i in 0..2 {
            if self.gas_mode[i] == GasMode::Dump {
                continue;
            }
            let mut left = 1024;
            for r in RelativeSide::ALL {
                if self.gas_sides[r as usize] != i as u8 + 1 {
                    continue;
                }
                let t = self.gas[i];
                let n = w
                    .push_substance(
                        p,
                        sides::direction(facing, r),
                        t.substance,
                        t.amount.min(left),
                    )
                    .min(t.amount.min(left));
                self.gas[i].take(n);
                left -= n;
            }
        }
        self.energy_slot(2, u64::MAX);
        if self.slots[0].stack.item() == I_WATER_BUCKET
            && self.insert_substance(recipes::SUB_WATER, 1000, true) == 1000
        {
            self.slots[0] = Slot::new(ItemStack::new(I_BUCKET, 1).expect("bucket"));
        }
        let [h, o] = self.gas.map(|t| t.amount);
        let both = self.gas_mode == [GasMode::Excess; 2] && h >= 2040 && o >= 2040;
        let ops = if both {
            0
        } else {
            (1u64 << self.upgrades[0].min(8))
                .min(u64::from(self.tank.amount / 2))
                .min(u64::from((2400 - h) / 2))
                .min(u64::from(2400 - o))
                .min(self.energy / 400)
        };
        let step = if ops > 0 {
            self.energy -= ops * 400;
            self.tank.take(ops as u32 * 2);
            self.gas[0].insert(recipes::SUB_HYDROGEN, ops as u32 * 2, 2400, false);
            self.gas[1].insert(recipes::SUB_OXYGEN, ops as u32, 2400, false);
            Step::Worked
        } else if both {
            Step::Idle
        } else if self.tank.amount < 2 {
            Step::NoWater
        } else if h > 2398 {
            Step::HydrogenFull
        } else if o == 2400 {
            Step::OxygenFull
        } else {
            Step::NoEnergy
        };
        self.produced = ops * 400;
        for i in 0..2 {
            let amount = self.gas[i].amount;
            let n = match self.gas_mode[i] {
                GasMode::Dump => (8u32 << self.upgrades[0].min(8)).min(amount),
                GasMode::Excess => amount.saturating_sub(2040).min(1024),
                GasMode::Store => 0,
            };
            self.gas[i].take(n);
        }
        step
    }
    fn take_source(&mut self, q: [i32; 3], w: &mut impl MachineWorld) -> bool {
        let Some(sub) = w.source(q) else { return false };
        if ![recipes::SUB_WATER, recipes::SUB_LAVA].contains(&sub)
            || (self.pump_substance != 0 && self.pump_substance != sub)
            || (self.tank.amount > 0 && self.tank.substance != sub)
            || self.tank.amount > 9000
        {
            return false;
        }
        if sub == recipes::SUB_LAVA && !w.drain_lava(q) {
            return false;
        }
        self.tank.insert(sub, 1000, 10_000, true);
        self.pump_substance = sub;
        true
    }
    fn suck(&mut self, p: [i32; 3], w: &mut impl MachineWorld) -> Result<bool, Step> {
        let below = [p[0], p[1].saturating_sub(1), p[2]];
        let known = self.pump_nodes[..usize::from(self.pump_node_count)].contains(&below);
        if !known && usize::from(self.pump_node_count) == MAX_PUMP_NODES {
            return Err(Step::Budget);
        }
        if self.take_source(below, w) {
            if !known {
                self.pump_nodes[usize::from(self.pump_node_count)] = below;
                self.pump_node_count += 1;
            }
            return Ok(true);
        }
        self.pump_nodes[..usize::from(self.pump_node_count)].sort_unstable();
        let mut reads = 0;
        while self.pump_node_count > 0 && reads < 64 {
            let node = self.pump_nodes[0];
            reads += 1;
            if self.take_source(node, w) {
                return Ok(true);
            }
            for face in Face::ALL {
                let Some(q) = sides::offset(node, face) else {
                    continue;
                };
                let dist = (0..3)
                    .map(|k| i128::from(q[k]) - i128::from(p[k]))
                    .map(|d| d * d)
                    .sum::<i128>();
                if dist > 6400 || self.pump_nodes[..usize::from(self.pump_node_count)].contains(&q)
                {
                    continue;
                }
                if usize::from(self.pump_node_count) == MAX_PUMP_NODES {
                    return Err(Step::Budget);
                }
                if self.take_source(q, w) {
                    self.pump_nodes[usize::from(self.pump_node_count)] = q;
                    self.pump_node_count += 1;
                    return Ok(true);
                }
            }
            self.pump_nodes
                .copy_within(1..usize::from(self.pump_node_count), 0);
            self.pump_node_count -= 1;
            self.pump_nodes[usize::from(self.pump_node_count)] = [0; 3];
        }
        if self.pump_node_count > 0 {
            Err(Step::Budget)
        } else {
            Ok(false)
        }
    }
    fn pump_tick(&mut self, p: [i32; 3], w: &mut impl MachineWorld) -> Step {
        self.energy_slot(0, u64::MAX);
        let cost = self.per_tick();
        let mut step = Step::NoRoom;
        if self.tank.amount <= 9000 {
            if self.energy >= cost {
                let paid = self.pump_substance != 0;
                if paid {
                    self.energy -= cost;
                }
                self.progress += 1;
                step = Step::Worked;
                if self.progress >= self.ticks() {
                    self.progress = 0;
                    match self.suck(p, w) {
                        Ok(true) => {
                            if !paid {
                                self.energy -= cost;
                            }
                        }
                        Ok(false) => {
                            self.pump_substance = 0;
                            self.pump_node_count = 0;
                            step = Step::Idle;
                        }
                        Err(reason) => {
                            self.progress = self.ticks() - 1;
                            step = reason;
                        }
                    }
                }
            } else {
                step = Step::NoEnergy;
            }
        }
        let t = self.tank;
        let n = w
            .push_substance(p, Face::Up, t.substance, t.amount.min(256))
            .min(t.amount.min(256));
        self.tank.take(n);
        step
    }
}

pub fn wind_power(base_y: i32) -> u64 {
    let top = i64::from(base_y).saturating_add(4).clamp(24, 127);
    (60.0 + 420.0 / 103.0 * (top - 24) as f64) as u64
}
