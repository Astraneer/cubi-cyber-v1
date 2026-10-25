//! Java 1.21.1 damage-window/armor formulas and the donor's selected survival
//! rules: food heals directly; regeneration starts after 100 quiet ticks and
//! heals one HP per 40 further ticks. No hunger system is invented here.

use super::{Inventory, SurvivalError};
use crate::items::food_nutrition;

pub const MAX_HP: f32 = 20.0;
pub const MAX_AIR: i16 = 300;
pub const HEALTH_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DamageKind {
    Fall,
    InWall,
    Drown,
    Cactus,
    OutOfWorld,
    PlayerAttack,
    Lava,
    OnFire,
    InFire,
    Generic,
    MobAttack,
}
impl DamageKind {
    pub const fn bypasses_armor(self) -> bool {
        !matches!(
            self,
            Self::Cactus
                | Self::PlayerAttack
                | Self::MobAttack
                | Self::Lava
                | Self::OnFire
                | Self::InFire
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub full: bool,
    pub raw: f32,
    pub taken: f32,
    pub armor_wear: u16,
    pub died: bool,
}

/// Integer-ceil implementation preserves the Java f32 multiply/subtract order.
pub fn fall_damage(distance: f32, landing_multiplier: f32) -> u32 {
    let damage = (distance - 3.0) * landing_multiplier;
    if !damage.is_finite() || damage <= 0.0 {
        return 0;
    }
    let truncated = damage as u32;
    truncated.saturating_add(u32::from(damage > truncated as f32))
}

pub fn armor_left(damage: f32, armor: f32, toughness: f32) -> f32 {
    if !damage.is_finite() || damage <= 0.0 {
        return 0.0;
    }
    let armor = if armor.is_finite() {
        armor.clamp(0.0, 30.0)
    } else {
        0.0
    };
    let toughness = if toughness.is_finite() {
        toughness.max(0.0)
    } else {
        0.0
    };
    let reduced = (armor - damage / (2.0 + toughness / 4.0)).clamp(armor * 0.2, 20.0);
    damage * (1.0 - reduced / 25.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Health {
    hp: f32,
    max: f32,
    now: u64,
    last_hurt: u64,
    last_damage: f32,
    regen_timer: u16,
    window: u8,
    spawn_protect: u8,
    hurt_time: u8,
    air: i16,
    fire: u16,
    death_time: u16,
    death_kind: Option<DamageKind>,
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}
impl Health {
    pub const fn new() -> Self {
        Self {
            hp: MAX_HP,
            max: MAX_HP,
            now: 0,
            last_hurt: 0,
            last_damage: 0.0,
            regen_timer: 0,
            window: 0,
            spawn_protect: 60,
            hurt_time: 0,
            air: MAX_AIR,
            fire: 0,
            death_time: 0,
            death_kind: None,
        }
    }
    pub const fn hp(&self) -> f32 {
        self.hp
    }
    pub const fn maximum(&self) -> f32 {
        self.max
    }
    pub fn with_maximum(maximum: f32) -> Result<Self, SurvivalError> {
        if !maximum.is_finite() || !(0.0..=1024.0).contains(&maximum) || maximum == 0.0 {
            return Err(SurvivalError::InvalidEncoding);
        }
        Ok(Self {
            hp: maximum,
            max: maximum,
            ..Self::new()
        })
    }
    pub const fn now(&self) -> u64 {
        self.now
    }
    pub const fn air(&self) -> i16 {
        self.air
    }
    pub const fn fire(&self) -> u16 {
        self.fire
    }
    pub const fn hurt_time(&self)->u8 {self.hurt_time}
    pub const fn death_time(&self) -> u16 {
        self.death_time
    }
    pub const fn death_kind(&self) -> Option<DamageKind> {
        self.death_kind
    }
    pub const fn alive(&self) -> bool {
        self.hp > 0.0
    }
    pub const fn can_eat(&self) -> bool {
        self.alive() && self.hp < self.max
    }
    pub const fn may_respawn(&self) -> bool {
        !self.alive() && self.death_time >= 20
    }
    /// Useful for explicit server policy and deterministic fixture setup.
    pub fn set_spawn_protection(&mut self, ticks: u8) {
        self.spawn_protect = ticks;
    }
    pub fn begin_tick(&mut self) {
        self.now = self.now.saturating_add(1);
        self.window = self.window.saturating_sub(1);
        self.spawn_protect = self.spawn_protect.saturating_sub(1);
    }
    pub fn damage(
        &mut self,
        kind: DamageKind,
        amount: f32,
        armor: (u8, f32),
        invulnerable: bool,
    ) -> Option<Hit> {
        if !self.alive()
            || !amount.is_finite()
            || amount <= 0.0
            || !armor.1.is_finite()
            || armor.1 < 0.0
        {
            return None;
        }
        if kind != DamageKind::OutOfWorld && (invulnerable || self.spawn_protect > 0) {
            return None;
        }
        let full = self.window <= 10;
        let raw = if full {
            self.window = 20;
            self.hurt_time = 10;
            amount
        } else {
            if amount <= self.last_damage {
                return None;
            }
            amount - self.last_damage
        };
        self.last_damage = amount;
        let mitigated = !kind.bypasses_armor();
        let left = if mitigated {
            armor_left(raw, f32::from(armor.0.min(30)), armor.1)
        } else {
            raw
        };
        let before = self.hp;
        self.hp = (self.hp - left).max(0.0);
        self.last_hurt = self.now;
        self.regen_timer = 0;
        if !self.alive() {
            self.death_kind = Some(kind);
        }
        Some(Hit {
            full,
            raw,
            taken: before - self.hp,
            armor_wear: if mitigated && armor.0 > 0 {
                (raw / 4.0).max(1.0) as u16
            } else {
                0
            },
            died: !self.alive(),
        })
    }
    pub fn heal(&mut self, amount: f32) {
        if self.alive() && amount.is_finite() && amount > 0.0 {
            self.hp = (self.hp + amount).min(self.max);
        }
    }
    pub fn tick_air(&mut self, underwater: bool) -> Option<f32> {
        if underwater {
            self.air -= 1;
            if self.air == -20 {
                self.air = 0;
                return Some(2.0);
            }
        } else {
            self.air = (self.air + 4).min(MAX_AIR);
        }
        None
    }
    pub fn ignite(&mut self, ticks: u16) {
        self.fire = self.fire.max(ticks);
    }
    pub fn tick_fire(&mut self, wet: bool, in_lava: bool) -> Option<f32> {
        if wet {
            self.fire = 0;
        }
        if self.fire == 0 {
            return None;
        }
        let damage = self.fire.is_multiple_of(20) && !in_lava;
        self.fire -= 1;
        damage.then_some(1.0)
    }
    pub fn base_tick_end(&mut self) {
        self.hurt_time = self.hurt_time.saturating_sub(1);
        if !self.alive() {
            self.death_time = self.death_time.saturating_add(1);
        }
    }
    pub fn end_tick(&mut self) -> bool {
        self.base_tick_end();
        self.regen_tick()
    }
    pub fn regen_tick(&mut self) -> bool {
        if self.alive()
            && self.hp < self.max
            && self.now.saturating_sub(self.last_hurt) > 100
            && self.fire == 0
        {
            self.regen_timer += 1;
            if self.regen_timer == 40 {
                self.heal(1.0);
                self.regen_timer = 0;
                return true;
            }
        } else {
            self.regen_timer = 0;
        }
        false
    }
    pub fn encode(&self) -> [u8; HEALTH_BYTES] {
        let mut bytes = [0; HEALTH_BYTES];
        bytes[..4].copy_from_slice(b"HPS1");
        bytes[4..8].copy_from_slice(&self.hp.to_bits().to_le_bytes());
        bytes[8..16].copy_from_slice(&self.now.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.last_hurt.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.last_damage.to_bits().to_le_bytes());
        bytes[28..30].copy_from_slice(&self.regen_timer.to_le_bytes());
        bytes[30] = self.window;
        bytes[31] = self.spawn_protect;
        bytes[32] = self.hurt_time;
        bytes[33..35].copy_from_slice(&self.air.to_le_bytes());
        bytes[35..37].copy_from_slice(&self.fire.to_le_bytes());
        bytes[37..39].copy_from_slice(&self.death_time.to_le_bytes());
        bytes[39] = self.death_kind.map_or(255, |kind| kind as u8);
        bytes[40..44].copy_from_slice(&self.max.to_bits().to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SurvivalError> {
        if bytes.len() != HEALTH_BYTES
            || &bytes[..4] != b"HPS1"
            || bytes[44..].iter().any(|&v| v != 0)
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let u16_at = |i| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let f32_at = |i| f32::from_le_bytes(bytes[i..i + 4].try_into().expect("fixed length"));
        let kinds = [
            DamageKind::Fall,
            DamageKind::InWall,
            DamageKind::Drown,
            DamageKind::Cactus,
            DamageKind::OutOfWorld,
            DamageKind::PlayerAttack,
            DamageKind::Lava,
            DamageKind::OnFire,
            DamageKind::InFire,
            DamageKind::Generic,
            DamageKind::MobAttack,
        ];
        let kind = if bytes[39] == 255 {
            None
        } else {
            Some(
                *kinds
                    .get(bytes[39] as usize)
                    .ok_or(SurvivalError::InvalidEncoding)?,
            )
        };
        let health = Self {
            hp: f32_at(4),
            max: f32_at(40),
            now: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed length")),
            last_hurt: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed length")),
            last_damage: f32_at(24),
            regen_timer: u16_at(28),
            window: bytes[30],
            spawn_protect: bytes[31],
            hurt_time: bytes[32],
            air: i16::from_le_bytes([bytes[33], bytes[34]]),
            fire: u16_at(35),
            death_time: u16_at(37),
            death_kind: kind,
        };
        if !health.hp.is_finite()
            || !health.max.is_finite()
            || health.max <= 0.0
            || health.max > 1024.0
            || !(0.0..=health.max).contains(&health.hp)
            || !health.last_damage.is_finite()
            || health.last_damage < 0.0
            || health.last_hurt > health.now
            || health.regen_timer >= 40
            || health.window > 20
            || health.hurt_time > 10
            || !(-19..=MAX_AIR).contains(&health.air)
            || (health.alive() && (health.death_time > 0 || health.death_kind.is_some()))
            || (!health.alive() && health.death_kind.is_none())
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        Ok(health)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Eating {
    slot: usize,
    item: u16,
    left: u8,
}
impl Eating {
    pub fn start(health: &Health, inventory: &Inventory) -> Option<Self> {
        (health.can_eat() && food_nutrition(inventory.hand().item()).is_some()).then_some(Self {
            slot: inventory.selected(),
            item: inventory.hand().item(),
            left: 32,
        })
    }
    pub const fn ticks_left(&self) -> u8 {
        self.left
    }
    /// The caller owns a copied authoritative player state until fsync. Releasing
    /// use or changing held food cancels; damage does not cancel eating.
    pub fn tick(
        &mut self,
        held: bool,
        health: &mut Health,
        inventory: &mut Inventory,
    ) -> Result<EatOutcome, SurvivalError> {
        if !held
            || !health.alive()
            || inventory.selected() != self.slot
            || inventory.hand().item() != self.item
            || inventory.hand().is_empty()
        {
            self.left = 0;
            return Ok(EatOutcome::Cancelled);
        }
        if self.left == 0 {
            return Ok(EatOutcome::Cancelled);
        }
        self.left -= 1;
        if self.left > 0 {
            return Ok(EatOutcome::Eating);
        }
        let nutrition = food_nutrition(self.item).ok_or(SurvivalError::NoEntitlement)?;
        inventory
            .plan_consume(self.slot, inventory.hand(), 1)?
            .apply(inventory)?;
        health.heal(f32::from(nutrition));
        Ok(EatOutcome::Finished)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EatOutcome {
    Eating,
    Finished,
    Cancelled,
}
