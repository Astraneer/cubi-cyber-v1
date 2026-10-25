//! Page-local adaptive cell storage. Only a 16³ page can be repacked.
use core_types::{Cell, CellProperties, LocalIdx, PAGE_VOLUME};
use std::collections::BTreeMap;

pub const DENSE_ENTER_UNIQUE: usize = 256;
pub const DENSE_LEAVE_UNIQUE: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMode {
    Uniform,
    Paletted,
    Dense,
}

#[derive(Clone, Debug)]
pub enum PageStorage {
    Uniform(Cell),
    Paletted {
        palette: Vec<Cell>,
        counts: Vec<u16>,
        bits: u8,
        indices: Vec<u64>,
    },
    Dense {
        cells: Box<[Cell; PAGE_VOLUME]>,
        counts: BTreeMap<Cell, u16>,
    },
}

fn bits_for(values: usize) -> u8 {
    (usize::BITS - (values.max(2) - 1).leading_zeros()) as u8
}
fn packed_len(bits: u8) -> usize {
    (PAGE_VOLUME * usize::from(bits)).div_ceil(64)
}
fn packed_read(words: &[u64], bits: u8, index: usize) -> usize {
    let bit = index * usize::from(bits);
    let offset = bit % 64;
    let mut value = words[bit / 64] >> offset;
    if offset + usize::from(bits) > 64 {
        value |= words[bit / 64 + 1] << (64 - offset);
    }
    (value & ((1_u64 << bits) - 1)) as usize
}
fn packed_write(words: &mut [u64], bits: u8, index: usize, value: usize) {
    let bit = index * usize::from(bits);
    let offset = bit % 64;
    let mask = (1_u64 << bits) - 1;
    words[bit / 64] = (words[bit / 64] & !(mask << offset)) | ((value as u64 & mask) << offset);
    if offset + usize::from(bits) > 64 {
        let high = usize::from(bits) - (64 - offset);
        let high_mask = (1_u64 << high) - 1;
        words[bit / 64 + 1] =
            (words[bit / 64 + 1] & !high_mask) | ((value as u64) >> (64 - offset));
    }
}

impl PageStorage {
    pub fn from_cells(cells: Box<[Cell; PAGE_VOLUME]>) -> Self {
        let mut histogram = BTreeMap::<Cell, u16>::new();
        for value in cells.iter() {
            *histogram.entry(*value).or_default() += 1;
        }
        if histogram.len() == 1 {
            return Self::Uniform(cells[0]);
        }
        if histogram.len() > DENSE_ENTER_UNIQUE {
            return Self::Dense {
                cells,
                counts: histogram,
            };
        }
        let palette: Vec<Cell> = histogram.keys().copied().collect();
        let counts = histogram.values().copied().collect();
        let bits = bits_for(palette.len());
        let mut indices = vec![0; packed_len(bits)];
        for (index, value) in cells.iter().enumerate() {
            packed_write(
                &mut indices,
                bits,
                index,
                palette.binary_search(value).expect("histogram entry"),
            );
        }
        Self::Paletted {
            palette,
            counts,
            bits,
            indices,
        }
    }
    pub const fn mode(&self) -> StorageMode {
        match self {
            Self::Uniform(_) => StorageMode::Uniform,
            Self::Paletted { .. } => StorageMode::Paletted,
            Self::Dense { .. } => StorageMode::Dense,
        }
    }
    pub fn get(&self, index: LocalIdx) -> Cell {
        let index = usize::from(index.get());
        match self {
            Self::Uniform(value) => *value,
            Self::Paletted {
                palette,
                bits,
                indices,
                ..
            } => palette[packed_read(indices, *bits, index)],
            Self::Dense { cells, .. } => cells[index],
        }
    }
    pub fn to_cells(&self) -> Box<[Cell; PAGE_VOLUME]> {
        let mut result = Box::new([Cell::AIR; PAGE_VOLUME]);
        for (index, value) in result.iter_mut().enumerate() {
            *value = self.get(LocalIdx::new(index as u16).expect("page range"));
        }
        result
    }
    /// Updates one packed index. Full scans occur only when representation changes.
    pub fn set(&mut self, index: LocalIdx, value: Cell) -> Cell {
        let old = self.get(index);
        if old == value {
            return old;
        }
        let i = usize::from(index.get());
        match self {
            Self::Uniform(previous) => {
                let old_value = *previous;
                let mut indices = vec![0; packed_len(1)];
                packed_write(&mut indices, 1, i, 1);
                *self = Self::Paletted {
                    palette: vec![old_value, value],
                    counts: vec![(PAGE_VOLUME - 1) as u16, 1],
                    bits: 1,
                    indices,
                };
            }
            Self::Paletted {
                palette,
                counts,
                bits,
                indices,
            } => {
                let old_slot = packed_read(indices, *bits, i);
                counts[old_slot] -= 1;
                let existing = palette
                    .iter()
                    .zip(counts.iter())
                    .position(|(cell, count)| *cell == value && *count != 0);
                let slot = existing.or_else(|| counts.iter().position(|count| *count == 0));
                let new_slot = if let Some(slot) = slot {
                    if counts[slot] == 0 {
                        palette[slot] = value;
                    }
                    slot
                } else if palette.len() < DENSE_ENTER_UNIQUE {
                    let slot = palette.len();
                    palette.push(value);
                    counts.push(0);
                    let new_bits = bits_for(palette.len());
                    if new_bits != *bits {
                        let mut repacked = vec![0; packed_len(new_bits)];
                        for cell in 0..PAGE_VOLUME {
                            packed_write(
                                &mut repacked,
                                new_bits,
                                cell,
                                packed_read(indices, *bits, cell),
                            );
                        }
                        *indices = repacked;
                        *bits = new_bits;
                    }
                    slot
                } else {
                    // Restore the bookkeeping before converting the logical cells.
                    counts[old_slot] += 1;
                    let mut cells = self.to_cells();
                    cells[i] = value;
                    *self = Self::from_cells(cells);
                    return old;
                };
                counts[new_slot] += 1;
                packed_write(indices, *bits, i, new_slot);
                if let Some((cell, _)) = palette
                    .iter()
                    .zip(counts.iter())
                    .find(|(_, count)| usize::from(**count) == PAGE_VOLUME)
                {
                    *self = Self::Uniform(*cell);
                }
            }
            Self::Dense { cells, counts } => {
                cells[i] = value;
                let old_count = counts.get_mut(&old).expect("dense histogram old cell");
                *old_count -= 1;
                if *old_count == 0 {
                    counts.remove(&old);
                }
                *counts.entry(value).or_default() += 1;
                if counts.len() <= DENSE_LEAVE_UNIQUE {
                    *self = Self::from_cells(cells.clone());
                }
            }
        }
        old
    }
    /// Logical heap payload; allocator overhead is intentionally not estimated.
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => 0,
            Self::Paletted {
                palette,
                counts,
                indices,
                ..
            } => palette.len() * 4 + counts.len() * 2 + indices.len() * 8,
            Self::Dense { cells, counts } => cells.len() * 4 + counts.len() * 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellMask([u64; PAGE_VOLUME / 64]);
impl Default for CellMask {
    fn default() -> Self {
        Self([0; PAGE_VOLUME / 64])
    }
}
impl CellMask {
    pub fn get(&self, index: LocalIdx) -> bool {
        self.0[usize::from(index.get()) / 64] & (1 << (index.get() % 64)) != 0
    }
    fn set(&mut self, index: LocalIdx, value: bool) {
        let word = &mut self.0[usize::from(index.get()) / 64];
        let bit = 1 << (index.get() % 64);
        if value {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }
    pub fn count(&self) -> u32 {
        self.0.iter().map(|word| word.count_ones()).sum()
    }
    pub fn words(&self) -> &[u64; PAGE_VOLUME / 64] {
        &self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMasks {
    pub occupancy: CellMask,
    pub opaque: CellMask,
    pub collidable: CellMask,
    pub blocks_projectile: CellMask,
    pub fluid: CellMask,
    pub emitter: CellMask,
}
impl PageMasks {
    pub(crate) fn update(&mut self, index: LocalIdx, properties: CellProperties) {
        self.occupancy.set(index, properties.occupied);
        self.opaque.set(index, properties.opaque);
        self.collidable.set(index, properties.collidable);
        self.blocks_projectile
            .set(index, properties.blocks_projectile);
        self.fluid.set(index, properties.fluid);
        self.emitter.set(index, properties.emitter());
    }
}
