//! Bounded table evaluation; the owner supplies a keyed RNG for this event.
//! Drops are values which must become durable item entities with the world edit.

use super::{Inventory, InventoryPlan, SurvivalError};
use crate::{
    blocks::{Loot, bdef},
    ids::*,
    items::{ItemKind, block_item, can_harvest, idef, *},
    stack::ItemStack,
};
use core_types::{Cell, DeterministicRng};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drops {
    stacks: [ItemStack; 4],
    len: u8,
}
impl Default for Drops {
    fn default() -> Self {
        Self {
            stacks: [ItemStack::EMPTY; 4],
            len: 0,
        }
    }
}
impl Drops {
    pub fn stacks(&self) -> &[ItemStack] {
        &self.stacks[..usize::from(self.len)]
    }
    fn push(&mut self, item: u16, count: u16) -> Result<(), SurvivalError> {
        if count == 0 || item == 0 {
            return Ok(());
        }
        if self.len as usize == self.stacks.len() {
            return Err(SurvivalError::Capacity);
        }
        self.stacks[self.len as usize] =
            ItemStack::new(item, count).map_err(|_| SurvivalError::InvalidStack)?;
        self.len += 1;
        Ok(())
    }
}
pub fn unit(rng: &mut DeterministicRng) -> f32 {
    ((rng.next_u64() >> 40) as u32) as f32 / 16_777_216.0
}

pub fn block_drops(
    cell: Cell,
    by_player: bool,
    shears: bool,
    rng: &mut DeterministicRng,
) -> Result<Drops, SurvivalError> {
    let definition = crate::block(cell.id()).ok_or(SurvivalError::UnknownInput)?;
    let mut drops = Drops::default();
    if shears && definition.leaves {
        drops.push(block_item(cell.id()), 1)?;
        return Ok(drops);
    }
    match definition.loot {
        Loot::None => {}
        Loot::Itself => drops.push(block_item(cell.id()), 1)?,
        Loot::SnowLayers if by_player => drops.push(I_SNOWBALL, (cell.state() & 7) + 1)?,
        Loot::SnowLayers => {}
        Loot::Crop => {
            let ripe = cell.state() & 7 == 7;
            let bonus = if ripe {
                1 + (0..3).filter(|_| unit(rng) < 0.571_428_6).count() as u16
            } else {
                0
            };
            match cell.id() {
                WHEAT => {
                    drops.push(if ripe { I_WHEAT } else { I_WHEAT_SEEDS }, 1)?;
                    if ripe {
                        drops.push(I_WHEAT_SEEDS, bonus)?;
                    }
                }
                CARROTS | POTATOES => {
                    let item = if cell.id() == CARROTS {
                        I_CARROT
                    } else {
                        I_POTATO
                    };
                    drops.push(item, 1)?;
                    if ripe {
                        drops.push(item, bonus)?;
                    }
                }
                _ => return Err(SurvivalError::UnknownInput),
            }
        }
        Loot::Table(rows) => {
            let mut done = 0u32;
            for row in rows {
                if row.alt >= 32 {
                    return Err(SurvivalError::UnknownInput);
                }
                if row.alt != 0 && done & (1 << row.alt) != 0 {
                    continue;
                }
                if row.chance >= 1.0 || unit(rng) < row.chance {
                    let count = if row.min == row.max {
                        u16::from(row.min)
                    } else {
                        u16::from(row.min)
                            + rng
                                .below(u64::from(row.max - row.min) + 1)
                                .expect("nonempty range") as u16
                    };
                    if row.alt != 0 {
                        done |= 1 << row.alt;
                    }
                    drops.push(row.item, count)?;
                }
            }
        }
    }
    Ok(drops)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Commit inventory wear, world cell removal and every drop together"]
pub struct MinePlan {
    pub inventory: InventoryPlan,
    pub drops: Drops,
}
impl Inventory {
    pub fn plan_mine(
        &self,
        expected: ItemStack,
        cell: Cell,
        rng: &mut DeterministicRng,
    ) -> Result<MinePlan, SurvivalError> {
        if self.hand() != expected {
            return Err(SurvivalError::NoEntitlement);
        }
        let block = crate::block(cell.id()).ok_or(SurvivalError::UnknownInput)?;
        if cell.id() == AIR || block.hardness < 0.0 {
            return Err(SurvivalError::NoEntitlement);
        }
        let wear = if block.hardness == 0.0 {
            0
        } else {
            match idef(expected.item()).kind {
                ItemKind::Tool { .. } | ItemKind::Shears => 1,
                ItemKind::Sword { .. } => 2,
                _ => 0,
            }
        };
        let inventory = if wear > 0 {
            self.plan_wear_tool(expected, wear)?
        } else {
            self.plan_pickup(ItemStack::EMPTY)?
        };
        let drops = if can_harvest(expected.item(), bdef(cell.id())) {
            block_drops(
                cell,
                true,
                matches!(idef(expected.item()).kind, ItemKind::Shears),
                rng,
            )?
        } else {
            Drops::default()
        };
        Ok(MinePlan { inventory, drops })
    }
}
