//! Fixed causal audio event. No item state, RNG, or private actor fields.
use core_types::{SpaceId, Tick};
mod block;
pub use block::{BlockActor, BlockMaterial};
pub const BYTES: usize = 64;
pub const MAX_BYTES: usize = 80;
/// Stack-owned encoding; legacy events do not gain an extra allocation.
#[derive(Clone, Copy, Debug)]
pub struct EncodedCue {
    bytes: [u8; MAX_BYTES],
    len: u8,
}
impl std::ops::Deref for EncodedCue {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}
impl std::ops::DerefMut for EncodedCue {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[..usize::from(self.len)]
    }
}
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
    BlockBreak(BlockActor),
    BlockPlace(BlockActor),
}
impl Kind {
    pub const fn tag(self) -> u8 {
        match self {
            Self::ChickenEgg => 0,
            Self::ChestOpen => 1,
            Self::ChestClose => 2,
            Self::ItemPickup => 3,
            Self::AttackWeak => 4,
            Self::AttackStrong => 5,
            Self::AttackCrit => 6,
            Self::AttackKnockback => 7,
            Self::BucketFill => 8,
            Self::BucketEmpty => 9,
            Self::Eat => 10,
            Self::EatDone => 11,
            Self::BlockBreak(_) => 12,
            Self::BlockPlace(_) => 13,
        }
    }
    pub const fn block(self) -> Option<BlockActor> {
        match self {
            Self::BlockBreak(actor) | Self::BlockPlace(actor) => Some(actor),
            _ => None,
        }
    }
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
        if let Some(block) = self.kind.block()
            && (self.actor != crate::player_entity_id(block.player)
                || self.position.iter().any(|p| p - p.floor() != 0.5))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn encode(self) -> Result<EncodedCue, Error> {
        self.validate()?;
        let block = self.kind.block();
        let mut bytes = EncodedCue {
            bytes: [0; MAX_BYTES],
            len: if block.is_some() {
                MAX_BYTES as u8
            } else {
                BYTES as u8
            },
        };
        bytes[..4].copy_from_slice(if block.is_some() { b"ACU2" } else { b"ACU1" });
        bytes[4] = self.kind.tag();
        if let Some(block) = block {
            bytes[5] = block.material as u8;
            bytes[BYTES..MAX_BYTES].copy_from_slice(&block.player.0);
        }
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
        let legacy = bytes.len() == BYTES && bytes.get(..4) == Some(b"ACU1".as_slice());
        let block = bytes.len() == MAX_BYTES && bytes.get(..4) == Some(b"ACU2".as_slice());
        if (!legacy && !block)
            || bytes[6..8] != [0; 2]
            || (legacy && bytes[5] != 0)
            || bytes[36..40] != [0; 4]
        {
            return Err(Error::Invalid);
        }
        let block_actor = || -> Result<BlockActor, Error> {
            if !block {
                return Err(Error::Invalid);
            }
            Ok(BlockActor {
                player: crate::PlayerId(
                    bytes[BYTES..MAX_BYTES]
                        .try_into()
                        .map_err(|_| Error::Invalid)?,
                ),
                material: BlockMaterial::decode(bytes[5]).ok_or(Error::Invalid)?,
            })
        };
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
            12 => Kind::BlockBreak(block_actor()?),
            13 => Kind::BlockPlace(block_actor()?),
            _ => return Err(Error::Invalid),
        };
        if block != kind.block().is_some() {
            return Err(Error::Invalid);
        }
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
