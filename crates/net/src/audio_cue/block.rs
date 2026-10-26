//! Sound families only: no private block IDs, states, contents or source counters.
use crate::PlayerId;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockMaterial {
    Stone = 1,
    Wood = 2,
    Gravel = 3,
    Grass = 4,
    Sand = 5,
    Snow = 6,
    Wool = 7,
    Glass = 8,
    Metal = 9,
    Ladder = 10,
}
impl BlockMaterial {
    pub const fn decode(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Stone,
            2 => Self::Wood,
            3 => Self::Gravel,
            4 => Self::Grass,
            5 => Self::Sand,
            6 => Self::Snow,
            7 => Self::Wool,
            8 => Self::Glass,
            9 => Self::Metal,
            10 => Self::Ladder,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockActor {
    /// Exact public identity. The legacy u64 actor is only an emitter/target key.
    pub player: PlayerId,
    pub material: BlockMaterial,
}
