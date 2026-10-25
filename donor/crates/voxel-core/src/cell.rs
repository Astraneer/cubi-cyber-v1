//! Размеры мира и ячейка (решение главного агента для Ф1): ячейка — u32 = номер блока (младшие 16 бит) | состояние
//! (старшие 16 бит). Номера блоков прежние (0–55, новые — только в конец), раскладка состояния прежняя — в младших
//! 8 битах из 16 (registry/blocks.rs); старшие 8 бит состояния свободны (кабели, ярусы машин, waterlogged).
//! Генератор даёт состояние 0. В меше и вершине формат пока прежний (Ф3) — меш только читает ячейку.

/// Сторона куска.
pub(crate) const CS: i32 = 16;
/// Высота мира.
pub(crate) const H: i32 = 128;
/// Верхний блок под водой.
pub(crate) const SEA: i32 = 48;

/// Номер блока.
pub(crate) type BlockId = u16;
/// Состояние блока (ось, сторона, горение, слои…; занято младшие 8 бит).
pub(crate) type State = u16;
/// Ячейка мира: номер блока | состояние << STATE_SHIFT.
pub(crate) type Cell = u32;

/// Сдвиг состояния в ячейке.
pub(crate) const STATE_SHIFT: u32 = 16;

/// Ячейка из номера блока и состояния.
#[inline]
pub(crate) const fn cell_of(b: BlockId, s: State) -> Cell {
    b as Cell | (s as Cell) << STATE_SHIFT
}
/// Номер блока ячейки.
#[inline]
pub(crate) const fn block_of(c: Cell) -> BlockId {
    c as BlockId
}
/// Состояние ячейки.
#[inline]
pub(crate) const fn state_of(c: Cell) -> State {
    (c >> STATE_SHIFT) as State
}

// Направления в порядке Minecraft (Direction): вниз, вверх, север (−Z), юг (+Z), запад (−X), восток (+X).
pub(crate) const F_DOWN: u8 = 0;
pub(crate) const F_UP: u8 = 1;
pub(crate) const F_NORTH: u8 = 2;
pub(crate) const F_SOUTH: u8 = 3;
pub(crate) const F_WEST: u8 = 4;
pub(crate) const F_EAST: u8 = 5;
pub(crate) const DIRS: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];

/// Соседняя клетка в сторону f.
#[inline]
pub(crate) fn offset(p: [i32; 3], f: u8) -> [i32; 3] {
    let d = DIRS[f as usize];
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

#[cfg(test)]
mod cell_tests {
    use super::*;

    #[test]
    fn cell_layout_u32() {
        // Номер блока — младшие 16 бит, состояние — старшие 16; старые номера и байт состояния целы.
        let c = cell_of(44, 2 | 8);
        assert_eq!((block_of(c), state_of(c)), (44, 10));
        assert_eq!(c, 44 | 10 << 16);
        assert_eq!(cell_of(0xFFFF, 0xFFFF), u32::MAX);
        assert_eq!((block_of(u32::MAX), state_of(u32::MAX)), (0xFFFF, 0xFFFF));
        // Номер блока больше 255 и состояние больше байта не задевают друг друга.
        let c = cell_of(300, 0x1FF);
        assert_eq!((block_of(c), state_of(c)), (300, 0x1FF));
        assert_eq!(cell_of(9, 0), 9, "состояние 0 — ячейка равна номеру блока");
        // Пачка правок для воркеров — i32: ячейка переживает перевод туда и обратно.
        let c = cell_of(53, 0x8005);
        assert_eq!((c as i32) as u32, c);
    }
}
