//! Deterministic entity values and bounded owner-side AI/combat helpers. Physics
//! adapters reuse `sim::collision`; the registry layer never owns a world.
pub mod combat;
pub mod item;
pub mod mob;
pub mod spatial;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityError {
    Capacity,
    DuplicateId,
    UnknownId,
    InvalidPosition,
    InvalidEncoding,
    NoEntitlement,
    UnknownInput,
    StalePlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MobKind {
    Pig,
    Cow,
    Sheep,
    Chicken,
    Zombie,
}
impl MobKind {
    pub const ALL: [Self; 5] = [
        Self::Pig,
        Self::Cow,
        Self::Sheep,
        Self::Chicken,
        Self::Zombie,
    ];
    pub fn from_byte(value: u8) -> Option<Self> {
        Self::ALL.get(value as usize).copied()
    }
    pub fn definition(self) -> &'static KindDef {
        &KINDS[self as usize]
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KindDef {
    pub name: &'static str,
    pub width: f64,
    pub height: f64,
    pub eye: f64,
    pub health: f32,
    pub speed: f32,
    pub panic: f32,
    pub armor: u8,
    pub attack: f32,
    pub follow: f64,
    pub look_range: f64,
    pub water_penalty: f32,
    pub hostile: bool,
    pub fall_immune: bool,
}
pub const KINDS: [KindDef; 5] = [
    KindDef {
        name: "pig",
        width: 0.9,
        height: 0.9,
        eye: 0.765,
        health: 10.0,
        speed: 0.25,
        panic: 1.25,
        armor: 0,
        attack: 0.0,
        follow: 16.0,
        look_range: 6.0,
        water_penalty: 8.0,
        hostile: false,
        fall_immune: false,
    },
    KindDef {
        name: "cow",
        width: 0.9,
        height: 1.4,
        eye: 1.3,
        health: 10.0,
        speed: 0.2,
        panic: 2.0,
        armor: 0,
        attack: 0.0,
        follow: 16.0,
        look_range: 6.0,
        water_penalty: 8.0,
        hostile: false,
        fall_immune: false,
    },
    KindDef {
        name: "sheep",
        width: 0.9,
        height: 1.3,
        eye: 1.235,
        health: 8.0,
        speed: 0.23,
        panic: 1.25,
        armor: 0,
        attack: 0.0,
        follow: 16.0,
        look_range: 6.0,
        water_penalty: 8.0,
        hostile: false,
        fall_immune: false,
    },
    KindDef {
        name: "chicken",
        width: 0.4,
        height: 0.7,
        eye: 0.644,
        health: 4.0,
        speed: 0.25,
        panic: 1.4,
        armor: 0,
        attack: 0.0,
        follow: 16.0,
        look_range: 6.0,
        water_penalty: 0.0,
        hostile: false,
        fall_immune: true,
    },
    KindDef {
        name: "zombie",
        width: 0.6,
        height: 1.95,
        eye: 1.74,
        health: 20.0,
        speed: 0.23,
        panic: 1.0,
        armor: 2,
        attack: 3.0,
        follow: 35.0,
        look_range: 8.0,
        water_penalty: 8.0,
        hostile: true,
        fall_immune: false,
    },
];
