//! Область куска с рамкой в 1 блок (для граней и теней у края): её заполняет генератор, на неё накладываются правки
//! (воркер, синхронная сборка, кэш главного потока — world), по ней строится меш. Ячейка (cell.rs) — u32 из трёх
//! параллельных массивов одним индексом: младший байт номера блока (blocks, всегда), старший байт номера (blocks_hi —
//! заводится, только если в области есть блок с номером ≥ 256) и состояние u16 (states — только если правки принесли
//! ненулевое состояние; генератор даёт состояние 0). Так номер блока — полные u16, а память и скорость генератора и
//! меша — как у прежней ячейки u16 (область ~42 КБ, а не 83): блоков с номером ≥ 256 пока нет (registry BLOCK_COUNT).

use crate::cell::{block_of, cell_of, state_of, BlockId, Cell, State, CS, H, SEA};
use crate::registry::blocks::{is_leaves, is_plant};
use crate::registry::ids::{AIR, WATER};

/// Рамка области в блоках.
pub(crate) const PAD: i32 = 1;
/// Сторона области: кусок и две рамки.
pub(crate) const W: i32 = CS + 2 * PAD;

pub(crate) struct Region {
    pub(crate) x0: i32,
    pub(crate) z0: i32,
    /// Младший байт номера блока, [(z * W + x) * H + y]. Генератор пишет сюда напрямую (его блоки — меньше 256).
    pub(crate) blocks: Vec<u8>,
    /// Старший байт номера блока тем же индексом — только если в области есть блок с номером ≥ 256.
    pub(crate) blocks_hi: Option<Vec<u8>>,
    /// [z * W + x] — оттенок rgb565 травы/листвы/воды столбца (0xFFFF — без оттенка).
    pub(crate) tint: Vec<u16>,
    /// Состояния тем же индексом — только если правки принесли ненулевое состояние.
    pub(crate) states: Option<Vec<State>>,
}

impl Region {
    #[inline]
    pub(crate) fn idx(lx: i32, y: i32, lz: i32) -> usize {
        ((lz * W + lx) * H + y) as usize
    }
    /// Номер блока в клетке области (вне области — воздух).
    #[inline]
    pub(crate) fn get(&self, lx: i32, y: i32, lz: i32) -> BlockId {
        if y < 0 || y >= H || lx < 0 || lz < 0 || lx >= W || lz >= W {
            return AIR;
        }
        self.block_at(Self::idx(lx, y, lz))
    }
    /// Номер блока по индексу.
    #[inline]
    pub(crate) fn block_at(&self, i: usize) -> BlockId {
        let lo = self.blocks[i] as BlockId;
        match &self.blocks_hi {
            None => lo,
            Some(h) => lo | (h[i] as BlockId) << 8,
        }
    }
    /// Записать номер блока по индексу (старший байт заводится при первом номере ≥ 256).
    #[inline]
    pub(crate) fn set_block_at(&mut self, i: usize, b: BlockId) {
        self.blocks[i] = b as u8;
        let hi = (b >> 8) as u8;
        if hi != 0 && self.blocks_hi.is_none() {
            self.blocks_hi = Some(vec![0; self.blocks.len()]);
        }
        if let Some(h) = self.blocks_hi.as_mut() {
            h[i] = hi;
        }
    }
    /// Состояние клетки (0 — нет правок с состоянием или вне области).
    #[inline]
    pub(crate) fn state(&self, lx: i32, y: i32, lz: i32) -> State {
        match &self.states {
            Some(s) if y >= 0 && y < H && lx >= 0 && lz >= 0 && lx < W && lz < W => s[Self::idx(lx, y, lz)],
            _ => 0,
        }
    }
    /// Записать в мировых координатах; mode 0 — только в воздух/растение, 1 — в воздух, растение или листву.
    pub(crate) fn put(&mut self, x: i32, y: i32, z: i32, b: BlockId, mode: u8) {
        let (lx, lz) = (x - self.x0, z - self.z0);
        if y < 1 || y >= H || lx < 0 || lz < 0 || lx >= W || lz >= W {
            return;
        }
        let i = Self::idx(lx, y, lz);
        let cur = self.block_at(i);
        let ok = cur == AIR || cur == WATER && y > SEA || is_plant(cur) || (mode == 1 && is_leaves(cur));
        if ok {
            self.set_block_at(i, b);
        }
    }
    /// Задевает ли квадрат (x ± r, z ± r) область (дерево целиком снаружи — не строить).
    #[inline]
    pub(crate) fn touches(&self, x: i32, z: i32, r: i32) -> bool {
        x + r >= self.x0 && x - r < self.x0 + W && z + r >= self.z0 && z - r < self.z0 + W
    }
    /// Пределы смещений dx, dz (lo…hi) вокруг (x, z), обрезанные областью: (dx от, dx до, dz от, dz до).
    #[inline]
    pub(crate) fn span(&self, x: i32, z: i32, lo: i32, hi: i32) -> (i32, i32, i32, i32) {
        ((self.x0 - x).max(lo), (self.x0 + W - 1 - x).min(hi), (self.z0 - z).max(lo), (self.z0 + W - 1 - z).min(hi))
    }

    /// Индекс ячейки в мировых координатах или None — вне области.
    #[inline]
    pub(crate) fn index_of(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (lx, lz) = (x - self.x0, z - self.z0);
        if y >= 0 && y < H && lx >= 0 && lz >= 0 && lx < W && lz < W {
            Some(Self::idx(lx, y, lz))
        } else {
            None
        }
    }
    /// Ячейка по индексу: номер блока | состояние.
    #[inline]
    pub(crate) fn cell(&self, i: usize) -> Cell {
        cell_of(self.block_at(i), self.states.as_ref().map_or(0, |s| s[i]))
    }
    /// Записать ячейку по индексу (массив состояний заводится при первом ненулевом состоянии).
    pub(crate) fn put_cell(&mut self, i: usize, c: Cell) {
        self.set_block_at(i, block_of(c));
        let s = state_of(c);
        if s != 0 && self.states.is_none() {
            self.states = Some(vec![0; self.blocks.len()]);
        }
        if let Some(st) = self.states.as_mut() {
            st[i] = s;
        }
    }
    /// Записать ячейку в мировых координатах (вне области — ничего).
    pub(crate) fn set_cell(&mut self, x: i32, y: i32, z: i32, c: Cell) {
        if let Some(i) = self.index_of(x, y, z) {
            self.put_cell(i, c);
        }
    }
    /// Наложить пачку правок [x, y, z, ячейка, …] (ячейка — u32 в i32: биты как есть) — сборка плитки: воркер,
    /// синхронно, кэш главного потока.
    pub(crate) fn apply_edits(&mut self, edits: &[i32]) {
        for e in edits.chunks_exact(4) {
            if let Some(i) = self.index_of(e[0], e[1], e[2]) {
                self.put_cell(i, e[3] as Cell);
            }
        }
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::gen::{generate, Gen};

    #[test]
    fn region_cells_u32() {
        // Генератор: только младшие байты номеров (блоки меньше 256), состояний нет.
        let mut r = generate(&Gen::new(1337), 0, 0);
        assert!(r.blocks_hi.is_none() && r.states.is_none());
        let i = Region::idx(5, 70, 5);
        let next = r.cell(Region::idx(6, 70, 5));
        // Номер блока ≥ 256 и состояние больше байта — целиком; соседняя ячейка не тронута.
        r.put_cell(i, cell_of(300, 0x1FF));
        assert!(r.blocks_hi.is_some() && r.states.is_some());
        assert_eq!((r.cell(i), r.get(5, 70, 5), r.state(5, 70, 5)), (cell_of(300, 0x1FF), 300, 0x1FF));
        assert_eq!(r.cell(Region::idx(6, 70, 5)), next);
        // Пачка правок воркера (ячейка u32 в i32) — та же ячейка, и обратно в номер 255.
        let c = cell_of(0x8001, 0xFFFF);
        let (x, z) = (r.x0 + 7, r.z0 + 7);
        r.apply_edits(&[x, 80, z, c as i32, r.x0 + 5, 70, r.z0 + 5, 255]);
        assert_eq!((r.cell(Region::idx(7, 80, 7)), r.cell(i)), (c, 255));
    }
}
