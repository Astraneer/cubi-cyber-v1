//! Pure donor transmitter-state packing; bit 7 remains waterlogged.

pub const fn arms_of(state: u16) -> u16 {
    (state & 0x7f) | ((state >> 1) & 0xf80)
}

pub const fn with_arms(state: u16, arms: u16) -> u16 {
    (state & !0x1f7f) | (arms & 0x7f) | ((arms & 0xf80) << 1)
}

pub const fn arm(arms: u16, face: u8) -> u16 {
    (arms >> (2 * face)) & 3
}

pub fn arm_mask(state: u16) -> u8 {
    let arms = arms_of(state);
    (0..6u8).fold(0, |mask, face| {
        if arm(arms, face) != 0 {
            mask | 1 << face
        } else {
            mask
        }
    })
}

pub fn arms_from_mask(mask: u8) -> u16 {
    (0..6u8).fold(0, |arms, face| {
        if mask >> face & 1 != 0 {
            arms | 1 << (2 * face)
        } else {
            arms
        }
    })
}
