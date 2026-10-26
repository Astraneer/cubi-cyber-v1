//! Bounded, allocation-free recipe matching and detached atomic craft plans.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ItemId, ids::*, items::*, stack::ItemStack};

pub use crate::crafting_data::{g7_recipes, path_recipes};

pub const TAG_PLANKS: &[ItemId] = &[PLANKS, BIRCH_PLANKS, SPRUCE_PLANKS];
pub const TAG_OAK_LOGS: &[ItemId] = &[LOG];
pub const TAG_BIRCH_LOGS: &[ItemId] = &[BIRCH_LOG];
pub const TAG_SPRUCE_LOGS: &[ItemId] = &[SPRUCE_LOG];
pub const TAG_STONE_TOOL_MATERIALS: &[ItemId] = &[COBBLE];
pub const TAG_STONE_CRAFTING_MATERIALS: &[ItemId] = &[COBBLE];
pub const LIST_COAL_OR_CHARCOAL: &[ItemId] = &[I_COAL, I_CHARCOAL];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ing {
    Item(ItemId),
    Set(&'static [ItemId]),
}

pub use Ing as Ingredient;

impl Ing {
    pub fn items(&self) -> &[ItemId] {
        match self {
            Self::Item(id) => std::slice::from_ref(id),
            Self::Set(ids) => ids,
        }
    }

    pub fn accepts(&self, stack: ItemStack) -> bool {
        !stack.is_empty() && stack.payload().is_none() && self.items().contains(&stack.item())
    }

    fn validate(&self) -> bool {
        !self.items().is_empty() && self.items().iter().all(|&id| crate::item(id).is_some())
    }
}

/// Startup-only recipe definitions; the compiled book uses fixed-size patterns.
#[derive(Clone, Debug)]
pub enum RShape {
    Shaped(Vec<&'static str>, Vec<(char, Ing)>),
    Shapeless(Vec<Ing>),
}

#[derive(Clone, Debug)]
pub struct RecipeDef {
    pub name: &'static str,
    pub shape: RShape,
    pub out: ItemId,
    pub count: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pattern {
    width: u8,
    height: u8,
    cells: [Option<Ing>; 9],
    occupied: u8,
    symmetric: bool,
}

impl Pattern {
    pub const fn width(&self) -> usize {
        self.width as usize
    }
    pub const fn height(&self) -> usize {
        self.height as usize
    }
    pub fn cells(&self) -> &[Option<Ing>] {
        &self.cells[..self.width() * self.height()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CraftError {
    InvalidGridSize,
    OutsideGrid,
    InvalidPattern,
    InvalidIngredient,
    InvalidOutput,
    TooManyRecipes,
    NoRecipe,
    InvalidCraftCount,
    NotEnoughIngredients,
    NoCapacity,
    StalePlan,
}

pub fn compile_pattern(rows: &[&str], key: &[(char, Ing)]) -> Result<Pattern, CraftError> {
    if rows.is_empty() || rows.len() > 3 || key.len() > 9 {
        return Err(CraftError::InvalidPattern);
    }
    let width = rows[0].chars().count();
    if !(1..=3).contains(&width) || rows.iter().any(|row| row.chars().count() != width) {
        return Err(CraftError::InvalidPattern);
    }
    for (index, &(symbol, ingredient)) in key.iter().enumerate() {
        if symbol == ' ' || key[..index].iter().any(|&(other, _)| other == symbol) {
            return Err(CraftError::InvalidPattern);
        }
        if !ingredient.validate() {
            return Err(CraftError::InvalidIngredient);
        }
    }
    let mut raw = [' '; 9];
    let (mut left, mut right, mut top, mut bottom) = (3, 0, 3, 0);
    for (y, row) in rows.iter().enumerate() {
        for (x, symbol) in row.chars().enumerate() {
            raw[x + y * 3] = symbol;
            if symbol != ' ' {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    if left == 3 {
        return Err(CraftError::InvalidPattern);
    }
    let (width, height) = (right - left + 1, bottom - top + 1);
    let mut cells = [None; 9];
    let mut used = [false; 9];
    let mut occupied = 0;
    for y in 0..height {
        for x in 0..width {
            let symbol = raw[left + x + (top + y) * 3];
            if symbol != ' ' {
                let index = key
                    .iter()
                    .position(|&(character, _)| character == symbol)
                    .ok_or(CraftError::InvalidPattern)?;
                used[index] = true;
                cells[x + y * width] = Some(key[index].1);
                occupied += 1;
            }
        }
    }
    if used[..key.len()].iter().any(|&value| !value) {
        return Err(CraftError::InvalidPattern);
    }
    let symmetric = (0..height)
        .all(|y| (0..width / 2).all(|x| cells[x + y * width] == cells[width - 1 - x + y * width]));
    Ok(Pattern {
        width: width as u8,
        height: height as u8,
        cells,
        occupied,
        symmetric,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    width: u8,
    height: u8,
    cells: [ItemStack; 9],
}

#[derive(Clone, Copy)]
struct Trim {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    occupied: usize,
}

impl Grid {
    pub fn new(width: usize, height: usize) -> Result<Self, CraftError> {
        if !(1..=3).contains(&width) || !(1..=3).contains(&height) {
            return Err(CraftError::InvalidGridSize);
        }
        Ok(Self {
            width: width as u8,
            height: height as u8,
            cells: [ItemStack::EMPTY; 9],
        })
    }

    pub const fn width(&self) -> usize {
        self.width as usize
    }
    pub const fn height(&self) -> usize {
        self.height as usize
    }
    pub fn cells(&self) -> &[ItemStack] {
        &self.cells[..self.width() * self.height()]
    }

    pub fn get(&self, x: usize, y: usize) -> Option<ItemStack> {
        (x < self.width() && y < self.height()).then(|| self.cells[x + y * self.width()])
    }

    pub fn set(&mut self, x: usize, y: usize, value: ItemStack) -> Result<(), CraftError> {
        if x >= self.width() || y >= self.height() {
            return Err(CraftError::OutsideGrid);
        }
        let index = x + y * self.width();
        self.cells[index] = value;
        Ok(())
    }

    fn trim(&self) -> Option<Trim> {
        let (mut left, mut right, mut top, mut bottom, mut occupied) = (3, 0, 3, 0, 0);
        for y in 0..self.height() {
            for x in 0..self.width() {
                if !self.cells[x + y * self.width()].is_empty() {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                    occupied += 1;
                }
            }
        }
        (occupied != 0).then(|| Trim {
            left,
            top,
            width: right - left + 1,
            height: bottom - top + 1,
            occupied,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeKind {
    Shaped(Pattern),
    Shapeless {
        ingredients: [Option<Ing>; 9],
        count: u8,
    },
    Repair,
}

#[derive(Clone, Debug)]
pub struct Recipe {
    name: &'static str,
    output: ItemId,
    count: u16,
    kind: RecipeKind,
}

impl Recipe {
    pub const fn name(&self) -> &'static str {
        self.name
    }
    pub const fn output_item(&self) -> ItemId {
        self.output
    }
    pub const fn output_count(&self) -> u16 {
        self.count
    }
    pub const fn kind(&self) -> &RecipeKind {
        &self.kind
    }

    pub fn ingredients(&self) -> impl Iterator<Item = &Ing> {
        let cells: &[Option<Ing>] = match &self.kind {
            RecipeKind::Shaped(pattern) => pattern.cells(),
            RecipeKind::Shapeless { ingredients, count } => &ingredients[..usize::from(*count)],
            RecipeKind::Repair => &[],
        };
        cells.iter().flatten()
    }

    pub fn matches(&self, grid: &Grid) -> bool {
        grid.trim()
            .is_some_and(|trim| self.matches_trimmed(grid, trim))
    }

    fn matches_trimmed(&self, grid: &Grid, trim: Trim) -> bool {
        match &self.kind {
            RecipeKind::Shaped(pattern) => {
                if trim.occupied != usize::from(pattern.occupied)
                    || trim.width != pattern.width()
                    || trim.height != pattern.height()
                {
                    return false;
                }
                let fits = |mirror| {
                    (0..trim.height).all(|y| {
                        (0..trim.width).all(|x| {
                            let source_x = if mirror { trim.width - x - 1 } else { x };
                            let stack = grid.cells[trim.left + x + (trim.top + y) * grid.width()];
                            match pattern.cells[source_x + y * trim.width] {
                                Some(ingredient) => ingredient.accepts(stack),
                                None => stack.is_empty(),
                            }
                        })
                    })
                };
                fits(false) || (!pattern.symmetric && fits(true))
            }
            RecipeKind::Shapeless { ingredients, count } => {
                if trim.occupied != usize::from(*count) {
                    return false;
                }
                let mut stacks = [ItemStack::EMPTY; 9];
                let mut len = 0;
                for stack in grid.cells() {
                    if !stack.is_empty() {
                        stacks[len] = *stack;
                        len += 1;
                    }
                }
                assign_all(&ingredients[..usize::from(*count)], &stacks[..len])
            }
            RecipeKind::Repair => repair_pair(grid).is_some(),
        }
    }

    pub fn result(&self, grid: &Grid) -> Option<ItemStack> {
        if !self.matches(grid) {
            return None;
        }
        if matches!(self.kind, RecipeKind::Repair) {
            let (a, b) = repair_pair(grid)?;
            let maximum = u32::from(idef(a.item()).max_damage);
            let remaining = (maximum - u32::from(a.damage()))
                + (maximum - u32::from(b.damage()))
                + maximum * 5 / 100;
            let damage = maximum.saturating_sub(remaining) as u16;
            ItemStack::from_parts(a.item(), 1, damage).ok()
        } else {
            ItemStack::new(self.output, self.count).ok()
        }
    }
}

fn repair_pair(grid: &Grid) -> Option<(ItemStack, ItemStack)> {
    let mut values = grid
        .cells()
        .iter()
        .copied()
        .filter(|value| !value.is_empty());
    let (a, b) = (values.next()?, values.next()?);
    (values.next().is_none()
        && a.item() == b.item()
        && a.count() == 1
        && b.count() == 1
        && idef(a.item()).max_damage > 0)
        .then_some((a, b))
}

/// Augmenting-path matching is bounded by nine ingredients and uses no heap.
fn assign_all(ingredients: &[Option<Ing>], stacks: &[ItemStack]) -> bool {
    fn take(
        index: usize,
        ingredients: &[Option<Ing>],
        stacks: &[ItemStack],
        seen: &mut u16,
        owners: &mut [Option<usize>; 9],
    ) -> bool {
        let ingredient = ingredients[index].expect("compiled ingredient");
        for (slot, stack) in stacks.iter().enumerate() {
            if *seen & (1 << slot) != 0 || !ingredient.accepts(*stack) {
                continue;
            }
            *seen |= 1 << slot;
            if owners[slot].is_none_or(|owner| take(owner, ingredients, stacks, seen, owners)) {
                owners[slot] = Some(index);
                return true;
            }
        }
        false
    }
    let mut owners = [None; 9];
    (0..ingredients.len()).all(|index| take(index, ingredients, stacks, &mut 0, &mut owners))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecipeId(u16);

impl RecipeId {
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }
    pub const fn raw(self) -> u16 {
        self.0
    }
}

#[derive(Debug)]
pub struct RecipeBook {
    recipes: Vec<Recipe>,
    outputs: BTreeMap<ItemId, Vec<RecipeId>>,
    uses: BTreeMap<ItemId, Vec<RecipeId>>,
}

impl RecipeBook {
    pub fn build(definitions: impl IntoIterator<Item = RecipeDef>) -> Result<Self, CraftError> {
        let mut recipes = Vec::new();
        for definition in definitions {
            if recipes.len() >= u16::MAX as usize {
                return Err(CraftError::TooManyRecipes);
            }
            let count = u16::try_from(definition.count).map_err(|_| CraftError::InvalidOutput)?;
            ItemStack::new(definition.out, count).map_err(|_| CraftError::InvalidOutput)?;
            if count == 0 {
                return Err(CraftError::InvalidOutput);
            }
            let kind = match definition.shape {
                RShape::Shaped(rows, key) => RecipeKind::Shaped(compile_pattern(&rows, &key)?),
                RShape::Shapeless(values) => {
                    if values.is_empty()
                        || values.len() > 9
                        || values.iter().any(|value| !value.validate())
                    {
                        return Err(CraftError::InvalidIngredient);
                    }
                    let mut ingredients = [None; 9];
                    for (target, value) in ingredients.iter_mut().zip(&values) {
                        *target = Some(*value);
                    }
                    RecipeKind::Shapeless {
                        ingredients,
                        count: values.len() as u8,
                    }
                }
            };
            recipes.push(Recipe {
                name: definition.name,
                output: definition.out,
                count,
                kind,
            });
        }
        let mut book = Self {
            recipes,
            outputs: BTreeMap::new(),
            uses: BTreeMap::new(),
        };
        book.build_indexes();
        Ok(book)
    }

    pub fn standard() -> Result<Self, CraftError> {
        let mut book = Self::build(path_recipes())?;
        book.recipes.push(Recipe {
            name: "repair_item",
            output: 0,
            count: 1,
            kind: RecipeKind::Repair,
        });
        book.build_indexes();
        Ok(book)
    }

    fn build_indexes(&mut self) {
        self.outputs.clear();
        self.uses.clear();
        for (index, recipe) in self.recipes.iter().enumerate() {
            let id = RecipeId(index as u16);
            if matches!(recipe.kind, RecipeKind::Repair) {
                for (item, definition) in crate::all_items() {
                    if definition.max_damage > 0 {
                        self.outputs.entry(item).or_default().push(id);
                        self.uses.entry(item).or_default().push(id);
                    }
                }
            } else {
                self.outputs.entry(recipe.output).or_default().push(id);
                let inputs: BTreeSet<_> = recipe
                    .ingredients()
                    .flat_map(|value| value.items().iter().copied())
                    .collect();
                for item in inputs {
                    self.uses.entry(item).or_default().push(id);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.recipes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }
    pub fn get(&self, id: RecipeId) -> Option<&Recipe> {
        self.recipes.get(usize::from(id.0))
    }
    pub fn iter(&self) -> impl Iterator<Item = (RecipeId, &Recipe)> {
        self.recipes
            .iter()
            .enumerate()
            .map(|(index, recipe)| (RecipeId(index as u16), recipe))
    }

    pub fn find(&self, grid: &Grid) -> Option<(RecipeId, &Recipe)> {
        let trim = grid.trim()?;
        self.iter()
            .find(|(_, recipe)| recipe.matches_trimmed(grid, trim))
    }

    pub fn result(&self, grid: &Grid) -> ItemStack {
        self.find(grid)
            .and_then(|(_, recipe)| recipe.result(grid))
            .unwrap_or(ItemStack::EMPTY)
    }

    pub fn recipes_producing(&self, item: ItemId) -> impl Iterator<Item = (RecipeId, &Recipe)> {
        self.outputs
            .get(&item)
            .into_iter()
            .flatten()
            .map(|&id| (id, &self.recipes[usize::from(id.0)]))
    }

    pub fn recipes_using(&self, item: ItemId) -> impl Iterator<Item = (RecipeId, &Recipe)> {
        self.uses
            .get(&item)
            .into_iter()
            .flatten()
            .map(|&id| (id, &self.recipes[usize::from(id.0)]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemainderPolicy {
    RejectOverflow,
    /// The caller must turn these explicit drops into gameplay events in the same
    /// authoritative transaction. Output items themselves are never discarded.
    CollectDrops,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotChange {
    pub slot: usize,
    pub before: ItemStack,
    pub after: ItemStack,
}

/// Detached deterministic value plan. A world owner can atomically commit its
/// grid/slot changes and explicit drops as component changes/events.
#[derive(Debug)]
#[must_use = "Commit or explicitly discard the complete craft plan, including its drops"]
pub struct CraftPlan {
    recipe: RecipeId,
    crafts: u16,
    before_grid: Grid,
    after_grid: Grid,
    before_inventory: Vec<ItemStack>,
    after_inventory: Vec<ItemStack>,
    changes: Vec<SlotChange>,
    drops: Vec<ItemStack>,
}

impl CraftPlan {
    pub fn prepare_exact(
        book: &RecipeBook,
        grid: &Grid,
        inventory: &[ItemStack],
        crafts: u16,
        policy: RemainderPolicy,
    ) -> Result<Self, CraftError> {
        if crafts == 0 {
            return Err(CraftError::InvalidCraftCount);
        }
        let (id, recipe) = book.find(grid).ok_or(CraftError::NoRecipe)?;
        let mut after_grid = *grid;
        let mut after_inventory = inventory.to_vec();
        let mut drops = Vec::new();
        for _ in 0..crafts {
            let output = recipe
                .result(&after_grid)
                .ok_or(CraftError::NotEnoughIngredients)?;
            if !insert(&mut after_inventory, output) {
                return Err(CraftError::NoCapacity);
            }
            for stack in &mut after_grid.cells[..grid.width() * grid.height()] {
                if stack.is_empty() {
                    continue;
                }
                let remainder = idef(stack.item()).remainder;
                *stack = stack
                    .with_count(stack.count() - 1)
                    .expect("decrement valid stack");
                if remainder == 0 {
                    continue;
                }
                let remainder =
                    ItemStack::new(remainder, 1).map_err(|_| CraftError::InvalidOutput)?;
                if stack.is_empty() {
                    *stack = remainder;
                } else if stack.same_kind(remainder) && stack.count() < stack.maximum() {
                    *stack = stack.with_count(stack.count() + 1).expect("bounded merge");
                } else if !insert(&mut after_inventory, remainder) {
                    match policy {
                        RemainderPolicy::RejectOverflow => return Err(CraftError::NoCapacity),
                        RemainderPolicy::CollectDrops => drops.push(remainder),
                    }
                }
            }
        }
        let changes = inventory
            .iter()
            .zip(&after_inventory)
            .enumerate()
            .filter_map(|(slot, (&before, &after))| {
                (before != after).then_some(SlotChange {
                    slot,
                    before,
                    after,
                })
            })
            .collect();
        Ok(Self {
            recipe: id,
            crafts,
            before_grid: *grid,
            after_grid,
            before_inventory: inventory.to_vec(),
            after_inventory,
            changes,
            drops,
        })
    }

    pub const fn recipe(&self) -> RecipeId {
        self.recipe
    }
    pub const fn crafts(&self) -> u16 {
        self.crafts
    }
    pub const fn grid_after(&self) -> &Grid {
        &self.after_grid
    }
    pub fn changes(&self) -> &[SlotChange] {
        &self.changes
    }
    pub fn drops(&self) -> &[ItemStack] {
        &self.drops
    }

    /// Validate every input first, then publish all local values together. This
    /// consumes the plan to prevent a second application of the same action.
    pub fn apply(
        self,
        grid: &mut Grid,
        inventory: &mut [ItemStack],
    ) -> Result<Vec<ItemStack>, CraftError> {
        if *grid != self.before_grid || inventory != self.before_inventory {
            return Err(CraftError::StalePlan);
        }
        *grid = self.after_grid;
        inventory.copy_from_slice(&self.after_inventory);
        Ok(self.drops)
    }
}

fn insert(slots: &mut [ItemStack], mut value: ItemStack) -> bool {
    for slot in slots.iter_mut() {
        if !slot.is_empty() && slot.same_kind(value) {
            let take = value.count().min(slot.maximum() - slot.count());
            *slot = slot.with_count(slot.count() + take).expect("bounded merge");
            value = value
                .with_count(value.count() - take)
                .expect("bounded remainder");
            if value.is_empty() {
                return true;
            }
        }
    }
    for slot in slots.iter_mut() {
        if slot.is_empty() {
            *slot = value;
            return true;
        }
    }
    value.is_empty()
}

/// Pure Minecraft reach check, using squared distance to the block box.
pub fn crafting_table_in_reach(eye: [f64; 3], position: [i32; 3], block_is_table: bool) -> bool {
    block_is_table
        && eye
            .into_iter()
            .zip(position)
            .map(|(coordinate, start)| {
                let start = f64::from(start);
                let distance = (start - coordinate)
                    .max(coordinate - (start + 1.0))
                    .max(0.0);
                distance * distance
            })
            .sum::<f64>()
            < 72.25
}
