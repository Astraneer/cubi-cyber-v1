//! Bounded generator scratch tile; not persistent world storage.
use super::cell::{BlockId, CS, H, SEA};
use rules::{
    blocks::{is_leaves, is_plant},
    ids::{AIR, WATER},
};
pub(crate) const PAD: i32 = 1;
pub(crate) const W: i32 = CS + 2 * PAD;
pub(crate) struct Region {
    pub(crate) x0: i32,
    pub(crate) z0: i32,
    pub(crate) blocks: crate::DraftBlocks,
    pub(crate) tint: Vec<u16>,
}
impl Region {
    pub(crate) fn idx(x: i32, y: i32, z: i32) -> usize {
        ((z * W + x) * H + y) as usize
    }
    pub(crate) fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        if !(0..H).contains(&y) || !(0..W).contains(&x) || !(0..W).contains(&z) {
            return AIR;
        }
        self.blocks[Self::idx(x, y, z)] as BlockId
    }
    pub(crate) fn set_block_at(&mut self, i: usize, block: BlockId) {
        assert!(
            block < 256,
            "profile4 generator block exceeds low-byte scratch format"
        );
        self.blocks[i] = block as u8;
    }
    pub(crate) fn put(&mut self, x: i32, y: i32, z: i32, block: BlockId, mode: u8) {
        let (x, z) = (x - self.x0, z - self.z0);
        if !(1..H).contains(&y) || !(0..W).contains(&x) || !(0..W).contains(&z) {
            return;
        }
        let i = Self::idx(x, y, z);
        let current = self.blocks[i] as BlockId;
        if current == AIR
            || current == WATER && y > SEA
            || is_plant(current)
            || mode == 1 && is_leaves(current)
        {
            self.set_block_at(i, block);
        }
    }
    pub(crate) fn touches(&self, x: i32, z: i32, radius: i32) -> bool {
        x + radius >= self.x0
            && x - radius < self.x0 + W
            && z + radius >= self.z0
            && z - radius < self.z0 + W
    }
    pub(crate) fn span(&self, x: i32, z: i32, lo: i32, hi: i32) -> (i32, i32, i32, i32) {
        (
            (self.x0 - x).max(lo),
            (self.x0 + W - 1 - x).min(hi),
            (self.z0 - z).max(lo),
            (self.z0 + W - 1 - z).min(hi),
        )
    }
    pub(crate) fn index_of(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (x, z) = (x - self.x0, z - self.z0);
        ((0..H).contains(&y) && (0..W).contains(&x) && (0..W).contains(&z))
            .then(|| Self::idx(x, y, z))
    }
}
