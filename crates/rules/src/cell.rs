//! Raw storage helpers for the copied pure donor formulas. New world APIs use
//! `core_types::Cell`; conversion at that boundary is explicit and lossless.

pub type BlockId = u16;
pub type State = u16;
pub type Cell = u32;
pub const DIRS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

pub const fn cell_of(id: BlockId, state: State) -> Cell {
    id as u32 | (state as u32) << 16
}

pub const fn block_of(cell: Cell) -> BlockId {
    cell as u16
}

pub const fn state_of(cell: Cell) -> State {
    (cell >> 16) as u16
}
