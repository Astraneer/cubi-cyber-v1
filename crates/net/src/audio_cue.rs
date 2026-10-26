//! Fixed causal audio event. No item state, RNG, or private actor fields.
use core_types::{SpaceId, Tick};
pub const BYTES: usize = 64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ChickenEgg,
    ChestOpen,
    ChestClose,
    ItemPickup,
    AttackWeak,
    AttackStrong,
    AttackCrit,
    AttackKnockback,
    BucketFill,
    BucketEmpty,
    Eat,
    EatDone,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub kind: Kind,
    pub actor: u64,
    pub sequence: u64,
    pub tick: Tick,
    pub space: SpaceId,
    pub position: [f64; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
}
impl Cue {
    pub fn validate(self) -> Result<(), Error> {
        if self.actor == 0
            || self.sequence == 0
            || (matches!(self.kind, Kind::ChickenEgg | Kind::ItemPickup)
                && self.sequence != self.tick.0)
            || self
                .position
                .iter()
                .any(|v| !v.is_finite() || v.abs() >= f64::from(crate::MAX_COORDINATE))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn encode(self) -> Result<[u8; BYTES], Error> {
        self.validate()?;
        let mut bytes = [0; BYTES];
        bytes[..4].copy_from_slice(b"ACU1");
        bytes[4] = match self.kind {
            Kind::ChickenEgg => 0,
            Kind::ChestOpen => 1,
            Kind::ChestClose => 2,
            Kind::ItemPickup => 3,
            Kind::AttackWeak => 4,
            Kind::AttackStrong => 5,
            Kind::AttackCrit => 6,
            Kind::AttackKnockback => 7,
            Kind::BucketFill => 8,
            Kind::BucketEmpty => 9,
            Kind::Eat => 10,
            Kind::EatDone => 11,
        };
        bytes[8..16].copy_from_slice(&self.actor.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.tick.0.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.space.0.to_le_bytes());
        for (i, p) in self.position.iter().enumerate() {
            bytes[40 + 8 * i..48 + 8 * i].copy_from_slice(&p.to_le_bytes());
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != BYTES
            || &bytes[..4] != b"ACU1"
            || bytes[5..8] != [0; 3]
            || bytes[36..40] != [0; 4]
        {
            return Err(Error::Invalid);
        }
        let kind = match bytes[4] {
            0 => Kind::ChickenEgg,
            1 => Kind::ChestOpen,
            2 => Kind::ChestClose,
            3 => Kind::ItemPickup,
            4 => Kind::AttackWeak,
            5 => Kind::AttackStrong,
            6 => Kind::AttackCrit,
            7 => Kind::AttackKnockback,
            8 => Kind::BucketFill,
            9 => Kind::BucketEmpty,
            10 => Kind::Eat,
            11 => Kind::EatDone,
            _ => return Err(Error::Invalid),
        };
        let actor = u64::from_le_bytes(bytes[8..16].try_into().map_err(|_| Error::Invalid)?);
        let sequence = u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| Error::Invalid)?);
        let tick = Tick(u64::from_le_bytes(
            bytes[24..32].try_into().map_err(|_| Error::Invalid)?,
        ));
        let space = SpaceId(u32::from_le_bytes(
            bytes[32..36].try_into().map_err(|_| Error::Invalid)?,
        ));
        let position = std::array::from_fn(|i| {
            f64::from_le_bytes(
                bytes[40 + 8 * i..48 + 8 * i]
                    .try_into()
                    .expect("fixed position"),
            )
        });
        let cue = Self {
            kind,
            actor,
            sequence,
            tick,
            space,
            position,
        };
        cue.validate()?;
        Ok(cue)
    }
}
