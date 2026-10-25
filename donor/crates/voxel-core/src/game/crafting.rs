//! Крафт (ASP-09): сетка 2×2 у окна инвентаря и 3×3 у верстака, 46 рецептов объёма, обрезка
//! рамки, зеркало по горизонтали, бесформенные (паросочетание), остатки рецепта, починка двумя предметами,
//! доступность верстака (72.25). Сами окна и клики — screen.rs.
//!
//! Сопоставление (CraftingRecipeInput.createPositioned, RawShapedRecipe.matches): сетка обрезается до рамки непустых
//! клеток; число непустых клеток, ширина и высота должны совпасть с узором; узор сравнивается прямо или зеркально.
//! Ингредиент смотрит только на предмет (Ingredient.test): количество и износ не важны. Порядок поиска — порядок
//! таблицы; пересечений между рецептами объёма нет (тест no_two_recipes_share_a_grid).

use crate::registry::ids::*;
use crate::registry::items::{
    idef, ItemId, I_BREAD, I_BUCKET, I_CHARCOAL, I_CLAY_BALL, I_COAL, I_DIAMOND, I_GOLDEN_HELMET, I_IRON_INGOT, I_IRON_NUGGET,
    I_LEATHER, I_LEATHER_HELMET, I_RAW_IRON, I_SHEARS, I_SNOWBALL, I_STICK, I_WHEAT, I_WOODEN_PICKAXE,
};
use crate::registry::items::I_GOLD_INGOT;
use crate::world::stack::ItemStack;

/// Теги и списки — только то, что есть в нашем мире (ни ободранных брёвен, ни чернита, ни сланца).
pub(crate) const TAG_PLANKS: &[ItemId] = &[PLANKS as ItemId, BIRCH_PLANKS as ItemId, SPRUCE_PLANKS as ItemId];
pub(crate) const TAG_OAK_LOGS: &[ItemId] = &[LOG as ItemId];
pub(crate) const TAG_BIRCH_LOGS: &[ItemId] = &[BIRCH_LOG as ItemId];
pub(crate) const TAG_SPRUCE_LOGS: &[ItemId] = &[SPRUCE_LOG as ItemId];
pub(crate) const TAG_STONE_TOOL_MATERIALS: &[ItemId] = &[COBBLE as ItemId];
pub(crate) const TAG_STONE_CRAFTING_MATERIALS: &[ItemId] = &[COBBLE as ItemId];
pub(crate) const LIST_COAL_OR_CHARCOAL: &[ItemId] = &[I_COAL, I_CHARCOAL]; // список прямо в рецепте факела, не тег

/// Ингредиент: предмет или набор предметов (тег).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ing {
    Item(ItemId),
    Set(&'static [ItemId]),
}

impl Ing {
    pub(crate) fn test(&self, s: &ItemStack) -> bool {
        !s.is_empty()
            && match *self {
                Ing::Item(i) => s.item == i,
                Ing::Set(v) => v.contains(&s.item),
            }
    }
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[ItemId] {
        match self {
            Ing::Item(i) => std::slice::from_ref(i),
            Ing::Set(v) => v,
        }
    }
    #[cfg(test)]
    pub(crate) fn overlaps(&self, o: &Ing) -> bool {
        self.items().iter().any(|i| o.items().contains(i))
    }
}

pub(crate) enum RShape {
    /// Ряды узора (пробел — пусто) и ключ.
    Shaped(Vec<&'static str>, Vec<(char, Ing)>),
    Shapeless(Vec<Ing>),
}

pub(crate) struct RecipeDef {
    pub(crate) name: &'static str,
    pub(crate) shape: RShape,
    pub(crate) out: ItemId,
    pub(crate) count: i32,
}

/// Узор после обрезки пустых рядов и столбцов (RawShapedRecipe.removePadding).
#[derive(Debug, Clone)]
pub(crate) struct Pattern {
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) cells: Vec<Option<Ing>>,
    pub(crate) n: usize,
    /// Каждый ряд читается одинаково с обеих сторон (Util.isSymmetrical) — только экономия зеркальной проверки.
    pub(crate) symmetric: bool,
}

pub(crate) fn compile_pattern(rows: &[&str], key: &[(char, Ing)]) -> Result<Pattern, String> {
    if rows.is_empty() || rows.len() > 3 {
        return Err("1..3 ряда".into());
    }
    let width = rows[0].chars().count();
    if width == 0 || width > 3 || rows.iter().any(|r| r.chars().count() != width) {
        return Err("ряды 1..3, одной ширины".into());
    }
    let filled = |r: &str| r.chars().any(|c| c != ' ');
    let top = rows.iter().position(|r| filled(r)).ok_or("пустой узор")?;
    let bottom = rows.iter().rposition(|r| filled(r)).unwrap();
    let col_filled = |x: usize| rows.iter().any(|r| r.chars().nth(x) != Some(' '));
    let left = (0..width).find(|&x| col_filled(x)).unwrap();
    let right = (0..width).rfind(|&x| col_filled(x)).unwrap();
    let (w, h) = (right - left + 1, bottom - top + 1);
    let mut cells = Vec::with_capacity(w * h);
    let mut used = vec![false; key.len()];
    for r in &rows[top..=bottom] {
        for c in r.chars().skip(left).take(w) {
            if c == ' ' {
                cells.push(None);
                continue;
            }
            let k = key.iter().position(|(ch, _)| *ch == c).ok_or(format!("символ {c} не в ключе"))?;
            used[k] = true;
            cells.push(Some(key[k].1));
        }
    }
    if used.iter().any(|u| !u) {
        return Err("лишний символ в ключе".into());
    }
    let n = cells.iter().filter(|c| c.is_some()).count();
    let symmetric = (0..h).all(|y| (0..w / 2).all(|x| cells[x + y * w] == cells[w - 1 - x + y * w]));
    Ok(Pattern { w, h, cells, n, symmetric })
}

/// Сетка крафта: клетка (x, y) = cells[x + y·w] (как j + i·w в Java).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) cells: [ItemStack; 9],
}

/// Рамка непустых клеток (left, top, w, h) и их число.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Trim {
    pub(crate) left: usize,
    pub(crate) top: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) n: usize,
}

impl Grid {
    pub(crate) fn new(w: usize, h: usize) -> Grid {
        Grid { w, h, cells: [ItemStack::EMPTY; 9] }
    }
    pub(crate) fn size(&self) -> usize {
        self.w * self.h
    }
    pub(crate) fn at(&self, x: usize, y: usize) -> &ItemStack {
        &self.cells[x + y * self.w]
    }
    pub(crate) fn set(&mut self, x: usize, y: usize, s: ItemStack) {
        self.cells[x + y * self.w] = s;
    }
    pub(crate) fn trim(&self) -> Option<Trim> {
        let (mut l, mut r, mut t, mut b, mut n) = (usize::MAX, 0, usize::MAX, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if !self.at(x, y).is_empty() {
                    l = l.min(x);
                    r = r.max(x);
                    t = t.min(y);
                    b = b.max(y);
                    n += 1;
                }
            }
        }
        (n > 0).then(|| Trim { left: l, top: t, w: r - l + 1, h: b - t + 1, n })
    }
}

pub(crate) enum RecipeKind {
    Shaped(Pattern),
    Shapeless(Vec<Ing>),
    /// Особый рецепт «починка» (RepairItemRecipe): два одинаковых предмета с прочностью по 1 штуке.
    Repair,
}

pub(crate) struct Recipe {
    #[allow(dead_code)] // имя рецепта — для отладки и проверок
    pub(crate) name: &'static str,
    pub(crate) out: ItemId,
    pub(crate) count: i32,
    pub(crate) kind: RecipeKind,
}

/// Два непустых стека рамки для починки (RepairItemRecipe.findPair) или None.
pub(crate) fn repair_pair(g: &Grid, t: &Trim) -> Option<(ItemStack, ItemStack)> {
    if t.n != 2 {
        return None;
    }
    let mut it = (0..t.h).flat_map(|y| (0..t.w).map(move |x| (x, y))).map(|(x, y)| *g.at(t.left + x, t.top + y)).filter(|s| !s.is_empty());
    let (a, b) = (it.next()?, it.next()?);
    let max = idef(a.item).max_damage;
    (a.item == b.item && a.count == 1 && b.count == 1 && max > 0).then_some((a, b))
}

impl Recipe {
    pub(crate) fn matches(&self, g: &Grid, t: &Trim) -> bool {
        match &self.kind {
            RecipeKind::Shaped(p) => {
                if t.n != p.n || t.w != p.w || t.h != p.h {
                    return false;
                }
                (!p.symmetric && Self::fits(p, g, t, true)) || Self::fits(p, g, t, false)
            }
            RecipeKind::Shapeless(ings) => {
                if t.n != ings.len() {
                    return false;
                }
                let mut stacks = Vec::with_capacity(9);
                for y in 0..t.h {
                    for x in 0..t.w {
                        let s = *g.at(t.left + x, t.top + y);
                        if !s.is_empty() {
                            stacks.push(s);
                        }
                    }
                }
                assign_all(ings, &stacks)
            }
            RecipeKind::Repair => repair_pair(g, t).is_some(),
        }
    }
    pub(crate) fn fits(p: &Pattern, g: &Grid, t: &Trim, mirror: bool) -> bool {
        (0..p.h).all(|y| {
            (0..p.w).all(|x| {
                let px = if mirror { p.w - 1 - x } else { x };
                let s = g.at(t.left + x, t.top + y);
                match &p.cells[px + y * p.w] {
                    None => s.is_empty(),
                    Some(i) => i.test(s),
                }
            })
        })
    }
    /// Итог одного крафта по этой сетке (у починки — износ по формуле, у прочих — копия выхода).
    pub(crate) fn craft(&self, g: &Grid, t: &Trim) -> ItemStack {
        match self.kind {
            RecipeKind::Repair => {
                let Some((a, b)) = repair_pair(g, t) else { return ItemStack::EMPTY };
                let max = idef(a.item).max_damage as i32;
                let left = (max - a.damage as i32) + (max - b.damage as i32) + max * 5 / 100;
                ItemStack { item: a.item, count: 1, damage: (max - left).max(0) as u16 }
            }
            _ => ItemStack::of(self.out, self.count),
        }
    }
}

/// Бесформенный рецепт: каждому ингредиенту — свой стек (паросочетание, у Java — RecipeMatcher).
pub(crate) fn assign_all(ings: &[Ing], stacks: &[ItemStack]) -> bool {
    fn try_take(i: usize, ings: &[Ing], st: &[ItemStack], seen: &mut [bool], owner: &mut [Option<usize>]) -> bool {
        for s in 0..st.len() {
            if seen[s] || !ings[i].test(&st[s]) {
                continue;
            }
            seen[s] = true;
            if owner[s].map_or(true, |o| try_take(o, ings, st, seen, owner)) {
                owner[s] = Some(i);
                return true;
            }
        }
        false
    }
    let mut owner = vec![None; stacks.len()];
    (0..ings.len()).all(|i| try_take(i, ings, stacks, &mut vec![false; stacks.len()], &mut owner))
}

pub(crate) struct RecipeBook {
    pub(crate) list: Vec<Recipe>,
}

impl RecipeBook {
    pub(crate) fn build(defs: Vec<RecipeDef>) -> Result<RecipeBook, String> {
        let mut list = Vec::new();
        for d in defs {
            let kind = match d.shape {
                RShape::Shaped(rows, key) => RecipeKind::Shaped(compile_pattern(&rows, &key).map_err(|e| format!("{}: {e}", d.name))?),
                RShape::Shapeless(v) => {
                    if v.is_empty() || v.len() > 9 {
                        return Err(d.name.into());
                    }
                    RecipeKind::Shapeless(v)
                }
            };
            list.push(Recipe { name: d.name, out: d.out, count: d.count, kind });
        }
        Ok(RecipeBook { list })
    }
    /// Рецепты объёма (46, ASP-09 §4.2) и в конце — особая «починка».
    pub(crate) fn standard() -> RecipeBook {
        let mut b = RecipeBook::build(path_recipes()).expect("рецепты");
        b.list.push(Recipe { name: "repair_item", out: 0, count: 1, kind: RecipeKind::Repair });
        b
    }
    /// RecipeManager.getFirstMatch: пустая сетка — ничего; иначе первый подходящий по порядку таблицы.
    pub(crate) fn find(&self, g: &Grid) -> Option<(usize, Trim)> {
        let t = g.trim()?;
        self.list.iter().position(|r| r.matches(g, &t)).map(|i| (i, t))
    }
    /// То, что показывает слот результата: итог ОДНОГО крафта.
    pub(crate) fn result(&self, g: &Grid) -> ItemStack {
        self.find(g).map_or(ItemStack::EMPTY, |(i, t)| self.list[i].craft(g, &t))
    }
    /// CraftingResultSlot.onTakeItem: в рамке −1 из каждой непустой клетки, затем остаток рецепта
    /// (Item.recipeRemainder). Остаток: в опустевшую клетку; к такому же — сложить (не больше предела стека —
    /// ASP-09 §6.3, в Java предела нет); иначе — `give` (в инвентарь, не влезло — бросок). Без рецепта — ничего
    /// (в Java здесь путь к удвоению — не повторяем). Результат после этого пересчитывает вызывающий.
    pub(crate) fn consume(&self, g: &mut Grid, give: &mut dyn FnMut(ItemStack)) {
        let Some((_, t)) = self.find(g) else { return };
        let rems: Vec<ItemId> = (0..t.h * t.w)
            .map(|k| {
                let s = g.at(t.left + k % t.w, t.top + k / t.w);
                if s.is_empty() { 0 } else { idef(s.item).remainder }
            })
            .collect();
        for k in 0..t.h * t.w {
            let (x, y) = (t.left + k % t.w, t.top + k / t.w);
            let mut s = *g.at(x, y);
            if !s.is_empty() {
                s.count -= 1;
                if s.count <= 0 {
                    s = ItemStack::EMPTY;
                }
            }
            if rems[k] != 0 {
                let r = ItemStack::of(rems[k], 1);
                if s.is_empty() {
                    s = r;
                } else if s.same_kind(&r) && s.n() < s.max() {
                    s.count += 1;
                } else {
                    give(r);
                }
            }
            g.set(x, y, s);
        }
    }
}

/// Рецепты объёма (misode/mcmeta 1.21.1-data, recipe/*.json), своими словами: 46 записей.
pub(crate) fn path_recipes() -> Vec<RecipeDef> {
    use Ing::*;
    use RShape::*;
    let i = |b: crate::cell::BlockId| b as ItemId;
    let mut v = vec![
        RecipeDef { name: "oak_planks", shape: Shapeless(vec![Set(TAG_OAK_LOGS)]), out: i(PLANKS), count: 4 },
        RecipeDef { name: "birch_planks", shape: Shapeless(vec![Set(TAG_BIRCH_LOGS)]), out: i(BIRCH_PLANKS), count: 4 },
        RecipeDef { name: "spruce_planks", shape: Shapeless(vec![Set(TAG_SPRUCE_LOGS)]), out: i(SPRUCE_PLANKS), count: 4 },
        RecipeDef { name: "stick", shape: Shaped(vec!["#", "#"], vec![('#', Set(TAG_PLANKS))]), out: I_STICK, count: 4 },
        RecipeDef { name: "crafting_table", shape: Shaped(vec!["##", "##"], vec![('#', Set(TAG_PLANKS))]), out: i(CRAFTING_TABLE), count: 1 },
        RecipeDef { name: "furnace", shape: Shaped(vec!["###", "# #", "###"], vec![('#', Set(TAG_STONE_CRAFTING_MATERIALS))]), out: i(FURNACE), count: 1 },
        RecipeDef { name: "torch", shape: Shaped(vec!["X", "#"], vec![('X', Set(LIST_COAL_OR_CHARCOAL)), ('#', Item(I_STICK))]), out: i(TORCH), count: 4 },
        RecipeDef { name: "chest", shape: Shaped(vec!["###", "# #", "###"], vec![('#', Set(TAG_PLANKS))]), out: i(CHEST), count: 1 },
        RecipeDef { name: "ladder", shape: Shaped(vec!["# #", "###", "# #"], vec![('#', Item(I_STICK))]), out: i(LADDER), count: 3 },
        RecipeDef { name: "bucket", shape: Shaped(vec!["# #", " # "], vec![('#', Item(I_IRON_INGOT))]), out: I_BUCKET, count: 1 },
        RecipeDef { name: "shears", shape: Shaped(vec![" #", "# "], vec![('#', Item(I_IRON_INGOT))]), out: I_SHEARS, count: 1 },
        RecipeDef { name: "iron_block", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_IRON_INGOT))]), out: i(IRON_BLOCK), count: 1 },
        RecipeDef { name: "iron_ingot_from_iron_block", shape: Shapeless(vec![Item(i(IRON_BLOCK))]), out: I_IRON_INGOT, count: 9 },
        RecipeDef { name: "iron_nugget", shape: Shapeless(vec![Item(I_IRON_INGOT)]), out: I_IRON_NUGGET, count: 9 },
        RecipeDef { name: "iron_ingot_from_nuggets", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_IRON_NUGGET))]), out: I_IRON_INGOT, count: 1 },
        RecipeDef { name: "raw_iron_block", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_RAW_IRON))]), out: i(RAW_IRON_BLOCK), count: 1 },
        RecipeDef { name: "raw_iron", shape: Shapeless(vec![Item(i(RAW_IRON_BLOCK))]), out: I_RAW_IRON, count: 9 },
        RecipeDef { name: "coal_block", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_COAL))]), out: i(COAL_BLOCK), count: 1 },
        RecipeDef { name: "coal", shape: Shapeless(vec![Item(i(COAL_BLOCK))]), out: I_COAL, count: 9 },
        RecipeDef { name: "snow_block", shape: Shaped(vec!["##", "##"], vec![('#', Item(I_SNOWBALL))]), out: i(SNOW), count: 1 },
        RecipeDef { name: "snow", shape: Shaped(vec!["###"], vec![('#', Item(i(SNOW)))]), out: i(SNOW_LAYER), count: 6 },
        RecipeDef { name: "clay", shape: Shaped(vec!["##", "##"], vec![('#', Item(I_CLAY_BALL))]), out: i(CLAY), count: 1 },
        RecipeDef { name: "sandstone", shape: Shaped(vec!["##", "##"], vec![('#', Item(i(SAND)))]), out: i(SANDSTONE), count: 1 },
    ];
    // Инструменты: X — материал яруса, # — палка. Номера: I_WOODEN_PICKAXE + 5·ярус + вид
    // (вид: 0 кирка, 1 топор, 2 лопата, 3 мотыга, 4 меч — порядок реестра).
    let tiers: [Ing; 3] = [Set(TAG_PLANKS), Set(TAG_STONE_TOOL_MATERIALS), Item(I_IRON_INGOT)];
    let shapes: [(&'static str, Vec<&'static str>); 5] = [
        ("pickaxe", vec!["XXX", " # ", " # "]),
        ("axe", vec!["XX", "X#", " #"]),
        ("shovel", vec!["X", "#", "#"]),
        ("hoe", vec!["XX", " #", " #"]),
        ("sword", vec!["X", "X", "#"]),
    ];
    for (t, mat) in tiers.iter().enumerate() {
        for (k, (name, rows)) in shapes.iter().enumerate() {
            let out = I_WOODEN_PICKAXE + (t * 5 + k) as ItemId;
            v.push(RecipeDef { name, shape: Shaped(rows.clone(), vec![('X', *mat), ('#', Item(I_STICK))]), out, count: 1 });
        }
    }
    // Броня: X — кожа или слиток. Номера: I_LEATHER_HELMET + 4·материал + часть (шлем, нагрудник, поножи, ботинки).
    let parts: [Vec<&'static str>; 4] = [vec!["XXX", "X X"], vec!["X X", "XXX", "XXX"], vec!["XXX", "X X", "X X"], vec!["X X", "X X"]];
    for (m, mat) in [I_LEATHER, I_IRON_INGOT].iter().enumerate() {
        for (p, rows) in parts.iter().enumerate() {
            let out = I_LEATHER_HELMET + (m * 4 + p) as ItemId;
            v.push(RecipeDef { name: "armor", shape: Shaped(rows.clone(), vec![('X', Item(*mat))]), out, count: 1 });
        }
    }
    v.extend(crate::world::mek::recipes::crafting()); // Mekanism M1
    v.extend(g7_recipes()); // команда items: золотая и алмазная броня, хлеб, стог
    v
}

/// Рецепты команды items (G7): золотая и алмазная броня (по узору железной), хлеб, стог сена и пшеница из стога.
pub(crate) fn g7_recipes() -> Vec<RecipeDef> {
    use Ing::*;
    use RShape::*;
    let mut v = Vec::new();
    let parts: [Vec<&'static str>; 4] = [vec!["XXX", "X X"], vec!["X X", "XXX", "XXX"], vec!["XXX", "X X", "X X"], vec!["X X", "X X"]];
    for (m, mat) in [I_GOLD_INGOT, I_DIAMOND].iter().enumerate() {
        for (p, rows) in parts.iter().enumerate() {
            let out = I_GOLDEN_HELMET + (m * 4 + p) as ItemId;
            v.push(RecipeDef { name: "armor", shape: Shaped(rows.clone(), vec![('X', Item(*mat))]), out, count: 1 });
        }
    }
    v.push(RecipeDef { name: "bread", shape: Shaped(vec!["###"], vec![('#', Item(I_WHEAT))]), out: I_BREAD, count: 1 });
    v.push(RecipeDef { name: "hay_block", shape: Shapeless(vec![Item(I_WHEAT); 9]), out: HAY_BLOCK as ItemId, count: 1 });
    v.push(RecipeDef { name: "wheat", shape: Shapeless(vec![Item(HAY_BLOCK as ItemId)]), out: I_WHEAT, count: 9 });
    v
}

/// Может ли одна сетка подойти обоим рецептам (тест «нет пересечений»).
#[cfg(test)]
pub(crate) fn could_both_match(a: &Recipe, b: &Recipe) -> bool {
    fn cells(p: &Pattern) -> Vec<Ing> {
        p.cells.iter().flatten().copied().collect()
    }
    fn bip(x: &[Ing], y: &[Ing]) -> bool {
        if x.len() != y.len() {
            return false;
        }
        fn go(i: usize, x: &[Ing], y: &[Ing], used: &mut Vec<bool>) -> bool {
            if i == x.len() {
                return true;
            }
            for j in 0..y.len() {
                if !used[j] && x[i].overlaps(&y[j]) {
                    used[j] = true;
                    if go(i + 1, x, y, used) {
                        return true;
                    }
                    used[j] = false;
                }
            }
            false
        }
        go(0, x, y, &mut vec![false; y.len()])
    }
    match (&a.kind, &b.kind) {
        (RecipeKind::Shaped(p), RecipeKind::Shaped(q)) => {
            if p.w != q.w || p.h != q.h {
                return false;
            }
            [false, true].iter().any(|&m| {
                (0..p.h).all(|y| {
                    (0..p.w).all(|x| {
                        let px = if m { p.w - 1 - x } else { x };
                        match (&p.cells[px + y * p.w], &q.cells[x + y * q.w]) {
                            (None, None) => true,
                            (Some(i), Some(j)) => i.overlaps(j),
                            _ => false,
                        }
                    })
                })
            })
        }
        (RecipeKind::Shapeless(x), RecipeKind::Shapeless(y)) => bip(x, y),
        (RecipeKind::Shapeless(x), RecipeKind::Shaped(q)) | (RecipeKind::Shaped(q), RecipeKind::Shapeless(x)) => bip(x, &cells(q)),
        _ => false, // починка: только инструменты и броня, которых нет ни в одном рецепте
    }
}

/// ScreenHandler.canUse для верстака: блок на месте и квадрат расстояния от глаз до коробки блока < (4.5 + 4.0)².
pub(crate) fn crafting_table_in_reach(eye: [f64; 3], pos: [i32; 3], block_is_table: bool) -> bool {
    if !block_is_table {
        return false;
    }
    let r = 4.5 + 4.0;
    let axis = |e: f64, lo: i32| {
        let lo = lo as f64;
        (lo - e).max(e - (lo + 1.0)).max(0.0)
    };
    let (dx, dy, dz) = (axis(eye[0], pos[0]), axis(eye[1], pos[1]), axis(eye[2], pos[2]));
    dx * dx + dy * dy + dz * dz < r * r
}

#[cfg(test)]
mod crafting_tests {
    use super::*;
    use crate::registry::items::I_WATER_BUCKET;

    fn book() -> RecipeBook {
        RecipeBook::standard()
    }
    /// Сетка из строк: '.' — пусто, буква — предмет из legend, у всех count = 1.
    fn g(rows: &[&str], legend: &[(char, ItemId)]) -> Grid {
        let (w, h) = (rows[0].len(), rows.len());
        let mut gr = Grid::new(w, h);
        for (y, r) in rows.iter().enumerate() {
            for (x, c) in r.chars().enumerate() {
                if c != '.' {
                    let it = legend.iter().find(|(k, _)| *k == c).unwrap().1;
                    gr.set(x, y, ItemStack::of(it, 1));
                }
            }
        }
        gr
    }
    fn out(b: &RecipeBook, gr: &Grid) -> (ItemId, i32) {
        let r = b.result(gr);
        (r.id(), r.n())
    }
    const fn tool(tier: u16, kind: u16) -> ItemId {
        I_WOODEN_PICKAXE + tier * 5 + kind // вид: 0 кирка, 1 топор, 2 лопата, 3 мотыга, 4 меч
    }
    const fn armor(mat: u16, part: u16) -> ItemId {
        I_LEATHER_HELMET + mat * 4 + part
    }
    const P: (char, ItemId) = ('p', PLANKS as ItemId);
    const B: (char, ItemId) = ('b', BIRCH_PLANKS as ItemId);
    const S: (char, ItemId) = ('s', SPRUCE_PLANKS as ItemId);
    const K: (char, ItemId) = ('k', I_STICK);
    const C: (char, ItemId) = ('c', COBBLE as ItemId);
    const I: (char, ItemId) = ('i', I_IRON_INGOT);
    const D: (char, ItemId) = ('d', DIRT as ItemId);
    const OAK: ItemId = LOG as ItemId;

    #[test]
    fn t00_count() {
        assert_eq!(path_recipes().len() - crate::world::mek::recipes::crafting().len() - g7_recipes().len(), 46, "46 рецептов объёма (ASP-09 §4.2)");
        assert_eq!(g7_recipes().len(), 11, "G7: 8 брони, хлеб, стог, пшеница из стога");
        assert_eq!(crate::world::mek::recipes::crafting().len(), 39, "Mekanism: M1 14 + 5 инструментов + 4 брони; M2 камера и плавильня; M3 ветряк, конфигуратор, 2 улучшения; M4 дробилка, схема, очиститель, ядро, сепаратор, улучшение газа; M5 транспортёр, труба, трубка, насос");
    }
    #[test]
    fn t01_log_any_cell_2x2() {
        let b = book();
        for rows in [["l.", ".."], [".l", ".."], ["..", "l."], ["..", ".l"]] {
            assert_eq!(out(&b, &g(&rows, &[('l', OAK)])), (PLANKS as ItemId, 4));
        }
        assert_eq!(out(&b, &g(&["..", ".l"], &[('l', BIRCH_LOG as ItemId)])), (BIRCH_PLANKS as ItemId, 4));
        assert_eq!(out(&b, &g(&["...", "...", "..l"], &[('l', SPRUCE_LOG as ItemId)])), (SPRUCE_PLANKS as ItemId, 4));
    }
    #[test]
    fn t02_two_logs_nothing() {
        let b = book();
        assert_eq!(out(&b, &g(&["ll", ".."], &[('l', OAK)])).0, 0);
        assert_eq!(out(&b, &g(&["l.", ".m"], &[('l', OAK), ('m', BIRCH_LOG as ItemId)])).0, 0);
    }
    #[test]
    fn t03_sticks_positions() {
        let b = book();
        assert_eq!(out(&b, &g(&["p.", "p."], &[P])), (I_STICK, 4));
        assert_eq!(out(&b, &g(&[".p", ".p"], &[P])), (I_STICK, 4));
        assert_eq!(out(&b, &g(&["pp", ".."], &[P])).0, 0); // по горизонтали — нет (в ваниле — нажимная плита)
        assert_eq!(out(&b, &g(&["p.", "b."], &[P, B])), (I_STICK, 4)); // разные доски
        assert_eq!(out(&b, &g(&["...", "..p", "..s"], &[P, S])), (I_STICK, 4)); // правый столбец 3×3, низ
        assert_eq!(out(&b, &g(&["p..", "...", "p.."], &[P])).0, 0); // с пропуском
        assert_eq!(out(&b, &g(&["p.", "pp"], &[P])).0, 0); // лишняя доска
    }
    #[test]
    fn t04_table_2x2_and_in_3x3() {
        let b = book();
        assert_eq!(out(&b, &g(&["pb", "sp"], &[P, B, S])), (CRAFTING_TABLE as ItemId, 1));
        assert_eq!(out(&b, &g(&["...", ".pp", ".pp"], &[P])), (CRAFTING_TABLE as ItemId, 1));
    }
    #[test]
    fn t05_pickaxe_and_flip() {
        let b = book();
        let l = [P, B, S, K];
        assert_eq!(out(&b, &g(&["pbs", ".k.", ".k."], &l)), (tool(0, 0), 1));
        assert_eq!(out(&b, &g(&[".k.", ".k.", "ppp"], &l)).0, 0); // вверх ногами
        assert_eq!(out(&b, &g(&["ppp", ".k.", ".kd"], &[P, K, D])).0, 0); // лишняя земля
        assert_eq!(out(&b, &g(&["ccc", ".k.", ".k."], &[C, K])), (tool(1, 0), 1));
        assert_eq!(out(&b, &g(&["iii", ".k.", ".k."], &[I, K])), (tool(2, 0), 1));
    }
    #[test]
    fn t06_axe_hoe_mirror_offset() {
        let b = book();
        let l = [P, K];
        for rows in [["pp.", "pk.", ".k."], [".pp", ".pk", "..k"], ["pp.", "kp.", "k.."], [".pp", ".kp", ".k."]] {
            assert_eq!(out(&b, &g(&rows, &l)), (tool(0, 1), 1), "{rows:?}");
        }
        for rows in [["pp.", ".k.", ".k."], ["pp.", "k..", "k.."], [".pp", "..k", "..k"]] {
            assert_eq!(out(&b, &g(&rows, &l)), (tool(0, 3), 1), "{rows:?}");
        }
    }
    #[test]
    fn t07_shovel_sword_any_column() {
        let b = book();
        for x in 0..3 {
            let mut gr = Grid::new(3, 3);
            gr.set(x, 0, ItemStack::of(COBBLE as ItemId, 1));
            gr.set(x, 1, ItemStack::of(I_STICK, 1));
            gr.set(x, 2, ItemStack::of(I_STICK, 1));
            assert_eq!(out(&b, &gr), (tool(1, 2), 1));
            gr.set(x, 1, ItemStack::of(COBBLE as ItemId, 1));
            assert_eq!(out(&b, &gr), (tool(1, 4), 1));
        }
    }
    #[test]
    fn t08_shears_both_diagonals_in_2x2() {
        let b = book();
        assert_eq!(out(&b, &g(&[".i", "i."], &[I])), (I_SHEARS, 1));
        assert_eq!(out(&b, &g(&["i.", ".i"], &[I])), (I_SHEARS, 1));
    }
    #[test]
    fn t09_misc() {
        let b = book();
        assert_eq!(out(&b, &g(&["ccc", "c.c", "ccc"], &[C])), (FURNACE as ItemId, 1));
        assert_eq!(out(&b, &g(&["ccc", "c.c", "cct"], &[C, ('t', STONE as ItemId)])).0, 0);
        assert_eq!(out(&b, &g(&["pbs", "p.b", "sps"], &[P, B, S])), (CHEST as ItemId, 1));
        assert_eq!(out(&b, &g(&["x", "k"], &[('x', I_COAL), K])), (TORCH as ItemId, 4));
        assert_eq!(out(&b, &g(&["x.", ".."], &[('x', I_CHARCOAL)])).0, 0);
        assert_eq!(out(&b, &g(&["x.", "k."], &[('x', I_CHARCOAL), K])), (TORCH as ItemId, 4));
        assert_eq!(out(&b, &g(&["k.k", "kkk", "k.k"], &[K])), (LADDER as ItemId, 3));
        assert_eq!(out(&b, &g(&["i.i", ".i.", "..."], &[I])), (I_BUCKET, 1));
        assert_eq!(out(&b, &g(&["...", ".i.", "i.i"], &[I])).0, 0); // ведро вверх ногами
        assert_eq!(out(&b, &g(&["...", "iii", "i.i"], &[I])), (armor(1, 0), 1)); // шлем внизу сетки
        assert_eq!(out(&b, &g(&["i.i", "i.i", "..."], &[I])), (armor(1, 3), 1)); // ботинки вверху
        assert_eq!(out(&b, &g(&["i.i", "iii", "iii"], &[I])), (armor(1, 1), 1));
        assert_eq!(out(&b, &g(&["iii", "i.i", "i.i"], &[I])), (armor(1, 2), 1));
        assert_eq!(out(&b, &g(&["iii", "iii", "iii"], &[I])), (IRON_BLOCK as ItemId, 1));
        assert_eq!(out(&b, &g(&["..", ".x"], &[('x', IRON_BLOCK as ItemId)])), (I_IRON_INGOT, 9));
        assert_eq!(out(&b, &g(&["x.", ".."], &[('x', I_IRON_INGOT)])), (I_IRON_NUGGET, 9));
        assert_eq!(out(&b, &g(&["xx", "xx"], &[('x', I_SNOWBALL)])), (SNOW as ItemId, 1));
        assert_eq!(out(&b, &g(&["...", "xxx", "..."], &[('x', SNOW as ItemId)])), (SNOW_LAYER as ItemId, 6));
        assert_eq!(out(&b, &g(&["..", ".."], &[])).0, 0);
    }
    #[test]
    fn t11_g7_armor_bread_hay() {
        let b = book();
        let gold = ('g', I_GOLD_INGOT);
        let dia = ('d', I_DIAMOND);
        let wheat = ('w', I_WHEAT);
        // Золотой комплект и алмазный — по узорам железного (шлем внизу сетки, ботинки вверху).
        assert_eq!(out(&b, &g(&["ggg", "g.g", "..."], &[gold])), (I_GOLDEN_HELMET, 1));
        assert_eq!(out(&b, &g(&["g.g", "ggg", "ggg"], &[gold])), (I_GOLDEN_HELMET + 1, 1));
        assert_eq!(out(&b, &g(&["ggg", "g.g", "g.g"], &[gold])), (I_GOLDEN_HELMET + 2, 1));
        assert_eq!(out(&b, &g(&["g.g", "g.g", "..."], &[gold])), (I_GOLDEN_HELMET + 3, 1));
        assert_eq!(out(&b, &g(&["ddd", "d.d", "..."], &[dia])), (I_GOLDEN_HELMET + 4, 1));
        assert_eq!(out(&b, &g(&["d.d", "ddd", "ddd"], &[dia])), (I_GOLDEN_HELMET + 5, 1));
        assert_eq!(out(&b, &g(&["ddd", "d.d", "d.d"], &[dia])), (I_GOLDEN_HELMET + 6, 1));
        assert_eq!(out(&b, &g(&["d.d", "d.d", "..."], &[dia])), (I_GOLDEN_HELMET + 7, 1));
        // Хлеб — ряд из трёх пшениц только в сетке 3×3; в 2×2 не помещается.
        assert_eq!(out(&b, &g(&["www", "...", "..."], &[wheat])), (I_BREAD, 1));
        assert_eq!(out(&b, &g(&["...", "...", "www"], &[wheat])), (I_BREAD, 1));
        assert_eq!(out(&b, &g(&["ww", "ww"], &[wheat])).0, 0, "в 2×2 хлеба нет");
        // Стог из 9 пшеницы (только 3×3) и обратно 9 пшеницы (хватает 2×2).
        assert_eq!(out(&b, &g(&["www", "www", "www"], &[wheat])), (HAY_BLOCK as ItemId, 1));
        assert_eq!(out(&b, &g(&["ww", "ww"], &[wheat])).0, 0);
        assert_eq!(out(&b, &g(&["x.", ".."], &[('x', HAY_BLOCK as ItemId)])), (I_WHEAT, 9));
    }
    #[test]
    fn t10_no_two_recipes_share_a_grid() {
        let b = book();
        for i in 0..b.list.len() {
            for j in i + 1..b.list.len() {
                assert!(!could_both_match(&b.list[i], &b.list[j]), "{} ~ {}", b.list[i].name, b.list[j].name);
            }
        }
        assert!(could_both_match(&b.list[3], &b.list[3])); // контроль: рецепт сам с собой
    }
    #[test]
    fn t11_trim_and_symmetry() {
        let p = compile_pattern(&["   ", " # ", " # "], &[('#', Ing::Item(I_STICK))]).unwrap();
        assert_eq!((p.w, p.h, p.n), (1, 2, 2));
        let a = compile_pattern(&["XX", "X#", " #"], &[('X', Ing::Set(TAG_PLANKS)), ('#', Ing::Item(I_STICK))]).unwrap();
        assert!(!a.symmetric);
        let pk = compile_pattern(&["XXX", " # ", " # "], &[('X', Ing::Set(TAG_PLANKS)), ('#', Ing::Item(I_STICK))]).unwrap();
        assert!(pk.symmetric);
        let t = g(&["...", ".p.", ".p."], &[P]).trim().unwrap();
        assert_eq!(t, Trim { left: 1, top: 1, w: 1, h: 2, n: 2 });
        let t = g(&["p.p", "...", "..."], &[P]).trim().unwrap();
        assert_eq!(t, Trim { left: 0, top: 0, w: 3, h: 1, n: 2 }); // пустой столбец внутри рамки остаётся
    }

    fn shift_craft(b: &RecipeBook, gr: &mut Grid) -> (u32, i32, ItemStack) {
        let first = b.result(gr);
        let (mut n, mut total) = (0, 0);
        loop {
            let r = b.result(gr);
            if r.is_empty() || r.item != first.item {
                return (n, total, r);
            }
            n += 1;
            total += r.count;
            b.consume(gr, &mut |_| {});
        }
    }
    #[test]
    fn t12_shift_logs() {
        let b = book();
        let mut gr = Grid::new(2, 2);
        gr.set(1, 0, ItemStack::of(OAK, 10));
        assert_eq!(shift_craft(&b, &mut gr), (10, 40, ItemStack::EMPTY));
        assert!(gr.cells.iter().all(|c| c.is_empty()));
    }
    #[test]
    fn t13_shift_pickaxe_min_stack() {
        let b = book();
        let mut gr = g(&["ppp", ".k.", ".k."], &[P, K]);
        gr.cells[0].count = 5;
        gr.cells[1].count = 3;
        gr.cells[2].count = 4;
        gr.cells[4].count = 2;
        gr.cells[7].count = 7;
        let (n, total, after) = shift_craft(&b, &mut gr);
        assert_eq!((n, total, after), (2, 2, ItemStack::EMPTY));
        let counts: Vec<i32> = gr.cells.iter().map(|c| c.n()).collect();
        assert_eq!(&counts[..9], &[3, 1, 2, 0, 0, 0, 0, 5, 0]);
    }
    #[test]
    fn t14_shift_stops_on_other_recipe() {
        let b = book();
        let mut gr = g(&["ppp", "p.p", "ppp"], &[P]);
        gr.cells[0].count = 2;
        gr.cells[3].count = 2; // левый столбец, два верхних
        let (n, _, after) = shift_craft(&b, &mut gr);
        assert_eq!(n, 1);
        assert_eq!((after.item, after.count), (I_STICK, 4)); // в слоте результата теперь палки, цикл стоп
    }
    #[test]
    fn t15_remainder_goes_back_to_cell() {
        use Ing::*;
        let defs = vec![RecipeDef { name: "test_wet_sand", shape: RShape::Shapeless(vec![Item(I_WATER_BUCKET), Item(SAND as ItemId)]), out: CLAY as ItemId, count: 1 }];
        let b = RecipeBook::build(defs).unwrap();
        let mut gr = g(&["w.", ".s"], &[('w', I_WATER_BUCKET), ('s', SAND as ItemId)]);
        gr.cells[3].count = 5;
        assert_eq!(out(&b, &gr), (CLAY as ItemId, 1));
        let mut given = vec![];
        b.consume(&mut gr, &mut |s| given.push(s));
        assert_eq!(gr.cells[0], ItemStack::of(I_BUCKET, 1));
        assert_eq!(gr.cells[3], ItemStack::of(SAND as ItemId, 4));
        assert!(given.is_empty());
    }
    #[test]
    fn t16_reach() {
        let eye = [0.5, 64.0 + 1.62, 0.5];
        assert!(!crafting_table_in_reach(eye, [9, 65, 0], true)); // ровно 8.5 — уже закрыто (строго <)
        assert!(crafting_table_in_reach([0.5000001, 65.62, 0.5], [9, 65, 0], true));
        assert!(crafting_table_in_reach(eye, [8, 65, 0], true));
        assert!(!crafting_table_in_reach(eye, [0, 65, 0], false)); // блок сломан/заменён
        assert!(crafting_table_in_reach(eye, [0, 65, 0], true)); // глаз внутри коробки — 0
        assert!(crafting_table_in_reach([1.0, 65.5, 1.0], [7, 65, 7], true)); // 36 + 36 = 72 < 72.25
        assert!(!crafting_table_in_reach([0.97, 65.5, 1.0], [7, 65, 7], true)); // 72.36
        assert!(!crafting_table_in_reach(eye, [0, 56, 0], true)); // dy = 8.62
        assert!(crafting_table_in_reach(eye, [0, 57, 0], true)); // dy = 7.62
    }
    #[test]
    fn t17_repair() {
        // Починка (ASP-09 §2.7): деревянная кирка 59 — износ 30 и 40 → ост (29 + 19 + 2) = 50 → износ 9.
        let b = book();
        let pick = I_WOODEN_PICKAXE;
        let mut gr = Grid::new(2, 2);
        gr.cells[0] = ItemStack { item: pick, count: 1, damage: 30 };
        gr.cells[3] = ItemStack { item: pick, count: 1, damage: 40 };
        assert_eq!(b.result(&gr), ItemStack { item: pick, count: 1, damage: 9 });
        // Целые — тоже «чинятся» (результат целый); разные предметы и не-инструменты — нет.
        gr.cells[0].damage = 0;
        gr.cells[3].damage = 0;
        assert_eq!(b.result(&gr).damage, 0);
        gr.cells[3].item = I_WOODEN_PICKAXE + 5;
        assert!(b.result(&gr).is_empty());
        let gr = g(&["d.", ".d"], &[D]);
        assert!(b.result(&gr).is_empty());
        // Расход: обе кирки уходят.
        let mut gr = Grid::new(2, 2);
        gr.cells[1] = ItemStack { item: pick, count: 1, damage: 50 };
        gr.cells[2] = ItemStack { item: pick, count: 1, damage: 50 };
        assert_eq!(b.result(&gr).damage, 59 - (9 + 9 + 2));
        b.consume(&mut gr, &mut |_| {});
        assert!(gr.cells.iter().all(|c| c.is_empty()));
    }
}
