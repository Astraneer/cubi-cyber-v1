//! Pure bounded farming transitions. Light is sampled from the actual lighting
//! channel; unknown neighbors defer the entire operation instead of becoming air.
use super::{CellChange, Inventory, InventoryPlan, SurvivalError};
use crate::{
    blocks::{HOE, bdef},
    ids::*,
    items::{ItemKind, idef},
    stack::ItemStack,
};
use core_types::Cell;

pub fn is_crop(id: u16) -> bool {
    matches!(id, WHEAT | CARROTS | POTATOES)
}
pub fn growth_denominator(moisture: f32) -> u32 {
    (25.0 / moisture.max(0.5)) as u32 + 1
}

fn at(
    query: &mut impl FnMut([i32; 3]) -> Option<Cell>,
    p: [i32; 3],
    offset: [i32; 3],
    reads: &mut u16,
) -> Result<Cell, SurvivalError> {
    *reads += 1;
    let mut q = p;
    for axis in 0..3 {
        q[axis] = q[axis]
            .checked_add(offset[axis])
            .ok_or(SurvivalError::UnknownInput)?;
    }
    query(q).ok_or(SurvivalError::UnknownInput)
}
pub fn moisture_factor(
    mut query: impl FnMut([i32; 3]) -> Option<Cell>,
    position: [i32; 3],
    crop: u16,
) -> Result<(f32, u16), SurvivalError> {
    let mut reads = 0;
    let mut moisture = 1.0f32;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let cell = at(&mut query, position, [dx, -1, dz], &mut reads)?;
            let mut contribution = if cell.id() != FARMLAND {
                0.0
            } else if cell.state() & 7 > 0 {
                3.0
            } else {
                1.0
            };
            if dx != 0 || dz != 0 {
                contribution /= 4.0;
            }
            moisture += contribution;
        }
    }
    let mut same = [false; 8];
    for (slot, offset) in same.iter_mut().zip([
        [-1, 0, 0],
        [1, 0, 0],
        [0, 0, -1],
        [0, 0, 1],
        [-1, 0, -1],
        [1, 0, -1],
        [1, 0, 1],
        [-1, 0, 1],
    ]) {
        *slot = at(&mut query, position, offset, &mut reads)?.id() == crop;
    }
    if ((same[0] || same[1]) && (same[2] || same[3])) || same[4..].iter().any(|&value| value) {
        moisture /= 2.0;
    }
    Ok((moisture, reads))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarmStep {
    pub change: Option<CellChange>,
    pub reads: u16,
}

/// `roll` is a keyed unbiased integer in 0..growth_denominator; no scan occurs
/// for mature crops. The scheduler chooses actual random-tick events, not pages.
pub fn random_tick(
    mut query: impl FnMut([i32; 3]) -> Option<Cell>,
    position: [i32; 3],
    light: u8,
    roll: u32,
) -> Result<FarmStep, SurvivalError> {
    let mut reads = 0;
    let before = at(&mut query, position, [0; 3], &mut reads)?;
    let mut after = before;
    if before.id() == FARMLAND {
        let mut wet = false;
        'search: for dy in 0..=1 {
            for dz in -4..=4 {
                for dx in -4..=4 {
                    if bdef(at(&mut query, position, [dx, dy, dz], &mut reads)?.id()).fluid
                        == crate::blocks::FLUID_WATER
                    {
                        wet = true;
                        break 'search;
                    }
                }
            }
        }
        let moisture = before.state() & 7;
        if wet {
            after = Cell::from_parts(FARMLAND, 7);
        } else if moisture > 0 {
            after = Cell::from_parts(FARMLAND, moisture - 1);
        } else if !is_crop(at(&mut query, position, [0, 1, 0], &mut reads)?.id()) {
            after = Cell::from_parts(DIRT, 0);
        }
    } else if is_crop(before.id()) && before.state() & 7 < 7 && light >= 9 {
        let (moisture, more_reads) = moisture_factor(&mut query, position, before.id())?;
        reads += more_reads;
        let denominator = growth_denominator(moisture);
        if roll >= denominator {
            return Err(SurvivalError::UnknownInput);
        }
        if roll == 0 {
            after = Cell::from_parts(before.id(), (before.state() & 7) + 1);
        }
    }
    Ok(FarmStep {
        change: (before != after).then_some(CellChange {
            position,
            before,
            after,
        }),
        reads,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Commit the cell change and inventory result atomically"]
pub struct FarmUse {
    pub cell: CellChange,
    pub inventory: InventoryPlan,
}

pub fn till(
    inventory: &Inventory,
    expected: ItemStack,
    position: [i32; 3],
    before: Cell,
    above: Option<Cell>,
    bottom_face: bool,
) -> Result<FarmUse, SurvivalError> {
    if inventory.hand() != expected
        || !matches!(idef(expected.item()).kind,ItemKind::Tool {mask,..} if mask & HOE != 0)
        || !(bdef(before.id()).base == GRASS || before.id() == DIRT)
        || bottom_face
        || above.ok_or(SurvivalError::UnknownInput)?.id() != AIR
    {
        return Err(SurvivalError::NoEntitlement);
    }
    Ok(FarmUse {
        cell: CellChange {
            position,
            before,
            after: Cell::from_parts(FARMLAND, 0),
        },
        inventory: inventory.plan_wear_tool(expected, 1)?,
    })
}
pub fn plant(
    inventory: &Inventory,
    expected: ItemStack,
    position: [i32; 3],
    before: Cell,
    below: Option<Cell>,
    light: u8,
) -> Result<FarmUse, SurvivalError> {
    if inventory.hand() != expected
        || before.id() != AIR
        || below.ok_or(SurvivalError::UnknownInput)?.id() != FARMLAND
        || light < 8
    {
        return Err(SurvivalError::NoEntitlement);
    }
    let ItemKind::Seed { crop, .. } = idef(expected.item()).kind else {
        return Err(SurvivalError::NoEntitlement);
    };
    Ok(FarmUse {
        cell: CellChange {
            position,
            before,
            after: Cell::from_parts(crop, 0),
        },
        inventory: inventory.plan_consume(inventory.selected(), expected, 1)?,
    })
}
pub fn trample(
    position: [i32; 3],
    before: Cell,
    fall_distance: f32,
    large_enough: bool,
    unit_roll: f32,
) -> Option<CellChange> {
    (before.id() == FARMLAND
        && large_enough
        && unit_roll.is_finite()
        && (0.0..1.0).contains(&unit_roll)
        && unit_roll < fall_distance - 0.5)
        .then_some(CellChange {
            position,
            before,
            after: Cell::from_parts(DIRT, 0),
        })
}
