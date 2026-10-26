//! Public, seed-free recipe-board blueprint. Only an authoritative landmark may
//! place this presentation plan; it does not generate or query world cells.
//! Position/content data: approved donor gen/spawn.rs boards/hall_sheets.
use crate::{
    ItemId,
    crafting::{RecipeBook, RecipeKind},
    ids::*,
    items::*,
};
use core_types::{BlockPos, SpaceId};
pub const BOARD_COUNT: usize = 9;
pub const SUBBLOCKS: i64 = 200;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hall {
    Double,
    Steel,
    Wind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pavilion {
    pub hall: Hall,
    /// World X/Z offsets from the authoritative canonical floor center.
    pub offset: [i32; 2],
}
pub const PAVILIONS: [Pavilion; 3] = [
    Pavilion {
        hall: Hall::Double,
        offset: [-19, 6],
    },
    Pavilion {
        hall: Hall::Steel,
        offset: [-19, -6],
    },
    Pavilion {
        hall: Hall::Wind,
        offset: [-19, -16],
    },
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    Show(Hall),
    Craft(Hall),
    Sky(bool),
    SkySign,
}
pub const SHEETS: [Sheet; BOARD_COUNT] = [
    Sheet::Show(Hall::Double),
    Sheet::Craft(Hall::Double),
    Sheet::Show(Hall::Steel),
    Sheet::Craft(Hall::Steel),
    Sheet::Show(Hall::Wind),
    Sheet::Craft(Hall::Wind),
    Sheet::Sky(true),
    Sheet::Sky(false),
    Sheet::SkySign,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    East,
    West,
    South,
}
impl Facing {
    pub const fn normal(self) -> [i32; 3] {
        match self {
            Self::East => [1, 0, 0],
            Self::West => [-1, 0, 0],
            Self::South => [0, 0, 1],
        }
    }
    pub const fn right(self) -> [i32; 3] {
        match self {
            Self::East => [0, 0, -1],
            Self::West => [0, 0, 1],
            Self::South => [1, 0, 0],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardPlan {
    pub sheet: Sheet,
    pub center_200: [i64; 3],
    pub size_200: [u16; 2],
    pub facing: Facing,
    /// Central actual backing voxel, not the image plane or the player's feet.
    pub backing: BlockPos,
    pub backing_size: [u8; 2],
}
impl BoardPlan {
    pub fn relative_center(&self, origin: [i64; 3]) -> [f32; 3] {
        std::array::from_fn(|axis| {
            ((i128::from(self.center_200[axis]) - i128::from(origin[axis]) * i128::from(SUBBLOCKS))
                as f64
                / SUBBLOCKS as f64) as f32
        })
    }
    pub fn backing_cells(&self) -> impl Iterator<Item = BlockPos> + '_ {
        let [w, h] = self.backing_size;
        let right = self.facing.right();
        (0..u32::from(w) * u32::from(h)).map(move |index| {
            let x = (index % u32::from(w)) as i32 - i32::from(w / 2);
            let y = (index / u32::from(w)) as i32 - i32::from(h / 2);
            BlockPos {
                space: self.backing.space,
                x: self.backing.x + right[0] * x,
                y: self.backing.y + y,
                z: self.backing.z + right[2] * x,
            }
        })
    }
    pub fn light_sample(&self) -> BlockPos {
        let n = self.facing.normal();
        BlockPos {
            space: self.backing.space,
            x: self.backing.x + n[0],
            y: self.backing.y,
            z: self.backing.z + n[2],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedSpace,
    CoordinateOverflow,
    MissingRecipe,
    UnsupportedRecipe,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
/// Center.y is the TOP BLOCK of the canonical plaza floor (y0), never feetY.
pub fn boards(center: BlockPos) -> Result<[BoardPlan; BOARD_COUNT], Error> {
    if center.space != SpaceId(0) {
        return Err(Error::UnsupportedSpace);
    }
    if [center.x, center.y, center.z]
        .iter()
        .any(|v| *v < i32::MIN + 64 || *v > i32::MAX - 64)
    {
        return Err(Error::CoordinateOverflow);
    }
    Ok(std::array::from_fn(|index| {
        let (offset, size, facing, backing, backing_size) = if index < 6 {
            let dz = PAVILIONS[index / 2].offset[1];
            let east = index % 2 == 0;
            (
                [if east { -3992 } else { -4208 }, 700, dz * 200 + 100],
                [1000, 600],
                if east { Facing::East } else { Facing::West },
                [-21, 3, dz],
                [5, 3],
            )
        } else {
            match index {
                6 => (
                    [-2300, 500, 808],
                    [175, 175],
                    Facing::South,
                    [-12, 2, 3],
                    [1, 1],
                ),
                7 => (
                    [-1900, 500, 808],
                    [175, 175],
                    Facing::South,
                    [-10, 2, 3],
                    [1, 1],
                ),
                _ => (
                    [-2100, 700, 808],
                    [600, 200],
                    Facing::South,
                    [-11, 3, 3],
                    [3, 1],
                ),
            }
        };
        BoardPlan {
            sheet: SHEETS[index],
            center_200: std::array::from_fn(|axis| {
                i64::from([center.x, center.y, center.z][axis]) * SUBBLOCKS
                    + i64::from(offset[axis])
            }),
            size_200: size,
            facing,
            backing: BlockPos {
                space: center.space,
                x: center.x + backing[0],
                y: center.y + backing[1],
                z: center.z + backing[2],
            },
            backing_size,
        }
    }))
}
#[derive(Clone, Copy, Debug)]
pub struct ShowSheet {
    pub title: &'static str,
    pub chain: &'static [(ItemId, u16)],
    pub extra: &'static [(ItemId, &'static str)],
    pub why: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub struct CraftEntry {
    pub name: &'static str,
    pub caption: &'static str,
}
impl Hall {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Double => "Удвоение",
            Self::Steel => "Сталь",
            Self::Wind => "Ветер",
        }
    }
    pub const fn show(self) -> ShowSheet {
        match self {
            Self::Wind => ShowSheet {
                title: "Ветер: руда в слитки",
                chain: &[
                    (I_RAW_IRON, 3),
                    (ENRICHMENT_CHAMBER as ItemId, 1),
                    (ENERGIZED_SMELTER as ItemId, 1),
                    (I_IRON_INGOT, 4),
                ],
                extra: &[
                    (WIND_GENERATOR as ItemId, "энергия ветра: выше — больше"),
                    (UNIVERSAL_CABLE as ItemId, "несёт её к машинам"),
                ],
                why: "3 сырого железа — 4 слитка",
            },
            Self::Steel => ShowSheet {
                title: "Сталь",
                chain: &[
                    (I_IRON_INGOT, 1),
                    (METALLURGIC_INFUSER as ItemId, 1),
                    (METALLURGIC_INFUSER as ItemId, 1),
                    (ENERGIZED_SMELTER as ItemId, 1),
                    (I_STEEL_INGOT, 1),
                ],
                extra: &[
                    (HEAT_GENERATOR as ItemId, "энергия: жжёт уголь"),
                    (I_COAL, "уголь в оба инфузора"),
                ],
                why: "Сталь — на корпуса машин и кабели",
            },
            Self::Double => ShowSheet {
                title: "Удвоение руды",
                chain: &[
                    (I_RAW_IRON, 1),
                    (PURIFICATION_CHAMBER as ItemId, 1),
                    (CRUSHER as ItemId, 1),
                    (ENRICHMENT_CHAMBER as ItemId, 1),
                    (ENERGIZED_SMELTER as ItemId, 1),
                    (I_IRON_INGOT, 2),
                ],
                extra: &[
                    (ELECTROLYTIC_SEPARATOR as ItemId, "кислород из воды"),
                    (LOGISTICAL_TRANSPORTER as ItemId, "руда из сундука"),
                ],
                why: "Из одной руды — два слитка",
            },
        }
    }
    pub const fn craft(self) -> &'static [CraftEntry; 4] {
        match self {
            Self::Wind => &[
                CraftEntry {
                    name: "wind_generator",
                    caption: "Ветряк",
                },
                CraftEntry {
                    name: "enrichment_chamber",
                    caption: "Камера",
                },
                CraftEntry {
                    name: "energized_smelter",
                    caption: "Плавильня",
                },
                CraftEntry {
                    name: "basic_universal_cable",
                    caption: "Кабель",
                },
            ],
            Self::Steel => &[
                CraftEntry {
                    name: "heat_generator",
                    caption: "Генератор",
                },
                CraftEntry {
                    name: "metallurgic_infuser",
                    caption: "Инфузор",
                },
                CraftEntry {
                    name: "steel_casing",
                    caption: "Корпус",
                },
                CraftEntry {
                    name: "energy_tablet",
                    caption: "Планшет",
                },
            ],
            Self::Double => &[
                CraftEntry {
                    name: "electrolytic_separator",
                    caption: "Сепаратор",
                },
                CraftEntry {
                    name: "purification_chamber",
                    caption: "Очиститель",
                },
                CraftEntry {
                    name: "crusher",
                    caption: "Дробилка",
                },
                CraftEntry {
                    name: "basic_logistical_transporter",
                    caption: "Транспортёр",
                },
            ],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecipeCells {
    pub grid: [ItemId; 9],
    pub output: ItemId,
    pub count: u16,
}
/// Canonical compiled RecipeBook data; tags display their first allowed item.
pub fn recipe_cells(book: &RecipeBook, name: &str) -> Result<RecipeCells, Error> {
    let (_, recipe) = book
        .iter()
        .find(|(_, recipe)| recipe.name() == name)
        .ok_or(Error::MissingRecipe)?;
    let mut grid = [0; 9];
    let first = |ingredient: &crate::crafting::Ing| {
        ingredient
            .items()
            .first()
            .copied()
            .ok_or(Error::UnsupportedRecipe)
    };
    match recipe.kind() {
        RecipeKind::Shaped(pattern) => {
            let w = pattern.width();
            let h = pattern.height();
            let ox = (3 - w) / 2;
            let oy = (3 - h) / 2;
            for y in 0..h {
                for x in 0..w {
                    if let Some(ingredient) = pattern.cells()[x + y * w] {
                        grid[ox + x + (oy + y) * 3] = first(&ingredient)?;
                    }
                }
            }
        }
        RecipeKind::Shapeless { ingredients, count } => {
            for (index, ingredient) in ingredients[..usize::from(*count)].iter().enumerate() {
                grid[index] = first(ingredient.as_ref().ok_or(Error::UnsupportedRecipe)?)?;
            }
        }
        RecipeKind::Repair => return Err(Error::UnsupportedRecipe),
    }
    Ok(RecipeCells {
        grid,
        output: recipe.output_item(),
        count: recipe.output_count(),
    })
}
