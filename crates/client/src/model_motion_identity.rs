//! Exact temporal model namespaces. This does not change the64B GPU ABI or
//! treat the legacy Human display hash as an object identity.
use core_types::BlockPos;
use net::PlayerId;
use render::presented_motion::{Error, ObjectKey};
pub const LOCAL: u16 = 1;
pub const MOB: u16 = 2;
pub const ITEM: u16 = 3;
pub const HUMAN: u16 = 4;
pub const PACKET: u16 = 5;
pub const PROP: u16 = 6;
pub fn human(space: u32, id: PlayerId) -> Result<ObjectKey, Error> {
    ObjectKey::named(
        HUMAN,
        space,
        [
            u64::from_le_bytes(id.0[..8].try_into().expect("UUIDlow64")),
            u64::from_le_bytes(id.0[8..].try_into().expect("UUIDhigh64")),
        ],
    )
}
pub fn prop(position: BlockPos) -> Result<ObjectKey, Error> {
    ObjectKey::named(
        PROP,
        position.space.0,
        [
            (u64::from(position.x as u32) << 32) | u64::from(position.z as u32),
            u64::from(position.y as u32),
        ],
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_uuid_coordinates_space_and_actor_families_are_independent() {
        let a = PlayerId([7; 16]);
        let mut b = a;
        b.0[15] = 8;
        assert_ne!(human(0, a).unwrap(), human(0, b).unwrap());
        assert_ne!(human(0, a).unwrap(), human(1, a).unwrap());
        let p = BlockPos {
            space: core_types::SpaceId(0),
            x: i32::MIN,
            y: -1,
            z: i32::MAX,
        };
        assert_ne!(prop(p).unwrap(), prop(BlockPos { y: 0, ..p }).unwrap());
        assert_ne!(prop(p).unwrap(), prop(BlockPos { x: 0, ..p }).unwrap());
        assert_ne!(
            ObjectKey::native(MOB, 0, u64::MAX).unwrap(),
            ObjectKey::native(ITEM, 0, u64::MAX).unwrap()
        );
        assert_ne!(
            ObjectKey::native(ITEM, 0, 0).unwrap(),
            ObjectKey::native(PACKET, 0, 0).unwrap()
        );
    }
}
