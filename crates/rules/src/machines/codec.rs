//! Versioned bounded state for an owner-side block-entity component.
use super::*;

const LEGACY_MACHINE_BYTES: usize = 3211;
pub const MACHINE_BYTES: usize = LEGACY_MACHINE_BYTES + 6 + 5 * 10;
pub const MACHINE_HEADER_BYTES: usize = 139;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineDecodeError {
    Length,
    Magic,
    Value,
    Stack,
}
struct Writer<'a> {
    bytes: &'a mut [u8],
    at: usize,
}
impl Writer<'_> {
    fn raw(&mut self, b: &[u8]) {
        self.bytes[self.at..self.at + b.len()].copy_from_slice(b);
        self.at += b.len();
    }
    fn u8(&mut self, n: u8) {
        self.raw(&[n]);
    }
    fn u16(&mut self, n: u16) {
        self.raw(&n.to_le_bytes());
    }
    fn u32(&mut self, n: u32) {
        self.raw(&n.to_le_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.raw(&n.to_le_bytes());
    }
    fn tank(&mut self, t: Tank) {
        self.u8(t.substance);
        self.u32(t.amount);
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn raw<const N: usize>(&mut self) -> [u8; N] {
        let b = self.bytes[self.at..self.at + N]
            .try_into()
            .expect("fixed validated size");
        self.at += N;
        b
    }
    fn u8(&mut self) -> u8 {
        self.raw::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.raw())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.raw())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.raw())
    }
    fn tank(&mut self) -> Tank {
        Tank {
            substance: self.u8(),
            amount: self.u32(),
        }
    }
    fn boolean(&mut self) -> Result<bool, MachineDecodeError> {
        match self.u8() {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MachineDecodeError::Value),
        }
    }
}

impl Machine {
    /// Returns the used prefix: 139 bytes for an ordinary machine, plus only
    /// the actual pump frontier. The fixed buffer is the maximum, not wire cost.
    pub fn encode(&self, out: &mut [u8; MACHINE_BYTES]) -> usize {
        out.fill(0);
        let mut w = Writer { bytes: out, at: 0 };
        w.raw(b"MEK1");
        w.u8(self.kind as u8);
        for s in self.slots {
            w.u16(s.stack.item());
            w.u16(s.stack.count());
            w.u16(s.stack.damage());
            w.u32(s.charge);
        }
        w.u64(self.energy);
        w.u32(self.progress);
        w.u16(self.recipe.unwrap_or(u16::MAX));
        w.tank(self.tank);
        for t in self.gas {
            w.tank(t);
        }
        for m in self.gas_mode {
            w.u8(m as u8);
        }
        w.u32(self.gas_used);
        w.raw(&self.upgrades);
        w.u8(self.upgrade_delay);
        w.u8(self.active.logic as u8);
        w.u8(self.active.shown as u8);
        w.u8(self.active.delay);
        w.u64(self.heat.to_bits());
        w.u64(self.produced);
        w.u64(self.ticker);
        w.u8(self.last as u8);
        for s in self.sides {
            w.u8(s as u8);
        }
        w.raw(&self.gas_sides);
        w.u8(self.eject as u8);
        w.u8(self.eject_delay);
        w.u8(self.pump_substance);
        w.u16(self.pump_node_count);
        for p in &self.pump_nodes[..usize::from(self.pump_node_count)] {
            for c in p {
                w.u32(*c as u32);
            }
        }
        debug_assert_eq!(
            w.at,
            MACHINE_HEADER_BYTES + usize::from(self.pump_node_count) * 12
        );
        let count = self
            .slots
            .iter()
            .filter(|s| s.stack.payload().is_some())
            .count();
        if count > 0 {
            w.raw(b"XRF1");
            w.u16(count as u16);
            for (i, slot) in self.slots.iter().enumerate() {
                if let Some(id) = slot.stack.payload() {
                    w.u16(i as u16);
                    w.u64(id.0);
                }
            }
        }
        w.at
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, MachineDecodeError> {
        use MachineDecodeError::*;
        if !(MACHINE_HEADER_BYTES..=MACHINE_BYTES).contains(&bytes.len()) {
            return Err(Length);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.raw::<4>() != *b"MEK1" {
            return Err(Magic);
        }
        let kind = match r.u8() {
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
            _ => return Err(Value),
        };
        let mut m = Self::new(kind);
        for slot in &mut m.slots {
            slot.stack = ItemStack::from_parts(r.u16(), r.u16(), r.u16()).map_err(|_| Stack)?;
            slot.charge = r.u32();
            if slot.charge > TABLET_CAPACITY
                || slot.charge % 20 != 0
                || (slot.charge > 0 && slot.stack.item() != I_ENERGY_TABLET)
            {
                return Err(Value);
            }
        }
        m.energy = r.u64();
        m.progress = r.u32();
        let key = r.u16();
        m.recipe = (key != u16::MAX).then_some(key);
        m.tank = r.tank();
        for t in &mut m.gas {
            *t = r.tank();
        }
        for mode in &mut m.gas_mode {
            *mode = match r.u8() {
                0 => GasMode::Store,
                1 => GasMode::Excess,
                2 => GasMode::Dump,
                _ => return Err(Value),
            };
        }
        m.gas_used = r.u32();
        m.upgrades = r.raw();
        m.upgrade_delay = r.u8();
        m.active = ActiveView {
            logic: r.boolean()?,
            shown: r.boolean()?,
            delay: r.u8(),
        };
        m.heat = f64::from_bits(r.u64());
        m.produced = r.u64();
        m.ticker = r.u64();
        m.last = match r.u8() {
            0 => Step::Idle,
            1 => Step::Reset,
            2 => Step::NoEnergy,
            3 => Step::NoRoom,
            4 => Step::WrongOutput,
            5 => Step::Worked,
            6 => Step::Finished,
            7 => Step::NoGas,
            8 => Step::NoWater,
            9 => Step::HydrogenFull,
            10 => Step::OxygenFull,
            11 => Step::Budget,
            _ => return Err(Value),
        };
        for s in &mut m.sides {
            *s = match r.u8() {
                0 => SideMode::None,
                1 => SideMode::Input,
                2 => SideMode::Output,
                3 => SideMode::Both,
                4 => SideMode::Energy,
                5 => SideMode::Extra,
                _ => return Err(Value),
            };
        }
        m.gas_sides = r.raw();
        m.eject = r.boolean()?;
        m.eject_delay = r.u8();
        m.pump_substance = r.u8();
        m.pump_node_count = r.u16();
        let count = usize::from(m.pump_node_count);
        if count > MAX_PUMP_NODES {
            return Err(Value);
        }
        let used = MACHINE_HEADER_BYTES + count * 12;
        if bytes.len() < used {
            return Err(Length);
        }
        for p in &mut m.pump_nodes[..count] {
            for c in p {
                *c = r.u32() as i32;
            }
        }
        let (refs, ref_bytes) =
            crate::item_payload::refs::read(&bytes[used..], 5).map_err(|_| Stack)?;
        let total = used + ref_bytes;
        if bytes.len() != total
            && (!matches!(bytes.len(), LEGACY_MACHINE_BYTES | MACHINE_BYTES)
                || bytes[total..].iter().any(|&b| b != 0))
        {
            return Err(Length);
        }
        for (i, id) in refs {
            if m.slots[i].charge != 0 {
                return Err(Value);
            }
            m.slots[i].stack = m.slots[i].stack.with_payload(id).map_err(|_| Stack)?;
        }
        if m.upgrades.iter().any(|&u| u > 8)
            || m.upgrade_delay > 20
            || m.active.delay > 60
            || m.eject_delay > 10
            || m.gas_sides.iter().any(|&s| s > 2)
            || usize::from(m.pump_node_count) > MAX_PUMP_NODES
            || !m.heat.is_finite()
            || m.heat < 0.0
            || m.heat > 1e12
            || m.energy > m.capacity()
            || m.progress > m.ticks().max(200)
            || m.gas_used > 2000
            || m.pump_substance > recipes::SUB_WATER
        {
            return Err(Value);
        }
        for (t, cap) in [
            (m.tank, definition(m.kind).tank_capacity),
            (m.gas[0], 2400),
            (m.gas[1], 2400),
        ] {
            if t.amount > cap
                || t.substance > recipes::SUB_WATER
                || (t.amount == 0) != (t.substance == 0)
            {
                return Err(Value);
            }
        }
        if m.gas[0].amount > 0 && m.gas[0].substance != recipes::SUB_HYDROGEN {
            return Err(Value);
        }
        if m.gas[1].amount > 0 && m.gas[1].substance != recipes::SUB_OXYGEN {
            return Err(Value);
        }
        if m.tank.amount > 0 {
            let accepts = match m.kind {
                MekKind::Infuser => {
                    [recipes::SUB_CARBON, recipes::SUB_REDSTONE].contains(&m.tank.substance)
                }
                MekKind::Heat => m.tank.substance == recipes::SUB_LAVA,
                MekKind::Purifier => m.tank.substance == recipes::SUB_OXYGEN,
                MekKind::Separator => m.tank.substance == recipes::SUB_WATER,
                MekKind::Pump => {
                    [recipes::SUB_WATER, recipes::SUB_LAVA].contains(&m.tank.substance)
                }
                _ => false,
            };
            if !accepts {
                return Err(Value);
            }
        }
        Ok(m)
    }
}
