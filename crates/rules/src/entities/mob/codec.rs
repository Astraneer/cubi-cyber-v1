//! Versioned private save values. Public replication should expose visible pose
//! fields, never private AI state or random-event counters.
use super::*;
pub const MOB_BYTES: usize = 320;
struct Writer {
    bytes: [u8; MOB_BYTES],
    cursor: usize,
}
impl Writer {
    fn put(&mut self, bytes: &[u8]) {
        self.bytes[self.cursor..self.cursor + bytes.len()].copy_from_slice(bytes);
        self.cursor += bytes.len();
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], EntityError> {
        let end = self
            .cursor
            .checked_add(N)
            .ok_or(EntityError::InvalidEncoding)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(EntityError::InvalidEncoding)?
            .try_into()
            .map_err(|_| EntityError::InvalidEncoding)?;
        self.cursor = end;
        Ok(value)
    }
}
impl Mob {
    fn validate(&self) -> Result<(), EntityError> {
        validate_position(self.body.position)?;
        validate_position(self.body.previous)?;
        if self
            .body
            .velocity
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 64.0)
            || [
                self.body.yaw,
                self.body.head,
                self.body.pitch,
                self.body.fall,
                self.limb_phase,
                self.limb_amount,
                self.ai.speed_multiplier,
            ]
            .iter()
            .any(|value| !value.is_finite())
            || self.body.fall < 0.0
            || self.body.jump_cooldown > 10
            || !(0.0..=2.0).contains(&self.limb_amount)
            || !(0.0..=8.0).contains(&self.ai.speed_multiplier)
            || self.ai.eating > 40
            || self.color > 15
            || (self.loot_emitted && self.life.alive())
            || self.life.maximum() != self.kind.definition().health
        {
            return Err(EntityError::InvalidEncoding);
        }
        if let Some(destination) = self.ai.destination {
            validate_position(destination)?;
        }
        Health::decode(&self.life.encode()).map_err(|_| EntityError::InvalidEncoding)?;
        Ok(())
    }
    pub fn encode(&self) -> Result<[u8; MOB_BYTES], EntityError> {
        self.validate()?;
        let mut writer = Writer {
            bytes: [0; MOB_BYTES],
            cursor: 0,
        };
        writer.put(b"MOB1");
        writer.put(&self.id.0.to_le_bytes());
        writer.put(&[
            self.kind as u8,
            u8::from(self.body.on_ground)
                | (u8::from(self.body.in_water) << 1)
                | (u8::from(self.body.in_lava) << 2)
                | (u8::from(self.baby) << 3)
                | (u8::from(self.sheared) << 4)
                | (u8::from(self.body.horizontal_collision) << 5),
            self.color,
            self.body.jump_cooldown,
        ]);
        for value in self
            .body
            .position
            .into_iter()
            .chain(self.body.previous)
            .chain(self.body.velocity)
        {
            writer.put(&value.to_bits().to_le_bytes());
        }
        for value in [
            self.body.yaw,
            self.body.head,
            self.body.pitch,
            self.body.fall,
        ] {
            writer.put(&value.to_bits().to_le_bytes());
        }
        writer.put(&self.life.encode());
        writer.put(&self.age.to_le_bytes());
        for value in [self.limb_phase, self.limb_amount] {
            writer.put(&value.to_bits().to_le_bytes());
        }
        writer.put(&[
            self.ai.goal as u8,
            u8::from(self.ai.target.is_some()),
            u8::from(self.ai.destination.is_some()),
            self.ai.eating,
        ]);
        writer.put(&self.ai.target.map_or(0, |id| id.0).to_le_bytes());
        for value in self.ai.destination.unwrap_or([0.0; 3]) {
            writer.put(&value.to_bits().to_le_bytes());
        }
        writer.put(&self.ai.goal_until.0.to_le_bytes());
        writer.put(&self.ai.panic_until.0.to_le_bytes());
        writer.put(&self.ai.attack_cooldown.to_le_bytes());
        writer.put(&[0; 2]);
        writer.put(&self.ai.event.to_le_bytes());
        writer.put(&self.ai.speed_multiplier.to_bits().to_le_bytes());
        writer.put(&self.space.0.to_le_bytes());
        writer.put(&self.last_tick.0.to_le_bytes());
        writer.put(&[u8::from(self.loot_emitted)]);
        debug_assert_eq!(writer.cursor, 265);
        Ok(writer.bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, EntityError> {
        if bytes.len() != MOB_BYTES
            || &bytes[..4] != b"MOB1"
            || bytes[265..].iter().any(|&value| value != 0)
        {
            return Err(EntityError::InvalidEncoding);
        }
        let mut reader = Reader { bytes, cursor: 4 };
        let id = EntityId(u64::from_le_bytes(reader.take()?));
        let [kind, flags, color, jump_cooldown] = reader.take()?;
        if flags & !63 != 0 || jump_cooldown > 10 {
            return Err(EntityError::InvalidEncoding);
        }
        let kind = MobKind::from_byte(kind).ok_or(EntityError::InvalidEncoding)?;
        let mut vectors = [[0.0; 3]; 3];
        for vector in &mut vectors {
            for value in vector {
                *value = f64::from_bits(u64::from_le_bytes(reader.take()?));
            }
        }
        let mut angles = [0.0; 4];
        for value in &mut angles {
            *value = f32::from_bits(u32::from_le_bytes(reader.take()?));
        }
        let life =
            Health::decode(&reader.take::<64>()?).map_err(|_| EntityError::InvalidEncoding)?;
        let age = u64::from_le_bytes(reader.take()?);
        let limb_phase = f32::from_bits(u32::from_le_bytes(reader.take()?));
        let limb_amount = f32::from_bits(u32::from_le_bytes(reader.take()?));
        let [goal, has_target, has_destination, eating] = reader.take()?;
        let goals = [
            Goal::Idle,
            Goal::Swim,
            Goal::Panic,
            Goal::Tempt,
            Goal::EatGrass,
            Goal::Wander,
            Goal::Melee,
        ];
        let goal = *goals
            .get(goal as usize)
            .ok_or(EntityError::InvalidEncoding)?;
        if has_target > 1 || has_destination > 1 {
            return Err(EntityError::InvalidEncoding);
        }
        let target = u64::from_le_bytes(reader.take()?);
        let mut destination = [0.0; 3];
        for value in &mut destination {
            *value = f64::from_bits(u64::from_le_bytes(reader.take()?));
        }
        if (has_target == 0 && target != 0)
            || (has_destination == 0 && destination.iter().any(|&value| value.to_bits() != 0))
        {
            return Err(EntityError::InvalidEncoding);
        }
        let goal_until = Tick(u64::from_le_bytes(reader.take()?));
        let panic_until = Tick(u64::from_le_bytes(reader.take()?));
        let attack_cooldown = u16::from_le_bytes(reader.take()?);
        if reader.take::<2>()? != [0; 2] {
            return Err(EntityError::InvalidEncoding);
        }
        let event = u64::from_le_bytes(reader.take()?);
        let speed_multiplier = f32::from_bits(u32::from_le_bytes(reader.take()?));
        let space = SpaceId(u32::from_le_bytes(reader.take()?));
        let last_tick = Tick(u64::from_le_bytes(reader.take()?));
        let [loot_emitted] = reader.take()?;
        if loot_emitted > 1 {
            return Err(EntityError::InvalidEncoding);
        }
        let mob = Self {
            id,
            space,
            kind,
            body: Body {
                position: vectors[0],
                previous: vectors[1],
                velocity: vectors[2],
                yaw: angles[0],
                head: angles[1],
                pitch: angles[2],
                fall: angles[3],
                on_ground: flags & 1 != 0,
                in_water: flags & 2 != 0,
                in_lava: flags & 4 != 0,
                jump_cooldown,
                horizontal_collision: flags & 32 != 0,
            },
            life,
            ai: AiState {
                goal,
                target: (has_target == 1).then_some(EntityId(target)),
                destination: (has_destination == 1).then_some(destination),
                goal_until,
                panic_until,
                attack_cooldown,
                eating,
                event,
                speed_multiplier,
            },
            age,
            last_tick,
            loot_emitted: loot_emitted == 1,
            baby: flags & 8 != 0,
            sheared: flags & 16 != 0,
            color,
            limb_phase,
            limb_amount,
        };
        mob.validate()?;
        Ok(mob)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntitySnapshot {
    pub mobs: Vec<Mob>,
    pub active: Vec<EntityId>,
    pub cursor: usize,
}
impl EntityStore {
    pub fn snapshot(&self) -> EntitySnapshot {
        EntitySnapshot {
            mobs: self.rows.clone(),
            active: self
                .active
                .iter()
                .map(|&index| self.rows[index].id)
                .collect(),
            cursor: self.cursor,
        }
    }
    pub fn restore(capacity: usize, snapshot: &EntitySnapshot) -> Result<Self, EntityError> {
        if snapshot.mobs.len() > capacity
            || snapshot.active.len() > snapshot.mobs.len()
            || (snapshot.active.is_empty() && snapshot.cursor != 0)
            || (!snapshot.active.is_empty() && snapshot.cursor >= snapshot.active.len())
        {
            return Err(EntityError::InvalidEncoding);
        }
        let mut store = Self::new(capacity)?;
        for &mob in &snapshot.mobs {
            mob.validate()?;
            store.insert(mob, false)?;
        }
        for &id in &snapshot.active {
            if store.active_slots[*store.ids.get(&id).ok_or(EntityError::UnknownId)?].is_some() {
                return Err(EntityError::DuplicateId);
            }
            store.set_active(id, true)?;
        }
        store.cursor = snapshot.cursor;
        Ok(store)
    }
}
impl EntitySnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, EntityError> {
        if self.mobs.len() > 1_000_000 || self.active.len() > self.mobs.len() {
            return Err(EntityError::Capacity);
        }
        let mut bytes =
            Vec::with_capacity(16 + self.mobs.len() * MOB_BYTES + self.active.len() * 8);
        bytes.extend_from_slice(b"ETS1");
        for value in [self.mobs.len(), self.active.len(), self.cursor] {
            bytes.extend_from_slice(&(value as u32).to_le_bytes());
        }
        for mob in &self.mobs {
            bytes.extend_from_slice(&mob.encode()?);
        }
        for id in &self.active {
            bytes.extend_from_slice(&id.0.to_le_bytes());
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], capacity: usize) -> Result<Self, EntityError> {
        if bytes.len() < 16 || &bytes[..4] != b"ETS1" {
            return Err(EntityError::InvalidEncoding);
        }
        let read = |index| {
            u32::from_le_bytes(bytes[index..index + 4].try_into().expect("header bounds")) as usize
        };
        let mobs = read(4);
        let active = read(8);
        let cursor = read(12);
        if mobs > capacity || active > mobs {
            return Err(EntityError::Capacity);
        }
        let boundary = mobs
            .checked_mul(MOB_BYTES)
            .and_then(|value| value.checked_add(16))
            .ok_or(EntityError::InvalidEncoding)?;
        if active
            .checked_mul(8)
            .and_then(|value| value.checked_add(boundary))
            != Some(bytes.len())
        {
            return Err(EntityError::InvalidEncoding);
        }
        let snapshot = Self {
            mobs: bytes[16..boundary]
                .chunks_exact(MOB_BYTES)
                .map(Mob::decode)
                .collect::<Result<_, _>>()?,
            active: bytes[boundary..]
                .chunks_exact(8)
                .map(|raw| EntityId(u64::from_le_bytes(raw.try_into().expect("fixed ID"))))
                .collect(),
            cursor,
        };
        EntityStore::restore(capacity, &snapshot)?;
        Ok(snapshot)
    }
}
