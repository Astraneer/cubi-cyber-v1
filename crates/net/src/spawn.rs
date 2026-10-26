//! Public, immutable landmark advertised by the authority for this session.
//! The center is the generated square's floor, not the player's login position.
use core_types::{BlockPos, SpaceId};

pub const LAYOUT_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Landmark {
    pub session: u64,
    pub center: BlockPos,
}

impl Landmark {
    pub fn validate(self) -> Result<(), crate::CodecError> {
        // Leave room for the fixed pavilion offsets without overflowing world
        // coordinates. A landmark never describes a private/moving space.
        if self.center.space != SpaceId(0)
            || [self.center.x, self.center.y, self.center.z]
                .into_iter()
                .any(|v| v.unsigned_abs() >= (crate::MAX_COORDINATE - 64) as u32)
        {
            return Err(crate::CodecError::InvalidBounds);
        }
        Ok(())
    }
}
