//! Текучие жидкости (ASP-15, решения после ASP-15 в mc/decisions.md): вода и лава — блоки WATER и LAVA с уровнем в битах
//! 0–3 состояния, как FluidBlock.LEVEL: 0 — источник, 1…7 — течёт (количество жидкости 8 − L), 8 — падающая. Генератор
//! пишет 0: море — неподвижные источники, при загрузке и генерации тики не ставятся (проверка — fluid_tests).
//!
//! Тики жидкостей идут в общей очереди тиков блоков (world/ticks.rs; ключ — клетка и блок, вода и лава разные; дубль не
//! ставится, срабатывают по (время, порядок) в активной зоне): через 5 тиков у воды, 30 у лавы. Очередь сохраняется
//! вместе с миром — разлив переживает перезапуск сервера. Тик ставится правилом «запись, изменившая ячейку, будит саму
//! клетку (если жидкость) и шесть соседей-жидкостей в порядке W, E, D, U, N, S» (Sim::set_world → fluid_wake). Лава рядом с
//! водой (сверху или сбоку) твердеет сразу: источник — обсидиан, течёт — булыжник (FluidBlock.receiveNeighborFluids).
//!
//! Правила растекания — FlowableFluid (ASP-15 §2): `fluid_next` (getUpdatedState: максимум соседей − спад, бесконечный
//! источник при двух источниках по бокам и твёрдом дне, падающая под жидкостью), `fluid_try_flow` (сначала вниз, вбок —
//! если вниз нельзя или клетка — источник), `fluid_spread` (вбок только к ближайшей «яме» в пределах 5 шагов у воды, 3 у
//! лавы; нет ям — во все стороны). Текучая вода смывает растения и факелы с дропом без звука, лава жжёт без дропа. Лава
//! вниз в воду — камень. Высота поверхности клетки — `fluid_height` (физика игрока, предметов; меш пока рисует любую
//! воду на 8/9 — вид не входит в G7).

use std::collections::HashMap;

use crate::cell::{block_of, cell_of, offset, state_of, BlockId, Cell, State, F_DOWN, F_EAST, F_NORTH, F_SOUTH, F_UP, F_WEST, H};
use crate::game::interact::protected;
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::blocks::{bdef, collision_of, Shape, FLUID_LAVA, FLUID_WATER};
use crate::registry::ids::{AIR, COBBLE, LAVA, OBSIDIAN, STONE, WATER};
use crate::world::WorldData;

/// Биты состояния блока жидкости: уровень.
pub(crate) const LEVEL_MASK: State = 0x0F;

/// Род жидкости.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Liquid {
    Water,
    Lava,
}

impl Liquid {
    /// Блок рода.
    pub(crate) fn block(self) -> BlockId {
        match self {
            Liquid::Water => WATER,
            Liquid::Lava => LAVA,
        }
    }
    /// Задержка тика (getTickRate): вода 5, лава 30 (обычный мир).
    pub(crate) fn tick_delay(self) -> u32 {
        match self {
            Liquid::Water => 5,
            Liquid::Lava => 30,
        }
    }
    /// Спад количества на блок (getLevelDecreasePerBlock): вода 1, лава 2.
    fn drop_per_block(self) -> u8 {
        match self {
            Liquid::Water => 1,
            Liquid::Lava => 2,
        }
    }
    /// Глубина поиска ямы (getMaxFlowDistance): вода 4 (путь до 5 клеток), лава 2.
    fn search(self) -> u8 {
        match self {
            Liquid::Water => 4,
            Liquid::Lava => 2,
        }
    }
    /// Бесконечный источник (правило мира waterSourceConversion; лава — нет).
    fn infinite(self) -> bool {
        self == Liquid::Water
    }
}

/// Состояние жидкости в клетке (FluidState).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Fl {
    pub(crate) liquid: Liquid,
    /// Количество 1…8: источник и падающая — 8.
    pub(crate) amount: u8,
    /// Источник (неподвижная жидкость).
    pub(crate) still: bool,
    pub(crate) falling: bool,
}

impl Fl {
    pub(crate) fn source(liquid: Liquid) -> Fl {
        Fl { liquid, amount: 8, still: true, falling: false }
    }
    pub(crate) fn falling(liquid: Liquid) -> Fl {
        Fl { liquid, amount: 8, still: false, falling: true }
    }
    pub(crate) fn flowing(liquid: Liquid, amount: u8) -> Fl {
        Fl { liquid, amount, still: false, falling: false }
    }
    /// Уровень блока (FluidBlock.LEVEL): 0 источник, 1…7 течёт, 8 падает.
    pub(crate) fn level(self) -> State {
        if self.still {
            0
        } else if self.falling {
            8
        } else {
            (8 - self.amount) as State
        }
    }
    /// Ячейка мира.
    pub(crate) fn cell(self) -> Cell {
        cell_of(self.liquid.block(), self.level())
    }
    /// Высота поверхности без учёта соседа сверху (FluidState.getHeight): количество / 9 во float.
    pub(crate) fn height(self) -> f64 {
        (self.amount as f32 / 9.0f32) as f64
    }
}

/// Жидкость в ячейке (FluidBlock.getFluidState): None — не жидкость.
pub(crate) fn fluid_of(c: Cell) -> Option<Fl> {
    let liquid = match bdef(block_of(c)).fluid {
        FLUID_WATER => Liquid::Water,
        FLUID_LAVA => Liquid::Lava,
        _ => return None,
    };
    Some(match state_of(c) & LEVEL_MASK {
        0 => Fl::source(liquid),
        l @ 1..=7 => Fl::flowing(liquid, 8 - l as u8),
        _ => Fl::falling(liquid),
    })
}

/// Высота жидкости в клетке для физики и лучей (FlowableFluid.getHeight): под такой же жидкостью — 1.0, иначе
/// количество / 9 (источник 8/9 = 0.8889). Род — по блоку (физика различает воду и лаву по `BlockDef.fluid`).
pub(crate) fn fluid_height(c: Cell, above: Cell) -> f64 {
    let Some(f) = fluid_of(c) else { return 0.0 };
    if fluid_of(above).is_some_and(|g| g.liquid == f.liquid) {
        1.0
    } else {
        f.height()
    }
}

/// Вектор течения жидкости клетки p (FlowableFluid.getVelocity, ASP-15 §7): от высокой поверхности к низкой по 4 соседям;
/// падающая клетка у сплошной стены тянет вниз; результат нормирован (ноль — покой). Нужен толчку игрока (ASP-03).
pub(crate) fn fluid_velocity(w: &mut WorldData, p: [i32; 3]) -> [f64; 3] {
    let Some(me) = fluid_of(w.get_cell(p[0], p[1], p[2])) else { return [0.0; 3] };
    let same = |g: Option<Fl>| g.map_or(true, |g| g.liquid == me.liquid);
    let (mut vx, mut vz) = (0.0f32, 0.0f32);
    for d in HORIZ {
        let q = offset(p, d);
        let n = fluid_of(w.get_cell(q[0], q[1], q[2]));
        if !same(n) {
            continue;
        }
        let mut f = n.map_or(0.0f32, |g| g.amount as f32 / 9.0);
        let mut g = 0.0f32;
        if f == 0.0 {
            if !is_solid(w.get_cell(q[0], q[1], q[2])) {
                let below = fluid_of(w.get_cell(q[0], q[1] - 1, q[2]));
                if same(below) {
                    f = below.map_or(0.0, |g| g.amount as f32 / 9.0);
                    if f > 0.0 {
                        g = me.amount as f32 / 9.0 - (f - 0.8888889f32);
                    }
                }
            }
        } else {
            g = me.amount as f32 / 9.0 - f;
        }
        if g == 0.0 {
            continue;
        }
        let dir = crate::cell::DIRS[d as usize];
        vx += dir[0] as f32 * g;
        vz += dir[2] as f32 * g;
    }
    let mut v = [vx as f64, 0.0, vz as f64];
    if me.falling {
        for d in HORIZ {
            let q = offset(p, d);
            let blocked = |w: &mut WorldData, q: [i32; 3]| crate::registry::blocks::face_full(w.get_cell(q[0], q[1], q[2]), d ^ 1);
            if blocked(w, q) || blocked(w, [q[0], q[1] + 1, q[2]]) {
                let l = (v[0] * v[0] + v[2] * v[2]).sqrt();
                if l > 0.0 {
                    v = [v[0] / l, 0.0, v[2] / l];
                }
                v[1] = -6.0;
                break;
            }
        }
    }
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 0.0 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0; 3]
    }
}

/// «Твёрдый» блок по устаревшему флагу isSolid (AbstractBlockState.shouldBeSolid): средняя сторона коробки коллизии не
/// меньше 35/48 или высота не меньше 1. Кактус, сундук, лестница, пашня — да; факел, растения, кабель, снежный слой — нет.
pub(crate) fn is_solid(c: Cell) -> bool {
    let boxes = collision_of(c);
    if boxes.is_empty() {
        return false;
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for s in boxes {
        for k in 0..3 {
            lo[k] = lo[k].min(s.lo[k]);
            hi[k] = hi[k].max(s.hi[k]);
        }
    }
    let len = |k: usize| hi[k] - lo[k];
    (len(0) + len(1) + len(2)) / 3.0 >= 35.0 / 48.0 - 1e-9 || len(1) >= 1.0 - 1e-9
}

/// Можно ли жидкости занять клетку (FlowableFluid.canFill): не твёрдый блок; кабель Mekanism не смывается.
fn fillable(c: Cell) -> bool {
    !is_solid(c) && !matches!(bdef(block_of(c)).shape, Shape::Cable | Shape::Large)
}

/// Кэш одного поиска ямы: клетка → (жидкость пройдёт сквозь неё, под ней яма).
type SpreadCache = HashMap<[i32; 3], (bool, bool)>;

const HORIZ: [u8; 4] = [F_NORTH, F_EAST, F_SOUTH, F_WEST];
/// Порядок записи вбок — перебор EnumMap Direction: N, S, W, E.
const WRITE_ORDER: [u8; 4] = [F_NORTH, F_SOUTH, F_WEST, F_EAST];
/// Порядок будения соседей (NeighborUpdater): W, E, D, U, N, S.
const WAKE_ORDER: [u8; 6] = [F_WEST, F_EAST, F_DOWN, F_UP, F_NORTH, F_SOUTH];

impl Sim {
    // ------------------------------------------------------------ будение

    /// После записи ячейки new в pos (Sim::set_world): новая жидкость ставит себе тик (onBlockAdded), соседи-жидкости —
    /// тоже (neighborUpdate); лава у воды твердеет вместо тика.
    pub(crate) fn fluid_wake(&mut self, w: &mut WorldData, pos: [i32; 3], new: Cell, ev: &mut Events) {
        if let Some(f) = fluid_of(new) {
            self.fluid_notice(w, pos, f, ev);
        }
        for d in WAKE_ORDER {
            let q = offset(pos, d);
            if q[1] < 0 || q[1] >= H {
                continue;
            }
            if let Some(f) = fluid_of(w.get_cell(q[0], q[1], q[2])) {
                self.fluid_notice(w, q, f, ev);
            }
        }
    }

    fn fluid_notice(&mut self, w: &mut WorldData, p: [i32; 3], f: Fl, ev: &mut Events) {
        if f.liquid == Liquid::Lava && self.lava_meets_water(w, p, f, ev) {
            return;
        }
        self.ticks.schedule(self.time, p, f.liquid.block(), f.liquid.tick_delay());
    }

    /// FluidBlock.receiveNeighborFluids: вода сверху или сбоку от лавы — источник становится обсидианом, течёт — булыжником.
    fn lava_meets_water(&mut self, w: &mut WorldData, p: [i32; 3], f: Fl, ev: &mut Events) -> bool {
        for d in [F_UP, F_NORTH, F_SOUTH, F_WEST, F_EAST] {
            let q = offset(p, d);
            if fluid_of(w.get_cell(q[0], q[1], q[2])).is_some_and(|g| g.liquid == Liquid::Water) {
                let b = if f.still { OBSIDIAN } else { COBBLE };
                self.set_world(w, p, b as Cell, ev);
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------ тик

    /// Запланированный тик жидкости в клетке p (FlowableFluid.onScheduledTick).
    pub(crate) fn fluid_tick(&mut self, w: &mut WorldData, p: [i32; 3], ev: &mut Events) {
        let Some(mut f) = fluid_of(w.get_cell(p[0], p[1], p[2])) else { return };
        if !f.still {
            match self.fluid_next(w, f.liquid, p) {
                None => {
                    self.set_world(w, p, AIR as Cell, ev);
                    return;
                }
                Some(n) if n != f => {
                    f = n;
                    self.set_world(w, p, n.cell(), ev);
                }
                _ => {}
            }
        }
        self.fluid_try_flow(w, p, f, ev);
    }

    fn fl_at(w: &mut WorldData, p: [i32; 3]) -> Option<Fl> {
        if p[1] < 0 || p[1] >= H {
            return None;
        }
        fluid_of(w.get_cell(p[0], p[1], p[2]))
    }

    /// Каким станет состояние жидкости l в клетке p (getUpdatedState): None — пусто. Считается по соседям, не по самой клетке.
    pub(crate) fn fluid_next(&mut self, w: &mut WorldData, l: Liquid, p: [i32; 3]) -> Option<Fl> {
        let (mut best, mut sources) = (0u8, 0);
        for d in HORIZ {
            if let Some(g) = Self::fl_at(w, offset(p, d)).filter(|g| g.liquid == l) {
                sources += g.still as u8;
                best = best.max(g.amount);
            }
        }
        if l.infinite() && sources >= 2 {
            let b = w.get_cell(p[0], p[1] - 1, p[2]);
            if is_solid(b) || fluid_of(b).is_some_and(|g| g.liquid == l && g.still) {
                return Some(Fl::source(l));
            }
        }
        if Self::fl_at(w, offset(p, F_UP)).is_some_and(|g| g.liquid == l) {
            return Some(Fl::falling(l));
        }
        let a = best as i32 - l.drop_per_block() as i32;
        (a > 0).then(|| Fl::flowing(l, a as u8))
    }

    /// Можно ли записать жидкость l в клетку q, идя по направлению dir (canFlow): прежняя жидкость там заменяема (воду —
    /// только сверху вниз и не водой; лаву — водой при высоте не меньше 4/9), клетка заполнима, не защищена.
    fn fluid_can_enter(&mut self, w: &mut WorldData, l: Liquid, q: [i32; 3], dir: u8) -> bool {
        if q[1] < 0 || q[1] >= H {
            return false;
        }
        let c = w.get_cell(q[0], q[1], q[2]);
        let replace_ok = match fluid_of(c) {
            None => true,
            Some(g) if g.liquid == Liquid::Water => dir == F_DOWN && l != Liquid::Water,
            Some(g) => g.amount >= 4 && l == Liquid::Water,
        };
        replace_ok && fillable(c) && !protected(w, q)
    }

    /// Пройдёт ли жидкость l в клетку q при поиске пути (canFlowThrough): там не источник той же жидкости, заполнимо.
    fn fluid_through(w: &mut WorldData, l: Liquid, q: [i32; 3], cache: &mut SpreadCache) -> bool {
        if let Some(&(t, _)) = cache.get(&q) {
            return t;
        }
        let c = w.get_cell(q[0], q[1], q[2]);
        let t = !fluid_of(c).is_some_and(|g| g.liquid == l && g.still) && fillable(c);
        let hole = t && Self::fluid_hole(w, l, q);
        cache.insert(q, (t, hole));
        t
    }

    /// Под клеткой at можно течь (canFlowDownTo): внизу та же жидкость или заполнимое.
    fn fluid_hole(w: &mut WorldData, l: Liquid, at: [i32; 3]) -> bool {
        if at[1] <= 0 {
            return false;
        }
        let b = w.get_cell(at[0], at[1] - 1, at[2]);
        fluid_of(b).is_some_and(|g| g.liquid == l) || fillable(b)
    }

    /// Текучая жидкость вниз, потом вбок (FlowableFluid.tryFlow).
    fn fluid_try_flow(&mut self, w: &mut WorldData, p: [i32; 3], f: Fl, ev: &mut Events) {
        let dn = [p[0], p[1] - 1, p[2]];
        if dn[1] >= 0 {
            if let Some(n) = self.fluid_next(w, f.liquid, dn) {
                if self.fluid_can_enter(w, f.liquid, dn, F_DOWN) {
                    self.fluid_flow_into(w, f.liquid, dn, F_DOWN, n, ev);
                    if self.fluid_sources_around(w, f.liquid, p) >= 3 {
                        self.fluid_flow_sides(w, p, f, ev);
                    }
                    return;
                }
            }
        }
        // Вниз нельзя: вбок — источник всегда, текущая — только если под ней не «яма» (иначе ручей сливается с прудом).
        if f.still || !(dn[1] >= 0 && Self::fluid_hole(w, f.liquid, p)) {
            self.fluid_flow_sides(w, p, f, ev);
        }
    }

    fn fluid_sources_around(&mut self, w: &mut WorldData, l: Liquid, p: [i32; 3]) -> u8 {
        HORIZ.iter().filter(|&&d| Self::fl_at(w, offset(p, d)).is_some_and(|g| g.liquid == l && g.still)).count() as u8
    }

    /// Вбок (flowToSides): к ближайшим ямам, иначе во все стороны.
    fn fluid_flow_sides(&mut self, w: &mut WorldData, p: [i32; 3], f: Fl, ev: &mut Events) {
        let i = if f.falling { 7 } else { f.amount as i32 - f.liquid.drop_per_block() as i32 };
        if i <= 0 {
            return;
        }
        let spread = self.fluid_spread(w, f.liquid, p);
        for d in WRITE_ORDER {
            let Some(&(_, n)) = spread.iter().find(|e| e.0 == d) else { continue };
            let q = offset(p, d);
            if self.fluid_can_enter(w, f.liquid, q, d) {
                self.fluid_flow_into(w, f.liquid, q, d, n, ev);
            }
        }
    }

    /// getSpread: стороны с наименьшим расстоянием до ямы и состояние жидкости в них.
    fn fluid_spread(&mut self, w: &mut WorldData, l: Liquid, p: [i32; 3]) -> Vec<(u8, Fl)> {
        let mut cache = SpreadCache::new();
        let (mut best, mut out) = (1000, Vec::new());
        for d in HORIZ {
            let q = offset(p, d);
            if !Self::fluid_through(w, l, q, &mut cache) {
                continue;
            }
            let Some(n) = self.fluid_next(w, l, q) else { continue };
            let dist = if cache[&q].1 { 0 } else { Self::fluid_down_dist(w, l, q, 1, d ^ 1, &mut cache) };
            if dist < best {
                out.clear();
                best = dist;
            }
            if dist <= best {
                out.push((d, n));
            }
        }
        out
    }

    /// getMinFlowDownDistance: сколько шагов от p до клетки над ямой (не больше глубины поиска), 1000 — нет.
    fn fluid_down_dist(w: &mut WorldData, l: Liquid, p: [i32; 3], i: u8, came: u8, cache: &mut SpreadCache) -> i32 {
        let mut best = 1000;
        for d in HORIZ {
            if d == came {
                continue;
            }
            let q = offset(p, d);
            if !Self::fluid_through(w, l, q, cache) {
                continue;
            }
            if cache[&q].1 {
                return i as i32;
            }
            if i >= l.search() {
                continue;
            }
            best = best.min(Self::fluid_down_dist(w, l, q, i + 1, d ^ 1, cache));
        }
        best
    }

    /// FlowableFluid.flow: записать состояние n в клетку q. Лава вниз в воду — камень; вода смывает занятую клетку с
    /// дропом без звука, лава — без дропа.
    fn fluid_flow_into(&mut self, w: &mut WorldData, l: Liquid, q: [i32; 3], dir: u8, n: Fl, ev: &mut Events) {
        let cur = w.get_cell(q[0], q[1], q[2]);
        if l == Liquid::Lava && dir == F_DOWN && fluid_of(cur).is_some_and(|g| g.liquid == Liquid::Water) {
            self.set_world(w, q, STONE as Cell, ev);
            return;
        }
        if block_of(cur) != AIR && l == Liquid::Water {
            self.drop_loot(cur, q, false);
        }
        self.set_world(w, q, n.cell(), ev);
    }
}
