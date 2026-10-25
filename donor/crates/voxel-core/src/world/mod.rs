//! Изменяемый мир (E1, E2, ячейка E5 — u32, cell.rs): правки поверх генератора, запрос блока, куски на перестройку.
//! Луч по обводкам — game/interact.rs (ASP-04), меш куска из кэша — mesh (impl World там же).
//!
//! Мир = генератор(сид) + правки. Правки лежат по кускам: (cx, cz) → (локальная позиция → ячейка).
//! Главный поток держит кэш областей (кусок с рамкой в 1 блок и всеми правками, как для сборки меша): запрос блока
//! за микросекунды и перестройка куска под рукой без генерации. Воркеры правок не хранят: получают пачку правок
//! группы с рамкой (tile_edits, ячейки целиком — с состоянием) вместе с заданием и накладывают её на область.
//! Дальние плитки (LOD) правки не учитывают: они строятся по столбцам генератора, а правки бывают только вблизи.
//! Состояния лежат в области параллельным массивом (Region.states, заводится, только если у области есть правки
//! с состоянием) — их читают меш (ориентации) и запрос ячейки.
//! Блок-сущности (печь, сундук — blockentity.rs) живут здесь же, рядом с правками: создаются и убираются в set_cell.
//! Поля мира закрыты: правки, кэш, индекс моделей и убранные блок-сущности — только через методы ниже.
//!
//! Правка, совпавшая с генератором, удаляется сразу при записи (Ф2, решение после ASP-15): у правки хранится ячейка
//! генератора под ней (base), и запись base обратно убирает правку, а последняя правка куска — сам кусок из правок.
//! Так следы разлива, опада и «сломал и поставил назад» не копятся ни в памяти, ни в пачках воркерам и сети, ни в
//! сохранении. Ячейку генератора даёт сама запись: до первой правки клетки кэш области и есть генератор. Правки из
//! хранилища (load_chunk) приходят без base — его досчитывает первая загрузка области куска (генерация там и так идёт).
//! Куски, где менялись правки или блок-сущности, копятся для сохранения (take_unsaved).
//!
//! Сеть (MP-0): у сервера и у клиента — по своему миру. Мир сервера ведёт журнал записей (enable_log / take_log): из
//! него собираются живые правки кадра (proto Msg::Blocks). Мир клиента — зеркало: правки с сервера пишутся сырой
//! записью set_cell. Отладочная сырая запись в ручку World (JS: tiles.edit, ?edit=; проверки) у локального сервера
//! уходит и в его мир (World::set_mirror) — как команда отладки, только сразу; у сетевого клиента зеркала нет.

pub(crate) mod blockentity;
pub(crate) mod grid;
pub(crate) mod logistics;
pub(crate) mod mek;
pub(crate) mod stack;
pub(crate) mod ticks;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::cell::{block_of, state_of, BlockId, Cell, CS, H};
use crate::gen::{biome_name, generate, Gen, Region, PAD};
use crate::registry::blocks::{bdef, is_model};
use crate::registry::ids::AIR;
use blockentity::{BlockEntities, BlockEntity};
use stack::{Container, ItemStack};

/// Областей в кэше главного потока (~42 КБ каждая: младшие байты номеров блоков и оттенки; старшие байты и состояния —
/// лениво): ~5,4 МБ — активная зона 9×9 кусков (E13) и запас (≥ 121).
const CACHE_CAP: usize = 128;

/// Клеток в куске: локальные ключи 0…CHUNK_CELLS − 1.
pub(crate) const CHUNK_CELLS: u16 = (CS * CS * H) as u16;

/// Позиция внутри куска: (lz·16 + lx)·128 + y.
#[inline]
pub(crate) fn local_key(lx: i32, y: i32, lz: i32) -> u16 {
    ((lz * CS + lx) * H + y) as u16
}
/// Позиция внутри куска по ключу: (lx, y, lz).
#[inline]
pub(crate) fn local_pos(k: u16) -> (i32, i32, i32) {
    let k = k as i32;
    ((k / H) % CS, k % H, k / (H * CS))
}
/// Кусок клетки (x, z).
#[inline]
pub(crate) fn chunk_of(x: i32, z: i32) -> (i32, i32) {
    (x.div_euclid(CS), z.div_euclid(CS))
}

/// Ячейка генератора ещё неизвестна (правка из хранилища до первой загрузки области куска).
const BASE_UNKNOWN: Cell = Cell::MAX;

/// Правка клетки: что стоит и что там было бы у генератора.
#[derive(Clone, Copy)]
struct Edit {
    cell: Cell,
    base: Cell,
}

/// Область куска в кэше главного потока.
struct Cached {
    reg: Region,
    used: u64,
}

impl Cached {
    #[inline]
    fn cell(&self, i: usize) -> Cell {
        self.reg.cell(i)
    }
}

pub(crate) struct WorldData {
    gen: Gen,
    /// Правки по кускам; куска без правок здесь нет (has_edits).
    edits: HashMap<(i32, i32), HashMap<u16, Edit>>,
    cache: HashMap<(i32, i32), Cached>,
    clock: u64,
    edit_count: u32,
    /// Куски, чьи меши изменились с прошлого take_dirty (правки из тиков игры и из JS) — без повторов.
    dirty: Vec<(i32, i32)>,
    /// Куски, где менялись правки, с прошлого take_unsaved (сохранение; блок-сущности ведут свой список).
    unsaved: HashSet<(i32, i32)>,
    /// Блок-сущности (печи, сундуки).
    be: BlockEntities,
    /// Клетки блоков со своей моделью (сундук, лестница, настенный факел, снежный слой) по кускам — их рисует проход
    /// моделей (Game::model_draws); генератор их не ставит, поэтому индекс ведут правки.
    models: HashMap<(i32, i32), Vec<u16>>,
    /// Журнал записей (мир сервера): (клетка, новая ячейка) по порядку — для кадров клиентам.
    log: Option<Vec<([i32; 3], Cell)>>,
    /// Энергосети (world/grid.rs): ведёт только мир сервера (grid.on); правки клеток — через set_cell.
    pub(crate) grid: grid::Grid,
    /// Предметы в пути по транспортёрам (world/logistics.rs, M5): мир сервера.
    pub(crate) logistics: logistics::Logistics,
    /// Источники жидкости, выкачанные насосом в тике машины: Sim запишет воздух дорогой игры.
    pub(crate) drained: Vec<[i32; 3]>,
}

impl WorldData {
    pub(crate) fn new(seed: u32) -> Self {
        WorldData {
            gen: Gen::new(seed),
            edits: HashMap::new(),
            cache: HashMap::new(),
            clock: 0,
            edit_count: 0,
            dirty: Vec::new(),
            unsaved: HashSet::new(),
            be: BlockEntities::default(),
            models: HashMap::new(),
            log: None,
            grid: grid::Grid::default(),
            logistics: logistics::Logistics::default(),
            drained: Vec::new(),
        }
    }

    /// Вести журнал записей (мир сервера).
    pub(crate) fn enable_log(&mut self) {
        self.log.get_or_insert_with(Vec::new);
    }
    /// Забрать журнал записей с прошлого вызова.
    pub(crate) fn take_log(&mut self) -> Vec<([i32; 3], Cell)> {
        self.log.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Забыть правки и блок-сущности куска (клиент: сервер прислал кусок заново или он ушёл из радиуса): кусок снова
    /// как у генератора; кэш куска и соседей (их рамки) сброшен, меши — на перестройку.
    pub(crate) fn forget_chunk(&mut self, cx: i32, cz: i32) {
        self.edits.remove(&(cx, cz));
        self.models.remove(&(cx, cz));
        self.be.forget_chunk(cx, cz);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let k = (cx + dx, cz + dz);
                self.cache.remove(&k);
                if !self.dirty.contains(&k) {
                    self.dirty.push(k);
                }
            }
        }
    }

    /// Копия правок и блок-сущностей другого мира (локальный сервер поднимает мир из того, что уже лежит в мире
    /// браузера или проверки): куски целиком, как из хранилища.
    pub(crate) fn copy_edits_from(&mut self, src: &WorldData) {
        let mut bes: HashMap<(i32, i32), Vec<([i32; 3], u64, BlockEntity)>> = HashMap::new();
        for (p, seq, be) in src.be.iter() {
            bes.entry(chunk_of(p[0], p[2])).or_default().push((p, seq, be.clone()));
        }
        let mut chunks: Vec<(i32, i32)> = src.edits.keys().copied().chain(bes.keys().copied()).collect();
        chunks.sort_unstable();
        chunks.dedup();
        for c in chunks {
            self.load_chunk(c.0, c.1, &src.chunk_edits(c.0, c.1), bes.remove(&c).unwrap_or_default());
        }
        self.be.set_next_seq(src.be.next_seq());
        self.edit_count = self.edit_count.max(src.edit_count);
        self.take_unsaved();
    }

    /// Генератор мира (сид, столбцы).
    pub(crate) fn gen(&self) -> &Gen {
        &self.gen
    }
    /// Блок-сущности: чтение и правка содержимого (заводит и убирает их только set_cell).
    pub(crate) fn be(&self) -> &BlockEntities {
        &self.be
    }
    pub(crate) fn be_mut(&mut self) -> &mut BlockEntities {
        &mut self.be
    }
    /// Забрать блок-сущности, убранные правками (блок стал другим), — их содержимое рассыпает игра.
    pub(crate) fn take_removed(&mut self) -> Vec<([i32; 3], BlockEntity)> {
        self.be.take_removed()
    }
    /// Сколько правок сделано (для отладки и отчёта; JS по нему решает, слать ли воркерам пачку правок).
    pub(crate) fn edit_count(&self) -> u32 {
        self.edit_count
    }
    /// Счётчик правок из сохранения.
    pub(crate) fn set_edit_count(&mut self, n: u32) {
        self.edit_count = n;
    }

    // ---------------------------------------------------------------- сохранение (save/)

    /// Куски с правками.
    pub(crate) fn edited_chunks(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.edits.keys().copied()
    }
    /// Правки куска по возрастанию ключа: (локальный ключ, ячейка).
    pub(crate) fn chunk_edits(&self, cx: i32, cz: i32) -> Vec<(u16, Cell)> {
        let Some(m) = self.edits.get(&(cx, cz)) else { return Vec::new() };
        let mut v: Vec<(u16, Cell)> = m.iter().map(|(&k, e)| (k, e.cell)).collect();
        v.sort_unstable_by_key(|e| e.0);
        v
    }
    /// Сколько правок лежит сейчас (после удаления совпавших с генератором).
    pub(crate) fn edits_len(&self) -> usize {
        self.edits.values().map(|m| m.len()).sum()
    }
    /// Забрать куски, где с прошлого вызова менялись правки или блок-сущности (без повторов).
    pub(crate) fn take_unsaved(&mut self) -> HashSet<(i32, i32)> {
        let mut s = std::mem::take(&mut self.unsaved);
        s.extend(self.be.take_touched());
        s
    }
    /// Положить кусок из хранилища: правки (ключи вне куска пропускаются) и блок-сущности (позиция, номер появления,
    /// сущность) — без правил игры и без проверки «совпало с генератором» (её сделает загрузка области: правки приходят
    /// без ячейки генератора). Прежние правки куска заменяются; кэш куска и соседей (их рамки) сбрасывается, меши —
    /// на перестройку. Для сохранения кусок не грязный: он и есть то, что лежит в хранилище.
    pub(crate) fn load_chunk(&mut self, cx: i32, cz: i32, edits: &[(u16, Cell)], bes: Vec<([i32; 3], u64, BlockEntity)>) {
        let m: HashMap<u16, Edit> =
            edits.iter().filter(|e| e.0 < CHUNK_CELLS).map(|&(k, cell)| (k, Edit { cell, base: BASE_UNKNOWN })).collect();
        let mut list: Vec<u16> = m.iter().filter(|(_, e)| is_model(block_of(e.cell))).map(|(&k, _)| k).collect();
        list.sort_unstable();
        if m.is_empty() {
            self.edits.remove(&(cx, cz));
        } else {
            self.edits.insert((cx, cz), m);
        }
        if list.is_empty() {
            self.models.remove(&(cx, cz));
        } else {
            self.models.insert((cx, cz), list);
        }
        for (p, seq, be) in bes {
            if chunk_of(p[0], p[2]) == (cx, cz) && p[1] >= 0 && p[1] < H {
                self.be.insert_loaded(p, seq, be);
            }
        }
        for dz in -1..=1 {
            for dx in -1..=1 {
                let k = (cx + dx, cz + dz);
                self.cache.remove(&k);
                if !self.dirty.contains(&k) {
                    self.dirty.push(k);
                }
            }
        }
    }

    /// Ячейки генератора для правок куска из хранилища (base неизвестен) — по свежей области генератора; совпавшие с
    /// ним правки (старое сохранение) убираются.
    fn fill_bases(&mut self, cx: i32, cz: i32, reg: &Region) {
        let Some(m) = self.edits.get_mut(&(cx, cz)) else { return };
        let mut stale = false;
        for (&k, e) in m.iter_mut() {
            if e.base == BASE_UNKNOWN {
                let (lx, y, lz) = local_pos(k);
                e.base = reg.cell(Region::idx(lx + PAD, y, lz + PAD));
                stale |= e.base == e.cell;
            }
        }
        if stale {
            m.retain(|_, e| e.cell != e.base);
            if m.is_empty() {
                self.edits.remove(&(cx, cz));
            }
        }
    }

    /// Забрать куски на перестройку: [cx, cz, …].
    pub(crate) fn take_dirty(&mut self) -> Vec<i32> {
        let out = self.dirty.iter().flat_map(|&(x, z)| [x, z]).collect();
        self.dirty.clear();
        out
    }
    /// Есть ли куски на перестройку.
    pub(crate) fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Есть ли у куска правки.
    pub(crate) fn has_edits(&self, cx: i32, cz: i32) -> bool {
        self.edits.contains_key(&(cx, cz))
    }
    /// Ячейка из правок (без генерации и кэша): None — клетку не правили. Случайные тики и показ смотрят только сюда:
    /// блоки с тиком и показом бывают только в правках.
    #[inline]
    pub(crate) fn edited_cell(&self, x: i32, y: i32, z: i32) -> Option<Cell> {
        if y < 0 || y >= H {
            return None;
        }
        let key = local_key(x.rem_euclid(CS), y, z.rem_euclid(CS));
        self.edits.get(&(x.div_euclid(CS), z.div_euclid(CS)))?.get(&key).map(|e| e.cell)
    }
    /// Клетки моделей (по индексу моделей) в кусках не дальше r от куска (cx, cz): (клетка, ячейка).
    pub(crate) fn models_near(&self, cx: i32, cz: i32, r: i32) -> impl Iterator<Item = ([i32; 3], Cell)> + '_ {
        self.models.iter().filter(move |(&(x, z), _)| (x - cx).abs() <= r && (z - cz).abs() <= r).flat_map(move |(&(x, z), list)| {
            let m = self.edits.get(&(x, z));
            list.iter().filter_map(move |&k| {
                let c = m?.get(&k)?.cell;
                let (lx, y, lz) = local_pos(k);
                Some(([x * CS + lx, y, z * CS + lz], c))
            })
        })
    }

    /// Правки кусков cx0..=cx1 × cz0..=cz1, попавшие в прямоугольник блоков [x0, x1] × [z0, z1], — пачкой
    /// [x, y, z, ячейка, …] (ячейка u32 — в i32 как есть); второй ответ — есть ли среди них ячейки с состоянием.
    #[allow(clippy::too_many_arguments)]
    fn edits_in(&self, cx0: i32, cz0: i32, cx1: i32, cz1: i32, x0: i32, z0: i32, x1: i32, z1: i32, out: &mut Vec<i32>) -> bool {
        let mut stated = false;
        if self.edits.is_empty() {
            return stated;
        }
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let Some(m) = self.edits.get(&(cx, cz)) else { continue };
                for (&k, e) in m {
                    let c = e.cell;
                    let (lx, y, lz) = local_pos(k);
                    let (x, z) = (cx * CS + lx, cz * CS + lz);
                    if x >= x0 && x <= x1 && z >= z0 && z <= z1 {
                        out.extend_from_slice(&[x, y, z, c as i32]);
                        stated |= state_of(c) != 0;
                    }
                }
            }
        }
        stated
    }

    /// Пачка правок для плитки L0 из n × n кусков: сами куски и рамка в 1 блок (грани и тени у края).
    pub(crate) fn tile_edits(&self, tx: i32, tz: i32, n: i32) -> Vec<i32> {
        let mut out = Vec::new();
        let (cx0, cz0) = (tx * n, tz * n);
        self.edits_in(cx0 - 1, cz0 - 1, cx0 + n, cz0 + n, cx0 * CS - PAD, cz0 * CS - PAD, (cx0 + n) * CS, (cz0 + n) * CS, &mut out);
        out
    }

    /// Убедиться, что область куска в кэше (генерация и правки при промахе; вытесняется самая давняя).
    fn load(&mut self, cx: i32, cz: i32) -> &mut Cached {
        self.clock += 1;
        let clock = self.clock;
        if !self.cache.contains_key(&(cx, cz)) {
            if self.cache.len() >= CACHE_CAP {
                if let Some(&old) = self.cache.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| k) {
                    self.cache.remove(&old);
                }
            }
            let mut reg = generate(&self.gen, cx, cz);
            self.fill_bases(cx, cz, &reg);
            let mut e = Vec::new();
            self.edits_in(cx - 1, cz - 1, cx + 1, cz + 1, cx * CS - PAD, cz * CS - PAD, cx * CS + CS, cz * CS + CS, &mut e);
            reg.apply_edits(&e);
            self.cache.insert((cx, cz), Cached { reg, used: clock });
        }
        let c = self.cache.get_mut(&(cx, cz)).unwrap();
        c.used = clock;
        c
    }

    /// Область куска с правками из кэша (меш куска под рукой).
    pub(crate) fn region(&mut self, cx: i32, cz: i32) -> &Region {
        &self.load(cx, cz).reg
    }

    /// Ячейка в точке: номер блока | состояние. Вне мира по высоте — воздух.
    pub(crate) fn get_cell(&mut self, x: i32, y: i32, z: i32) -> Cell {
        if y < 0 || y >= H {
            return AIR as Cell;
        }
        let (cx, cz) = (x.div_euclid(CS), z.div_euclid(CS));
        let i = Region::idx(x.rem_euclid(CS) + PAD, y, z.rem_euclid(CS) + PAD);
        self.clock += 1;
        if let Some(c) = self.cache.get_mut(&(cx, cz)) {
            c.used = self.clock;
            return c.cell(i);
        }
        self.load(cx, cz).cell(i)
    }

    pub(crate) fn get_block(&mut self, x: i32, y: i32, z: i32) -> BlockId {
        block_of(self.get_cell(x, y, z))
    }

    /// Поставить ячейку. Ответ — куски [cx, cz, …], чьи меши изменились: сам кусок и соседи, в рамку которых попал блок
    /// (грани и тени в углах у соседа). Пусто — ячейка уже такая или вне мира по высоте. Ячейка генератора убирает
    /// правку клетки (см. шапку).
    pub(crate) fn set_cell(&mut self, x: i32, y: i32, z: i32, c: Cell) -> Vec<i32> {
        if y < 0 || y >= H {
            return Vec::new();
        }
        let old = self.get_cell(x, y, z); // область куска в кэше: ячейки генератора у правок куска известны
        if old == c {
            return Vec::new();
        }
        // Блок-сущность (WorldChunk.setBlockState): блок стал другим — убрать (содержимое рассыплет игра, Sim::set_world);
        // новый блок с сущностью — завести, если её нет. Смена состояния того же блока (LIT, сторона) её не трогает.
        let (ob, nb) = (block_of(old), block_of(c));
        if ob != nb {
            self.be.remove([x, y, z]);
        }
        self.grid.on_cell([x, y, z], old, c);
        if grid::tkind(old) == Some(grid::Kind::Item) && grid::tkind(c) != Some(grid::Kind::Item) {
            self.logistics.on_removed([x, y, z]);
        }
        if let Some(k) = bdef(nb).be {
            self.be.ensure([x, y, z], k);
        }
        let (cx, cz) = (x.div_euclid(CS), z.div_euclid(CS));
        let (lx, lz) = (x.rem_euclid(CS), z.rem_euclid(CS));
        let key = local_key(lx, y, lz);
        if is_model(ob) != is_model(nb) {
            let list = self.models.entry((cx, cz)).or_default();
            if is_model(nb) {
                list.push(key);
            } else {
                list.retain(|&k| k != key);
            }
        }
        // Без правки клетка — генератор, значит old и есть его ячейка.
        let m = self.edits.entry((cx, cz)).or_default();
        let base = m.get(&key).map_or(old, |e| e.base);
        debug_assert!(base != BASE_UNKNOWN, "ячейка генератора неизвестна при загруженной области");
        if c == base {
            m.remove(&key);
            if m.is_empty() {
                self.edits.remove(&(cx, cz));
            }
        } else {
            m.insert(key, Edit { cell: c, base });
        }
        self.unsaved.insert((cx, cz));
        self.edit_count += 1;
        if let Some(log) = self.log.as_mut() {
            log.push(([x, y, z], c));
        }
        let near = |l: i32| -> &'static [i32] {
            if l == 0 {
                &[0, -1]
            } else if l == CS - 1 {
                &[0, 1]
            } else {
                &[0]
            }
        };
        let mut out = Vec::with_capacity(8);
        for &dz in near(lz) {
            for &dx in near(lx) {
                let k = (cx + dx, cz + dz);
                if let Some(e) = self.cache.get_mut(&k) {
                    e.reg.set_cell(x, y, z, c);
                }
                out.extend_from_slice(&[k.0, k.1]);
                if !self.dirty.contains(&k) {
                    self.dirty.push(k);
                }
            }
        }
        out
    }
}

/// Мир — общая ручка на данные мира: клиент игры и отрисовка в главном потоке держат один и тот же мир (правки и кэш);
/// у сервера — свой. Меш из мира (build_tile, mesh_chunk) — impl World в mesh. Обёртка для JS — voxel-wasm.
pub struct World {
    pub(crate) d: Rc<RefCell<WorldData>>,
    /// Отладочное зеркало сырой записи (см. шапку): мир локального сервера. Общее для всех ручек этого мира.
    mirror: Rc<RefCell<Option<Rc<RefCell<WorldData>>>>>,
}

impl World {
    pub fn new(seed: u32) -> World {
        World { d: Rc::new(RefCell::new(WorldData::new(seed))), mirror: Rc::new(RefCell::new(None)) }
    }

    /// Ещё одна ручка на тот же мир.
    pub(crate) fn share(&self) -> World {
        World { d: self.d.clone(), mirror: self.mirror.clone() }
    }

    /// Сырую запись этой ручки (и всех её копий) повторять в мире m (локальный сервер).
    pub(crate) fn set_mirror(&self, m: &World) {
        *self.mirror.borrow_mut() = Some(m.d.clone());
    }
    /// Больше не повторять (клиент сетевого сервера).
    pub(crate) fn clear_mirror(&self) {
        *self.mirror.borrow_mut() = None;
    }

    /// Сид мира — им же воркеры строят свои генераторы (web/worker.js получает его первым сообщением).
    pub fn seed(&self) -> u32 {
        self.d.borrow().gen().seed
    }

    /// Высота земли (генератора, без правок) в точке — для подсказки на экране и точки появления.
    pub fn ground(&self, x: i32, z: i32) -> i32 {
        self.d.borrow().gen().column(x, z).h
    }

    /// Перекрёсток (gen/spawn.rs): [x, y пола, z центра Маяка, y истока луча над крышей, радиус защиты, проём Врат
    /// странствий x0, y0, z0, x1, y1, z1]; пусто — площади нет (cargo test без site_on).
    pub fn crossroads(&self) -> Vec<i32> {
        let d = self.d.borrow();
        let s = &d.gen().site;
        if !s.active() {
            return Vec::new();
        }
        let mut v = vec![s.x, s.y0, s.z, s.y0 + crate::gen::spawn::ROOF + 2, crate::gen::spawn::R_ZONE];
        v.extend(s.gate_box());
        v
    }

    /// Доски Перекрёстка (G13, G15: gen/spawn.rs boards_json) — строка JSON: место, сторона, размер, содержимое.
    /// «[]» — площади нет.
    pub fn crossroads_boards(&self) -> String {
        crate::gen::spawn::boards_json(&self.d.borrow().gen().site)
    }

    /// Имя биома с вариантом (луг, болото, старый лес…) — из генератора.
    pub fn biome(&self, x: i32, z: i32) -> String {
        biome_name(&self.d.borrow().gen().column(x, z)).to_string()
    }

    /// Ячейка в точке с учётом правок (кэш областей; промах — генерация куска, ~0,3 мс).
    pub fn get_cell(&self, x: i32, y: i32, z: i32) -> Cell {
        self.d.borrow_mut().get_cell(x, y, z)
    }

    /// Номер блока в точке (ячейка без состояния).
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> BlockId {
        self.d.borrow_mut().get_block(x, y, z)
    }

    /// Поставить ячейку «как генератор» (без правил игры: соседи, тики — Game::world_edit). Ответ — куски [cx, cz, …],
    /// чьи меши надо перестроить: этой правки и всех, ещё не забранных take_dirty (правки из тиков игры). Клетка вне
    /// высоты мира ничего не меняет и просто отдаёт накопленное.
    pub fn set_cell(&self, x: i32, y: i32, z: i32, c: Cell) -> Vec<i32> {
        if let Some(m) = self.mirror.borrow().as_ref() {
            m.borrow_mut().set_cell(x, y, z, c);
        }
        let mut d = self.d.borrow_mut();
        d.set_cell(x, y, z, c);
        d.take_dirty()
    }

    /// Поставить блок с состоянием 0 (AIR — убрать). Ответ — как у set_cell.
    pub fn set_block(&self, x: i32, y: i32, z: i32, b: BlockId) -> Vec<i32> {
        self.set_cell(x, y, z, b as Cell)
    }

    /// Куски [cx, cz, …], чьи меши изменились с прошлого вызова (ломание и ставление в тиках игры).
    pub fn take_dirty(&self) -> Vec<i32> {
        self.d.borrow_mut().take_dirty()
    }

    pub fn has_dirty(&self) -> bool {
        self.d.borrow().has_dirty()
    }

    /// Правки для задания плитки L0 (n × n кусков с рамкой) — пачкой [x, y, z, ячейка, …] для build_tile воркера.
    pub fn tile_edits(&self, tx: i32, tz: i32, n: i32) -> Vec<i32> {
        self.d.borrow().tile_edits(tx, tz, n)
    }

    pub fn edit_count(&self) -> u32 {
        self.d.borrow().edit_count()
    }
}

#[cfg(test)]
mod world_tests {
    use super::*;
    use crate::cell::cell_of;
    use crate::mesh::{ChunkMesh, PART};
    use crate::registry::blocks::opaque;
    use crate::registry::ids::{DIRT, LOG};

    /// Верхний непрозрачный блок столбца (по миру с правками).
    fn top(w: &World, x: i32, z: i32) -> i32 {
        (0..H).rev().find(|&y| opaque(w.get_block(x, y, z))).unwrap()
    }

    /// Куски потока s (0…4 — корзины solid, 5 — cutout, 6 — water) из меша группы: срезы по таблице частей.
    fn streams(m: &ChunkMesh) -> Vec<Vec<Vec<u32>>> {
        let np = m.parts.len() / PART;
        let mut out = vec![vec![Vec::new(); np]; PART];
        let mut at = [0usize; 3];
        for s in 0..PART {
            let (buf, a) = match s {
                0..=4 => (&m.solid, 0),
                5 => (&m.cutout, 1),
                _ => (&m.water, 2),
            };
            for k in 0..np {
                let q = m.parts[k * PART + s] as usize * 8;
                out[s][k] = buf[at[a]..at[a] + q].to_vec();
                at[a] += q;
            }
        }
        out
    }

    /// Заменить части группы новыми мешами кусков — как делает движок в JS (engine/tiles.js, splice).
    fn splice(group: &ChunkMesh, new: &[(usize, &ChunkMesh)]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut st = streams(group);
        for (k, m) in new {
            let one = streams(m);
            for s in 0..PART {
                st[s][*k] = one[s][0].clone();
            }
        }
        let cat = |r: std::ops::Range<usize>| -> Vec<u32> { r.flat_map(|s| st[s].concat()).collect() };
        (cat(0..5), cat(5..6), cat(6..7))
    }

    #[test]
    fn get_after_set() {
        let w = World::new(1337);
        let reg = generate(w.d.borrow().gen(), 0, 0);
        for (x, y, z) in [(0, 40, 0), (5, 60, 9), (15, 1, 15)] {
            assert_eq!(w.get_block(x, y, z), reg.get(x + PAD, y, z + PAD));
        }
        let y = top(&w, 5, 9);
        assert_ne!(w.get_block(5, y, 9), AIR);
        assert_eq!(w.set_block(5, y, 9, AIR), vec![0, 0]);
        assert_eq!(w.get_block(5, y, 9), AIR);
        assert!(w.set_block(5, y, 9, AIR).is_empty(), "тот же блок — без перестройки");
        assert!(w.set_block(5, H, 9, DIRT).is_empty() && w.set_block(5, -1, 9, DIRT).is_empty());
        // Отрицательные координаты: кусок (−1, −1), угол — четыре куска.
        let mut got = w.set_block(-1, 100, -16, DIRT);
        got.sort();
        let mut want = vec![-1, -1, 0, -1, -1, -2, 0, -2];
        want.sort();
        assert_eq!(got, want);
        assert_eq!(w.get_block(-1, 100, -16), DIRT);
        assert_eq!(w.edit_count(), 2);
    }

    #[test]
    fn edit_changes_mesh() {
        let w = World::new(1337);
        let before = w.mesh_chunk(0, 0, 4);
        assert_eq!(before.solid, w.build(0, 0).solid, "кусок из кэша = кусок генератора");
        let y = top(&w, 8, 8);
        w.set_block(8, y, 8, AIR);
        let after = w.mesh_chunk(0, 0, 4);
        assert_ne!(before.solid, after.solid);
        w.set_block(8, y + 1, 8, DIRT);
        assert_ne!(w.mesh_chunk(0, 0, 4).solid, after.solid);
    }

    #[test]
    fn border_edit_reaches_neighbour() {
        let w = World::new(1337);
        // Убираем блок в толще у восточного края куска (0, 0): у соседа (1, 0) открывается грань −X.
        let y = top(&w, 15, 8).min(top(&w, 16, 8)) - 2;
        assert!(opaque(w.get_block(15, y, 8)) && opaque(w.get_block(16, y, 8)));
        let nb = w.mesh_chunk(1, 0, 4);
        assert_eq!(w.set_block(15, y, 8, AIR), vec![0, 0, 1, 0]);
        let nb2 = w.mesh_chunk(1, 0, 4);
        assert_ne!(nb.solid, nb2.solid);
        assert_eq!(nb2.ranges[4], nb.ranges[4] + 1, "у соседа ровно одна новая грань −X (корзина 4)");
        // Угол куска: четыре куска (тени в углах соседей по диагонали).
        let yc = top(&w, 31, 31) + 1;
        let mut got = w.set_block(31, yc, 31, DIRT);
        got.sort();
        let mut want = vec![1, 1, 2, 1, 1, 2, 2, 2];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn worker_path_matches_main() {
        let w = World::new(1337);
        let (tx, tz) = (0, 0);
        let group0 = w.build_tile(0, tx, tz, 4, &w.tile_edits(tx, tz, 4));
        // Правки: внутри куска, на границе кусков внутри группы, на внешней границе группы (рамка соседней группы).
        let e = [(5, 9), (15, 20), (63, 30), (64, 30), (40, 0), (40, -1)];
        let mut touched = Vec::new();
        for (x, z) in e {
            let y = top(&w, x, z);
            touched.extend(w.set_block(x, y, z, AIR));
            touched.extend(w.set_block(x, y + 1, z, DIRT));
            touched.extend(w.set_block(x, y + 2, z, DIRT));
        }
        let edits = w.tile_edits(tx, tz, 4);
        assert!(edits.len() >= 4 * 3 * 5, "правки группы и её рамки");
        assert!(!edits.chunks(4).any(|e| e[0] > 64 || e[2] < -1), "чужие правки вне рамки не берём");
        // Воркер: чистый мир + пачка правок = главный поток.
        let worker = World::new(1337);
        let group1 = worker.build_tile(0, tx, tz, 4, &edits);
        assert_eq!(group1.solid, w.build_tile(0, tx, tz, 4, &edits).solid);
        assert_ne!(group1.solid, group0.solid);
        // Быстрый путь: части группы заменены мешами затронутых кусков = группа, собранная заново.
        let mut chunks: Vec<(i32, i32)> =
            touched.chunks(2).map(|c| (c[0], c[1])).filter(|c| c.0 >= 0 && c.0 < 4 && c.1 >= 0 && c.1 < 4).collect();
        chunks.sort();
        chunks.dedup();
        assert!(chunks.len() >= 4);
        let meshes: Vec<(usize, ChunkMesh)> =
            chunks.iter().map(|&(cx, cz)| ((cz * 4 + cx) as usize, w.mesh_chunk(cx, cz, 4))).collect();
        let refs: Vec<(usize, &ChunkMesh)> = meshes.iter().map(|(k, m)| (*k, m)).collect();
        let (solid, cutout, water) = splice(&group0, &refs);
        assert_eq!(solid, group1.solid);
        assert_eq!(cutout, group1.cutout);
        assert_eq!(water, group1.water);
        let r: u32 = group1.parts.iter().enumerate().filter(|(i, _)| i % PART < 5).map(|(_, v)| v).sum();
        assert_eq!(r as usize, group1.solid.len() / 8);
    }

    #[test]
    fn deterministic_and_survives_eviction() {
        let a = World::new(1337);
        let b = World::new(1337);
        let list: Vec<(i32, i32, i32, BlockId)> =
            (0..40).map(|i| (i * 7 % 48 - 8, 50 + i % 30, i * 13 % 48 - 8, if i % 3 == 0 { AIR } else { DIRT })).collect();
        for &(x, y, z, v) in &list {
            a.set_block(x, y, z, v);
        }
        for &(x, y, z, v) in list.iter().rev() {
            b.set_block(x, y, z, v);
        }
        for cz in -1..3 {
            for cx in -1..3 {
                assert_eq!(a.mesh_chunk(cx, cz, 4).solid, b.mesh_chunk(cx, cz, 4).solid);
            }
        }
        let sorted = |v: Vec<i32>| {
            let mut q: Vec<[i32; 4]> = v.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
            q.sort();
            q.concat()
        };
        assert_eq!(sorted(a.tile_edits(0, 0, 4)), sorted(b.tile_edits(0, 0, 4)));
        // Кэш вытесняет области, правки остаются.
        let (x, y, z) = (3, 120, 3);
        a.set_block(x, y, z, DIRT);
        for i in 0..(CACHE_CAP as i32 + 10) {
            a.get_block(i * 16 + 100, 10, 500);
        }
        assert!(!a.d.borrow().cache.contains_key(&(0, 0)));
        assert_eq!(a.get_block(x, y, z), DIRT);
        assert!(a.d.borrow().cache.len() <= CACHE_CAP);
    }

    #[test]
    fn cells_keep_state() {
        let w = World::new(1337);
        let plain = World::new(1337);
        let y = top(&w, 8, 8) + 1;
        let log_x = cell_of(LOG, 1); // состояние 1 — например, ось бревна
        assert_eq!(w.set_cell(8, y, 8, log_x), vec![0, 0]);
        plain.set_block(8, y, 8, LOG);
        assert_eq!(w.get_cell(8, y, 8), log_x);
        assert_eq!(w.get_block(8, y, 8), LOG);
        assert_eq!(plain.get_cell(8, y, 8), LOG as Cell);
        assert_ne!(w.mesh_chunk(0, 0, 4).solid, plain.mesh_chunk(0, 0, 4).solid, "меш читает ось бревна");
        // Только состояние — тоже правка; set_block сбрасывает состояние в 0.
        assert_eq!(w.set_cell(8, y, 8, cell_of(LOG, 2)), vec![0, 0]);
        assert_eq!(w.set_block(8, y, 8, LOG), vec![0, 0]);
        assert_eq!(w.get_cell(8, y, 8), LOG as Cell);
        w.set_cell(8, y, 8, log_x);
        // Пачка для воркера несёт ячейку целиком; меш у воркера тот же, что у главного потока (с осью бревна).
        let e = w.tile_edits(0, 0, 4);
        assert!(e.chunks(4).any(|e| e[3] == log_x as i32));
        assert_ne!(World::new(1337).build_tile(0, 0, 0, 4, &e).solid, plain.build_tile(0, 0, 0, 4, &plain.tile_edits(0, 0, 4)).solid);
        assert_eq!(World::new(1337).build_tile(0, 0, 0, 1, &w.tile_edits(0, 0, 1)).solid, w.mesh_chunk(0, 0, 1).solid);
        // Состояние на границе видно и в рамке соседа; переживает вытеснение из кэша.
        let yb = top(&w, 15, 8) + 1;
        let cell = cell_of(LOG, 3);
        w.set_cell(15, yb, 8, cell);
        let i = w.d.borrow_mut().load(1, 0).reg.index_of(15, yb, 8).unwrap();
        assert_eq!(w.d.borrow_mut().load(1, 0).cell(i), cell);
        for k in 0..(CACHE_CAP as i32 + 4) {
            w.get_block(k * 16 + 4000, 10, 0);
        }
        assert!(!w.d.borrow().cache.contains_key(&(0, 0)));
        assert_eq!(w.get_cell(8, y, 8), log_x);
        assert_eq!(w.get_cell(15, yb, 8), cell);
        let i = w.d.borrow_mut().load(1, 0).reg.index_of(15, yb, 8).unwrap();
        assert_eq!(w.d.borrow_mut().load(1, 0).cell(i), cell, "рамка соседа после перезагрузки");
    }

    #[test]
    fn game_shares_world() {
        let w = World::new(1337);
        let mut g = crate::game::Game::new(&w);
        for _ in 0..crate::game::TPS {
            g.tick();
        }
        assert_eq!(g.ticks(), 20.0);
        w.set_block(1, 100, 1, DIRT);
        assert_eq!(g.client.sim.world.get_block(1, 100, 1), DIRT, "правка видна клиенту игры (тот же мир, что у отрисовки)");
        assert_eq!(g.server.sim.sim.world.get_block(1, 100, 1), DIRT, "отладочная сырая запись — и в мире локального сервера");
    }

    #[test]
    fn speed() {
        let w = World::new(1337);
        let t = std::time::Instant::now();
        let n = 40;
        for i in 0..n {
            std::hint::black_box(generate(w.d.borrow().gen(), i, 7));
        }
        let gen_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        for cz in 0..8 {
            for cx in 0..8 {
                w.get_block(cx * 16, 0, cz * 16);
            }
        }
        let t = std::time::Instant::now();
        let mut s = 0u32;
        let m = 1_000_000;
        for i in 0..m {
            s += w.get_block(i % 128, 40 + i % 30, (i / 7) % 128) as u32;
        }
        let get_ns = t.elapsed().as_secs_f64() * 1e9 / m as f64;
        std::hint::black_box(s);
        let t = std::time::Instant::now();
        let k = 40;
        let mut chunks = 0;
        for i in 0..k {
            let (x, z) = (i * 5 % 64, i * 11 % 64);
            let y = top(&w, x, z);
            let c = w.set_block(x, y, z, AIR);
            for p in c.chunks(2) {
                std::hint::black_box(w.mesh_chunk(p[0], p[1], 4));
                chunks += 1;
            }
        }
        let edit_ms = t.elapsed().as_secs_f64() * 1000.0 / k as f64;
        println!(
            "generate {:.3} ms/chunk; get_block {:.1} ns; edit {:.3} ms ({:.2} chunks/edit, mesh from cache)",
            gen_ms, get_ns, edit_ms, chunks as f64 / k as f64
        );
    }
}

// ---------------------------------------------------------------- машины и сети Mekanism: вопросы к миру

impl WorldData {
    /// Включить энергосети (мир сервера) и поставить в очередь кабели из правок (после загрузки сохранения).
    pub(crate) fn enable_grid(&mut self) {
        self.grid.on = true;
        self.logistics.on = true;
        let mut cables: Vec<[i32; 3]> = Vec::new();
        for (&(cx, cz), m) in &self.edits {
            for (&k, e) in m {
                if grid::tkind(e.cell).is_some_and(|k| k != grid::Kind::Item) {
                    let (lx, y, lz) = local_pos(k);
                    cables.push([cx * CS + lx, y, cz * CS + lz]);
                }
            }
        }
        cables.sort_unstable();
        for p in cables {
            self.grid.queue_cable(p);
        }
    }
    /// Фаза сетей (после блок-сущностей): свести правки и раздать (Sim::tick_world).
    pub(crate) fn tick_grid(&mut self, zone: &ticks::Zone) {
        let mut g = std::mem::take(&mut self.grid);
        {
            let mut w = GridView { w: self, zone };
            g.commit(&mut w);
            g.tick(&mut w);
        }
        self.grid = g;
        let mut lg = std::mem::take(&mut self.logistics);
        lg.tick(self, zone);
        self.logistics = lg;
    }

    /// Рукава передатчика в клетке p (MEK-02 §14.2, MEK-14 §21.2): сторона NONE — нет; сосед — передатчик того же вида,
    /// его сторона не NONE и (у транспортёров) цвета соединяются — труба; сосед-приёмник этого вида — по режиму стороны
    /// (труба, сопло PUSH, фланец PULL). Остальное — нет.
    pub(crate) fn transmitter_arms(&mut self, p: [i32; 3]) -> u16 {
        use grid::*;
        let c = self.get_cell(p[0], p[1], p[2]);
        let Some(k) = tkind(c) else { return 0 };
        let mut a = 0u16;
        for f in 0..6u8 {
            let m = self.grid.mode(p, f);
            if m == M_NONE {
                continue;
            }
            let q = crate::cell::offset(p, f);
            let qc = self.get_cell(q[0], q[1], q[2]);
            let code = if tkind(qc) == Some(k) {
                let col = k != Kind::Item || colors_link(self.grid.color(p), self.grid.color(q));
                if self.grid.mode(q, f ^ 1) != M_NONE && col {
                    ARM_TUBE
                } else {
                    ARM_NONE
                }
            } else if (k == Kind::Item && logistics::is_inventory(qc)) || (k != Kind::Item && mek::transmitter_face(qc, f ^ 1, k)) {
                match m {
                    M_PUSH => ARM_PUSH,
                    M_PULL => ARM_PULL,
                    _ => ARM_TUBE,
                }
            } else {
                ARM_NONE
            };
            a |= code << (2 * f);
        }
        a
    }
    /// Свести правки сетей без раздачи (загрузка, проверки).
    pub(crate) fn commit_grid(&mut self) {
        let zone = ticks::Zone::default();
        let mut g = std::mem::take(&mut self.grid);
        g.commit(&mut GridView { w: self, zone: &zone });
        self.grid = g;
    }
}

/// Мир для сети: машины — приёмники, зона — кто получает.
struct GridView<'a> {
    w: &'a mut WorldData,
    zone: &'a ticks::Zone,
}

impl grid::GridWorld for GridView<'_> {
    fn cell(&mut self, p: [i32; 3]) -> Cell {
        self.w.get_cell(p[0], p[1], p[2])
    }
    fn accepts(&mut self, p: [i32; 3], f: u8, k: grid::Kind) -> bool {
        let c = self.w.get_cell(p[0], p[1], p[2]);
        match k {
            grid::Kind::Energy => mek::cell_io(c, f).0,
            // Жидкость: сепаратор (вода), тепловой (лава); газ: очиститель (кислород). Насос и газ сепаратора — источники.
            grid::Kind::Fluid | grid::Kind::Gas => {
                use crate::registry::blocks::{bdef, BeKind, MekKind};
                matches!(
                    (bdef(block_of(c)).be, k),
                    (Some(BeKind::Mek(MekKind::Separator | MekKind::Heat)), grid::Kind::Fluid) | (Some(BeKind::Mek(MekKind::Purifier)), grid::Kind::Gas)
                )
            }
            grid::Kind::Item => false,
        }
    }
    fn room(&mut self, p: [i32; 3], k: grid::Kind, sub: u8) -> u64 {
        match k {
            grid::Kind::Energy => self.w.be.mek(p).map_or(0, |m| m.free()),
            _ => self.w.be.mek(p).map_or(0, |m| m.sub_room(sub) as u64),
        }
    }
    fn give(&mut self, p: [i32; 3], k: grid::Kind, sub: u8, n: u64) -> u64 {
        match k {
            grid::Kind::Energy => self.w.be.mek_mut(p).map_or(0, |m| m.insert_energy(n)),
            _ => self.w.be.mek_mut(p).map_or(0, |m| m.insert_gas(sub, n.min(u32::MAX as u64) as u32, false) as u64),
        }
    }
    fn extract(&mut self, p: [i32; 3], f: u8, _k: grid::Kind, want: u8, max: u64) -> (u8, u64) {
        let c = self.w.get_cell(p[0], p[1], p[2]);
        let facing = crate::registry::blocks::facing_of(c);
        self.w.be.mek_mut(p).map_or((0, 0), |m| {
            let (s, n) = m.extract_sub(facing, f, want, max.min(u32::MAX as u64) as u32);
            (s, n as u64)
        })
    }
    fn in_zone(&self, p: [i32; 3]) -> bool {
        self.zone.has(p[0], p[2])
    }
}

impl mek::MekWorld for WorldData {
    fn cell(&mut self, p: [i32; 3]) -> Cell {
        self.get_cell(p[0], p[1], p[2])
    }
    fn push_energy(&mut self, p: [i32; 3], f: u8, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let q = crate::cell::offset(p, f);
        let c = self.get_cell(q[0], q[1], q[2]);
        if grid::is_cable(c) {
            return self.grid.insert_energy(q, n, false);
        }
        if mek::cell_io(c, f ^ 1).0 {
            return self.be.mek_mut(q).map_or(0, |m| m.insert_energy(n));
        }
        0
    }
    fn push_items(&mut self, p: [i32; 3], f: u8, s: ItemStack) -> i32 {
        use crate::cell::F_UP;
        use crate::world::blockentity::fuel_time;
        use mek::sides::{S_ENERGY, S_EXTRA, S_IN, S_IO};
        if s.is_empty() {
            return 0;
        }
        let q = crate::cell::offset(p, f);
        let face = f ^ 1; // грань соседа, обращённая к машине
        let c = self.get_cell(q[0], q[1], q[2]);
        // M5: транспортёр — стек в путь (MEK-14 §7: выталкиватель кладёт всю стопку, сколько примут цели).
        if grid::tkind(c) == Some(grid::Kind::Item) {
            let mut lg = std::mem::take(&mut self.logistics);
            let n = lg.insert_from(self, p, q, s, 0);
            self.logistics = lg;
            return n;
        }
        match self.be.get_mut(q) {
            // Сундук: слоты по порядку, каждый пустой или с тем же предметом берёт сколько может.
            Some(BlockEntity::Chest(ch)) => insert_stack(ch, &(0..27).collect::<Vec<_>>(), s),
            // Печь: сверху — во вход, с боков и снизу — только топливо (ванильные грани).
            Some(BlockEntity::Furnace(fu)) => {
                if face == F_UP {
                    insert_stack(fu, &[0], s)
                } else if fuel_time(s.item) > 0 || s.item == crate::registry::items::I_BUCKET {
                    insert_stack(fu, &[1], s)
                } else {
                    0
                }
            }
            // Машина: только через грань «вход» (вход-выход), «энергия», «доп.» — в слоты этого режима с их проверками.
            Some(BlockEntity::Mek(m)) => {
                let mode = m.side_at(crate::registry::blocks::facing_of(c), face);
                if !matches!(mode, S_IN | S_IO | S_ENERGY | S_EXTRA) {
                    return 0;
                }
                let k = m.kind;
                let d = m.def();
                let slots: Vec<usize> = m
                    .mode_slots(mode)
                    .into_iter()
                    .filter(|&i| d.slots[i].0 != mek::Role::Out && mek::role_accepts(k, d.slots[i].0, s.item))
                    .collect();
                insert_stack(&mut **m, &slots, s)
            }
            None => 0,
        }
    }
    fn push_gas(&mut self, p: [i32; 3], f: u8, sub: u8, n: u32) -> u32 {
        use mek::recipes::{SUB_LAVA, SUB_WATER};
        let q = crate::cell::offset(p, f);
        let c = self.get_cell(q[0], q[1], q[2]);
        // M5: труба (вода, лава) или трубка (газы) — в запас сети; рукав к машине должен быть (сторона не NONE).
        if let Some(k) = grid::tkind(c).filter(|&k| k == grid::Kind::Fluid || k == grid::Kind::Gas) {
            let want = if matches!(sub, SUB_WATER | SUB_LAVA) { grid::Kind::Fluid } else { grid::Kind::Gas };
            if k != want || grid::arm(grid::arms_of(crate::cell::state_of(c)), f ^ 1) == grid::ARM_NONE {
                return 0;
            }
            return self.grid.insert_sub(q, k, sub, n as u64, false) as u32;
        }
        self.be.mek_mut(q).map_or(0, |m| m.insert_gas(sub, n, false))
    }
    fn drain(&mut self, q: [i32; 3]) -> bool {
        self.drained.push(q);
        true
    }
}

/// Положить стопку s в слоты slots хранилища по порядку: пустой слот или слот с тем же предметом берёт сколько может.
/// Ответ — сколько положено.
pub(crate) fn insert_stack(c: &mut dyn Container, slots: &[usize], s: ItemStack) -> i32 {
    let mut left = s.count;
    let max = s.max().max(1);
    for &i in slots {
        if left <= 0 {
            break;
        }
        let cur = c.get(i);
        if cur.is_empty() {
            let n = left.min(max);
            c.set(i, s.with_count(n));
            left -= n;
        } else if cur.same_kind(&s) && cur.count < max {
            let n = left.min(max - cur.count);
            c.set(i, cur.with_count(cur.count + n));
            left -= n;
        }
    }
    s.count - left
}
