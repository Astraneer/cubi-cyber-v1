//! Логистика Mekanism M5 (MEK-14 §1–§9, §21.2; решения после MEK-13/14 в mekanism/decisions.md): предметы в пути по
//! логистическим транспортёрам.
//!
//! Транспортёр — блок с рукавами в состоянии (как кабель, world/grid.rs): связь двух транспортёров — рукав «труба» с
//! обеих сторон (режимы NONE и разные цвета её убирают), рукав к хранилищу несёт режим стороны: труба (NORMAL — сюда
//! можно класть), сопло (PUSH), фланец (PULL — отсюда тянуть). Логика читает только ячейки и карту цветов, своей сети
//! предметов у нас нет: путь ищется обходом по транспортёрам от стартовой клетки (Дейкстра; у базового яруса шаг
//! стоит одинаково — обход в ширину), без предела мода «2 × расстояния» (решение).
//!
//! Стек в пути (Transit) — предмет, цвет, прогресс 0…100 внутри текущей клетки, откуда вошёл, путь (клетки от текущей
//! до цели; у пути с целью последний элемент — хранилище), цель / дом / простой. Тик — фаза сетей после энергии
//! (Sim::tick_world → WorldData::tick_grid): (1) убранные транспортёры — их стеки выпадают; (2) заборы сторон PULL по
//! таймерам (раз в 11 тиков, пустой источник — раз в 4, неудача — 2, 7, 20, 40 тиков; счётчик неудач сбрасывается при
//! успехе — решение); (3) ровно один сдвиг за тик на стек по возрастанию номера: +5 прогресса у базового; на 50 (центр)
//! — «пустит ли следующий / примет ли цель», на 100 — передача в соседа или вставка в цель, отказ — новый путь (другая
//! цель → домой → простой → выпадение). Выталкиватель машины (фаза блок-сущностей) кладёт стек сразу через
//! WorldData::push_items → insert_from: стек первым сдвигом идёт в той же фазе сетей.
//! Прогноз вставки учитывает всё, что уже едет к цели: транспортёр не заберёт больше, чем поместится. Стеки едут только
//! в активной зоне, цели вне неё не выбираются (решение).
//!
//! Мультиплеер: клиенту — состояние стека при появлении и при каждом событии, которого клиент сам не выведет
//! (новый путь, частичная вставка), и конец (Msg::Transit / TransitEnd); обычный сдвиг клиент делает сам тем же
//! правилом (View::advance), рисует кусочно-линейно между тиками (Transit::draw_pos) — без рывков на передачах.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::cell::{offset, state_of, Cell, F_DOWN, F_EAST, F_NORTH, F_SOUTH, F_UP, F_WEST};
use crate::registry::blocks::{bdef, BeKind};
use crate::world::blockentity::BlockEntity;
use crate::world::grid::{arm, arms_of, tkind, Kind, ARM_NONE, ARM_PULL, ARM_PUSH, ARM_TUBE, M_PULL};
use crate::world::mek::sides::{S_IO, S_OUT};
use crate::world::mek::Role;
use crate::world::stack::ItemStack;
use crate::world::ticks::Zone;
use crate::world::mek::MekWorld;
use crate::world::WorldData;

pub(crate) type Pos = [i32; 3];

/// Прогресс за тик базового транспортёра (TransporterTier.BASIC: 20 тиков на блок, 1 блок/с).
pub(crate) const SPEED: u16 = 5;
/// Штук за один забор стороны PULL (базовый ярус).
pub(crate) const PULL_N: i32 = 1;
/// Ждать после удачного забора: следующий через 11 тиков.
pub(crate) const PULL_PERIOD: u8 = 10;
/// Источник пуст — проверка раз в 4 тика.
pub(crate) const IDLE_POLL: u8 = 3;
/// Ожидание после n-й неудачи подряд (⌊e^n⌋, не больше 40): 2, 7, 20, 40…
pub(crate) fn backoff(fails: u8) -> u8 {
    [2, 7, 20, 40][(fails.max(1) - 1).min(3) as usize]
}
/// Порядок сторон забора и выталкивания (EnumSet<Direction>): низ, верх, север, юг, запад, восток.
const SIDE_ORDER: [u8; 6] = [F_DOWN, F_UP, F_NORTH, F_SOUTH, F_WEST, F_EAST];
/// Цветов транспортёра (EnumColor в порядке цикла конфигуратора); 0 — без цвета.
pub(crate) const COLORS: u8 = 18;
/// Имена цветов (сообщение конфигуратора), 0 — «нет».
pub(crate) fn color_name(c: u8) -> &'static str {
    [
        "нет", "чёрный", "синий", "зелёный", "голубой", "тёмно-красный", "фиолетовый", "оранжевый", "светло-серый", "серый",
        "светло-синий", "лаймовый", "бирюзовый", "красный", "пурпурный", "жёлтый", "белый", "коричневый", "розовый",
    ][c.min(COLORS) as usize]
}

/// Куда едет стек: к цели, домой (в источник), простой (катается по линии, пока не появится цель).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum Goal {
    #[default]
    Dest,
    Home,
    Idle,
}

/// Стек в пути.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Transit {
    pub(crate) id: u32,
    pub(crate) item: ItemStack,
    pub(crate) color: u8,
    /// 0…100 в клетке path[step] (на 100 — у выхода, ждёт передачи).
    pub(crate) progress: u16,
    /// Откуда вошёл в текущую клетку (сосед, источник или отказавший).
    pub(crate) from: Pos,
    /// Клетки пути: транспортёры, у пути с целью (Dest, Home) последний элемент — хранилище.
    pub(crate) path: Vec<Pos>,
    pub(crate) step: u16,
    pub(crate) goal: Goal,
    pub(crate) home: Option<Pos>,
    pub(crate) idle_dir: Option<u8>,
    /// Путь посчитан (после загрузки — нет: считается в первом тике).
    pub(crate) ready: bool,
}

/// Направление d, при котором b = a + d (или None, если не соседи).
pub(crate) fn dir(a: Pos, b: Pos) -> Option<u8> {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    (0..6u8).find(|&f| crate::cell::DIRS[f as usize] == d)
}
fn dir_vec(a: Pos, b: Pos) -> [f64; 3] {
    [(b[0] - a[0]) as f64, (b[1] - a[1]) as f64, (b[2] - a[2]) as f64]
}

impl Transit {
    /// Текущая клетка.
    pub(crate) fn at(&self) -> Pos {
        self.path[(self.step as usize).min(self.path.len() - 1)]
    }
    /// Следующая клетка пути (транспортёр или цель).
    pub(crate) fn next(&self) -> Option<Pos> {
        self.path.get(self.step as usize + 1).copied()
    }
    /// Цель пути (хранилище) — у Dest и Home.
    pub(crate) fn target(&self) -> Option<Pos> {
        (self.goal != Goal::Idle).then(|| *self.path.last().unwrap())
    }
    /// Текущая клетка — последний транспортёр пути.
    pub(crate) fn is_last(&self) -> bool {
        let tail = if self.goal == Goal::Idle { 1 } else { 2 };
        self.step as usize + tail >= self.path.len()
    }
    /// Точка рисования (низ предмета; MEK-14 §9, кусочно-линейно — «лучше мода»): s = прогресс + alpha · скорость; до
    /// 50 — от входа к центру, с 50 — от центра к выходу; за 100 — уже в следующей клетке, у цели — стоп на грани.
    pub(crate) fn draw_pos(&self, alpha: f64, speed: u16) -> [f64; 3] {
        let i = self.step as usize;
        let mut s = self.progress as f64 + alpha * speed as f64;
        let (mut prev, mut cell, mut nxt) = (self.from, self.at(), self.next());
        if s >= 100.0 {
            if !self.is_last() && nxt.is_some() {
                prev = cell;
                cell = nxt.unwrap();
                nxt = self.path.get(i + 2).copied();
                s -= 100.0;
            } else {
                s = 100.0;
            }
        }
        let d = if s < 50.0 { dir_vec(prev, cell) } else { nxt.map_or(dir_vec(prev, cell), |n| dir_vec(cell, n)) };
        // Источник может быть не соседом (после загрузки) — без направления предмет стоит в центре.
        let d = if d.iter().map(|v| v.abs()).sum::<f64>() == 1.0 { d } else { [0.0; 3] };
        let p = s / 100.0 - 0.5;
        [cell[0] as f64 + 0.5 + d[0] * p, cell[1] as f64 + 0.25 + d[1] * p, cell[2] as f64 + 0.5 + d[2] * p]
    }
}

/// Предметы в пути (мир сервера).
#[derive(Default)]
pub(crate) struct Logistics {
    /// Ведёт ли этот мир логистику (мир сервера, как Grid::on).
    pub(crate) on: bool,
    /// По возрастанию номера.
    pub(crate) stacks: Vec<Transit>,
    pub(crate) next_id: u32,
    /// Таймеры забора транспортёров со сторонами PULL: (ждать, неудач подряд).
    pull_wait: BTreeMap<Pos, (u8, u8)>,
    /// Убранные транспортёры (из set_cell): их стеки выпадут в начале фазы.
    removed: Vec<Pos>,
    /// Выпавшее (предмет, точка) — забирает Sim (сущности предметов).
    pub(crate) drops: Vec<(ItemStack, [f64; 3])>,
    /// Номера стеков с новым состоянием для клиентов (появился, новый путь) и закончившиеся — забирает сервер (flush).
    pub(crate) changed: Vec<u32>,
    pub(crate) ended: Vec<u32>,
    /// Активная зона последнего тика (выталкивание машин идёт до фазы сетей).
    zone: Zone,
    /// Счётчики для F3 и стенда: доставлено, выпало.
    pub(crate) delivered: u64,
    pub(crate) dropped: u64,
}

/// Хранилище за гранью: слоты, в которые можно класть предмет item с грани face (вход машины — по её стороне и роли).
/// None — не хранилище.
pub(crate) fn insert_slots(w: &mut WorldData, q: Pos, face: u8, item: &ItemStack) -> Option<Vec<usize>> {
    use crate::world::mek::sides::{S_ENERGY, S_EXTRA, S_IN};
    let c = w.get_cell(q[0], q[1], q[2]);
    match w.be().get(q)? {
        BlockEntity::Chest(_) => Some((0..27).collect()),
        BlockEntity::Furnace(_) => {
            if face == F_UP {
                Some(vec![0])
            } else if crate::world::blockentity::fuel_time(item.item) > 0 || item.item == crate::registry::items::I_BUCKET {
                Some(vec![1])
            } else {
                Some(Vec::new())
            }
        }
        BlockEntity::Mek(m) => {
            let mode = m.side_at(crate::registry::blocks::facing_of(c), face);
            if !matches!(mode, S_IN | S_IO | S_ENERGY | S_EXTRA) {
                return Some(Vec::new());
            }
            let k = m.kind;
            let d = m.def();
            Some(
                m.mode_slots(mode)
                    .into_iter()
                    .filter(|&i| d.slots[i].0 != Role::Out && crate::world::mek::role_accepts(k, d.slots[i].0, item.item))
                    .collect(),
            )
        }
    }
}

/// Сколько штук item поместится в слоты slots снимка cur (без записи).
fn fit(cur: &mut [ItemStack], slots: &[usize], s: ItemStack) -> i32 {
    let mut left = s.count;
    let max = s.max().max(1);
    for &i in slots {
        if left <= 0 {
            break;
        }
        let c = cur[i];
        if c.is_empty() {
            let n = left.min(max);
            cur[i] = s.with_count(n);
            left -= n;
        } else if c.same_kind(&s) && c.count < max {
            let n = left.min(max - c.count);
            cur[i] = c.with_count(c.count + n);
            left -= n;
        }
    }
    s.count - left
}

/// Хранилище клетки q для забора с грани face: слоты, из которых можно брать (сундук — все, печь — выход, машина —
/// слоты выхода за стороной «выход» / «вход-выход»), с последнего к первому.
fn extract_slots(w: &mut WorldData, q: Pos, face: u8) -> Vec<usize> {
    let c = w.get_cell(q[0], q[1], q[2]);
    match w.be().get(q) {
        Some(BlockEntity::Chest(_)) => (0..27).rev().collect(),
        Some(BlockEntity::Furnace(_)) => vec![2],
        Some(BlockEntity::Mek(m)) => {
            let mode = m.side_at(crate::registry::blocks::facing_of(c), face);
            if !matches!(mode, S_OUT | S_IO) {
                return Vec::new();
            }
            let d = m.def();
            let mut v: Vec<usize> = m.mode_slots(mode).into_iter().filter(|&i| d.slots[i].0 == Role::Out).collect();
            v.reverse();
            v
        }
        None => Vec::new(),
    }
}

/// Хранилище ли клетка для транспортёра (рукав к ней): сундук, печь, машина со сторонами предметов.
pub(crate) fn is_inventory(c: Cell) -> bool {
    match bdef(crate::cell::block_of(c)).be {
        Some(BeKind::Chest) | Some(BeKind::Furnace) => true,
        Some(BeKind::Mek(k)) => !crate::world::mek::sides::item_modes(k).is_empty(),
        None => false,
    }
}

/// Транспортёр в клетке.
fn is_tp(w: &mut WorldData, p: Pos) -> bool {
    tkind(w.get_cell(p[0], p[1], p[2])) == Some(Kind::Item)
}
/// Рукав транспортёра p в сторону f.
fn arm_at(w: &mut WorldData, p: Pos, f: u8) -> u16 {
    arm(arms_of(state_of(w.get_cell(p[0], p[1], p[2]))), f)
}
/// Связаны ли соседние транспортёры a → b (рукав с обеих сторон).
fn linked(w: &mut WorldData, a: Pos, b: Pos) -> bool {
    let Some(f) = dir(a, b) else { return false };
    is_tp(w, a) && is_tp(w, b) && arm_at(w, a, f) == ARM_TUBE && arm_at(w, b, f ^ 1) == ARM_TUBE
}
/// Пустит ли транспортёр p стек цвета color: в бесцветный — любой, в цветной — только своего цвета.
fn enters(w: &WorldData, p: Pos, color: u8) -> bool {
    let c = w.grid.color(p);
    c == 0 || c == color
}

/// Обход от стартового транспортёра по связанным транспортёрам, пускающим стек цвета color: расстояние (шагов) и
/// родитель. Порядок соседей — по сторонам 0…5: детерминированно.
fn field(w: &mut WorldData, start: Pos, color: u8) -> (Vec<Pos>, HashMap<Pos, (u32, Pos)>) {
    let mut seen: HashMap<Pos, (u32, Pos)> = HashMap::from([(start, (0, start))]);
    let mut order = vec![start];
    let mut q = VecDeque::from([start]);
    while let Some(c) = q.pop_front() {
        let d = seen[&c].0;
        if order.len() > 16_384 {
            break;
        }
        for f in 0..6u8 {
            let n = offset(c, f);
            if seen.contains_key(&n) || arm_at(w, c, f) != ARM_TUBE || !linked(w, c, n) || !enters(w, n, color) {
                continue;
            }
            seen.insert(n, (d + 1, c));
            order.push(n);
            q.push_back(n);
        }
    }
    (order, seen)
}
fn build_path(seen: &HashMap<Pos, (u32, Pos)>, start: Pos, end: Pos) -> Vec<Pos> {
    let mut v = vec![end];
    let mut c = end;
    while c != start {
        c = seen[&c].1;
        v.push(c);
    }
    v.reverse();
    v
}

impl Logistics {
    fn index(&self, id: u32) -> Option<usize> {
        self.stacks.binary_search_by_key(&id, |t| t.id).ok()
    }

    /// Сколько штук item поместится в хранилище q через грань face с учётом всего, что уже едет к q (кроме стека
    /// skip). 0 — не хранилище или полно.
    pub(crate) fn predict(&self, w: &mut WorldData, q: Pos, face: u8, item: &ItemStack, skip: Option<u32>) -> i32 {
        let Some(slots) = insert_slots(w, q, face, item) else { return 0 };
        if slots.is_empty() {
            return 0;
        }
        let Some(c) = w.be_mut().container(q) else { return 0 };
        let mut cur: Vec<ItemStack> = (0..c.size()).map(|i| c.get(i)).collect();
        for t in &self.stacks {
            if Some(t.id) == skip || t.target() != Some(q) {
                continue;
            }
            // Едущие к этой цели — по слотам их грани (у сундука слоты общие).
            let tf = dir(*t.path.last().unwrap(), t.path[t.path.len().saturating_sub(2)]).unwrap_or(face);
            let ts = insert_slots(w, q, tf, &t.item).unwrap_or_default();
            let put = fit(&mut cur, &ts, t.item);
            if put < t.item.count && tf == face {
                return 0; // едущий целиком не влез через ту же грань — цель полна
            }
        }
        fit(&mut cur, &slots, *item)
    }

    /// Лучшая цель для стека item цвета color от транспортёра start (дом исключён): путь и сколько влезет. Сортировка —
    /// (шагов, позиция цели, грань): ближняя первая, поровну не делится (MEK-14 §5).
    fn route(&self, w: &mut WorldData, start: Pos, color: u8, item: &ItemStack, home: Option<Pos>, skip: Option<u32>) -> Option<(Vec<Pos>, i32)> {
        let (order, seen) = field(w, start, color);
        let mut best: Option<(u32, Pos, u8, Pos, i32)> = None;
        for t in order {
            let d = seen[&t].0;
            if best.is_some_and(|b| b.0 < d) {
                break; // дальше только дальние
            }
            for f in 0..6u8 {
                let code = arm_at(w, t, f);
                if code != ARM_TUBE && code != ARM_PUSH {
                    continue;
                }
                let q = offset(t, f);
                if Some(q) == home || is_tp(w, q) || !self.zone.has(q[0], q[2]) {
                    continue;
                }
                let n = self.predict(w, q, f ^ 1, item, skip);
                if n <= 0 {
                    continue;
                }
                let key = (d, q, f ^ 1, t, n);
                if best.is_none_or(|b| (key.0, key.1, key.2) < (b.0, b.1, b.2)) {
                    best = Some(key);
                }
            }
        }
        let (_, q, _, t, n) = best?;
        let mut path = build_path(&seen, start, t);
        path.push(q);
        Some((path, n.min(item.count)))
    }

    /// Путь домой: к транспортёру, у которого рукав к дому (любой, и PULL), и дом примет хоть что-то.
    fn route_home(&self, w: &mut WorldData, start: Pos, color: u8, item: &ItemStack, home: Pos, skip: Option<u32>) -> Option<Vec<Pos>> {
        let (order, seen) = field(w, start, color);
        for t in order {
            let Some(f) = dir(t, home) else { continue };
            if arm_at(w, t, f) == ARM_NONE || is_tp(w, home) {
                continue;
            }
            if self.predict(w, home, f ^ 1, item, skip) > 0 {
                let mut path = build_path(&seen, start, t);
                path.push(home);
                return Some(path);
            }
        }
        None
    }

    /// Путь простоя от at (MEK-14 §6): прямо по idle_dir до конца линии; иначе новая цель; иначе сторона — сначала любая,
    /// кроме обратной (к back), потом обратная.
    fn idle_path(&self, w: &mut WorldData, t: &Transit, at: Pos, back: Option<Pos>) -> Option<(Vec<Pos>, Goal, Option<u8>)> {
        let straight = |w: &mut WorldData, d: u8| -> Option<Vec<Pos>> {
            let mut v = vec![at];
            let mut c = at;
            loop {
                let n = offset(c, d);
                if !linked(w, c, n) || !enters(w, n, t.color) || v.len() > 4096 {
                    break;
                }
                v.push(n);
                c = n;
            }
            (v.len() > 1).then_some(v)
        };
        if let Some(d) = t.idle_dir {
            if let Some(v) = straight(w, d) {
                return Some((v, Goal::Idle, Some(d)));
            }
        }
        if let Some((v, _)) = self.route(w, at, t.color, &t.item, t.home, Some(t.id)) {
            return Some((v, Goal::Dest, None));
        }
        let rev = back.and_then(|b| dir(at, b));
        for d in (0..6u8).filter(|&d| Some(d) != rev).chain(rev) {
            if let Some(v) = straight(w, d) {
                return Some((v, Goal::Idle, Some(d)));
            }
        }
        None
    }

    /// Новый путь стека i (отказ refused — сосед или цель, не принявшие стек; exit — на выходе из клетки): другая цель →
    /// домой → простой → выпадение. false — выпал.
    fn reroute(&mut self, w: &mut WorldData, i: usize, refused: Option<Pos>, exit: bool) -> bool {
        let t = self.stacks[i].clone();
        let at = t.at();
        let back = refused.or(Some(t.from));
        let mut found: Option<(Vec<Pos>, Goal, Option<u8>)> = None;
        if t.goal != Goal::Idle {
            if let Some((v, _)) = self.route(w, at, t.color, &t.item, t.home, Some(t.id)) {
                found = Some((v, Goal::Dest, None));
            }
        }
        let mut home = t.home;
        if found.is_none() {
            if let Some(h) = t.home {
                match self.route_home(w, at, t.color, &t.item, h, Some(t.id)) {
                    Some(v) => found = Some((v, Goal::Home, None)),
                    None => home = None, // дом забывается навсегда
                }
            }
        }
        if found.is_none() {
            let mut tt = t.clone();
            tt.home = home;
            found = self.idle_path(w, &tt, at, back);
        }
        let Some((path, goal, idle_dir)) = found else {
            self.drop_stack(i);
            return false;
        };
        let s = &mut self.stacks[i];
        s.path = path;
        s.step = 0;
        s.goal = goal;
        s.home = home;
        s.idle_dir = idle_dir;
        s.ready = true;
        if let Some(r) = refused {
            s.progress = 0;
            s.from = r;
        } else if exit {
            s.progress = 50;
        }
        self.changed.push(s.id);
        true
    }

    /// Стек i выпадает предметом в своей точке.
    fn drop_stack(&mut self, i: usize) {
        let t = self.stacks.remove(i);
        let p = t.draw_pos(0.0, 0);
        self.drops.push((t.item, [p[0], p[1] + 0.25, p[2]]));
        self.ended.push(t.id);
        self.dropped += 1;
    }

    fn spawn(&mut self, mut t: Transit) {
        t.id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        t.ready = true;
        self.changed.push(t.id);
        self.stacks.push(t);
    }

    /// Вставка стека s в транспортёр via со стороны источника src (выталкиватель машины, MEK-14 §7): сторона
    /// транспортёра к источнику должна принимать (не NONE, не PUSH), путь к цели есть. Ответ — сколько взято (одним
    /// стеком, сколько примет цель по прогнозу).
    pub(crate) fn insert_from(&mut self, w: &mut WorldData, src: Pos, via: Pos, s: ItemStack, color: u8) -> i32 {
        if !self.on || s.is_empty() {
            return 0;
        }
        let Some(f) = dir(via, src) else { return 0 };
        let code = arm_at(w, via, f);
        if !(code == ARM_TUBE || code == ARM_PULL) || !enters(w, via, color) || !self.zone.has(via[0], via[2]) {
            return 0;
        }
        let Some((path, n)) = self.route(w, via, color, &s, Some(src), None) else { return 0 };
        self.spawn(Transit {
            item: s.with_count(n),
            color,
            from: src,
            path,
            goal: Goal::Dest,
            home: Some(src),
            ..Default::default()
        });
        n
    }

    /// Забор стороной PULL транспортёра p из хранилища q (грань face): заявка — по видам с последнего слота, до PULL_N
    /// каждого; уходит первый вид, для которого нашлась цель. Ответ: None — заявка пуста; Some(ушло ли).
    fn pull(&mut self, w: &mut WorldData, p: Pos, q: Pos, face: u8) -> Option<bool> {
        let slots = extract_slots(w, q, face);
        let c = w.be_mut().container(q)?;
        let mut kinds: Vec<(ItemStack, i32)> = Vec::new();
        for &i in &slots {
            let s = c.get(i);
            if s.is_empty() {
                continue;
            }
            match kinds.iter_mut().find(|(k, _)| k.same_kind(&s)) {
                Some(k) => k.1 += s.count,
                None => kinds.push((s, s.count)),
            }
        }
        if kinds.is_empty() {
            return None;
        }
        let color = w.grid.color(p);
        for (k, have) in kinds {
            let want = k.with_count(have.min(PULL_N));
            let Some((path, n)) = self.route(w, p, color, &want, Some(q), None) else { continue };
            // Вынуть n штук с последнего слота к первому.
            let c = w.be_mut().container(q).unwrap();
            let mut left = n;
            for &i in &slots {
                let s = c.get(i);
                if left == 0 {
                    break;
                }
                if s.same_kind(&k) && !s.is_empty() {
                    let x = s.count.min(left);
                    c.set(i, if s.count > x { s.with_count(s.count - x) } else { ItemStack::EMPTY });
                    left -= x;
                }
            }
            self.spawn(Transit { item: k.with_count(n - left), color, from: q, path, goal: Goal::Dest, home: Some(q), ..Default::default() });
            return Some(true);
        }
        Some(false)
    }

    /// Зона этого тика — до фазы блок-сущностей (выталкиватели машин кладут стеки раньше фазы сетей).
    pub(crate) fn set_zone(&mut self, zone: &Zone) {
        if self.on {
            self.zone = zone.clone();
        }
    }

    /// Убранный транспортёр (WorldData::set_cell): стеки в нём выпадут в фазе сетей.
    pub(crate) fn on_removed(&mut self, p: Pos) {
        if self.on {
            self.removed.push(p);
        }
    }

    /// Фаза сетей (после энергии и труб).
    pub(crate) fn tick(&mut self, w: &mut WorldData, zone: &Zone) {
        if !self.on {
            return;
        }
        self.zone = zone.clone();
        // 1. Убранные транспортёры: их стеки — предметами.
        for p in std::mem::take(&mut self.removed) {
            if is_tp(w, p) {
                continue;
            }
            self.pull_wait.remove(&p);
            let mut i = 0;
            while i < self.stacks.len() {
                if self.stacks[i].ready && self.stacks[i].at() == p {
                    self.drop_stack(i);
                } else {
                    i += 1;
                }
            }
        }
        // 2. Заборы сторон PULL по таймерам.
        let pulls: Vec<(Pos, u16)> = w
            .grid
            .modes
            .iter()
            .filter(|(_, &m)| (0..6).any(|f| (m >> (2 * f)) & 3 == M_PULL as u16))
            .map(|(&p, &m)| (p, m))
            .collect();
        self.pull_wait.retain(|p, _| pulls.iter().any(|(q, _)| q == p));
        for (p, m) in pulls {
            if !zone.has(p[0], p[2]) || !is_tp(w, p) {
                continue;
            }
            let e = self.pull_wait.entry(p).or_insert((0, 0));
            if e.0 > 0 {
                e.0 -= 1;
                continue;
            }
            let mut wait = IDLE_POLL;
            let mut fails = e.1;
            for f in SIDE_ORDER {
                if (m >> (2 * f)) & 3 != M_PULL as u16 || arm_at(w, p, f) != ARM_PULL {
                    continue;
                }
                match self.pull(w, p, offset(p, f), f ^ 1) {
                    None => {}
                    Some(true) => {
                        wait = PULL_PERIOD;
                        fails = 0;
                        break;
                    }
                    Some(false) => {
                        fails = fails.saturating_add(1);
                        wait = backoff(fails);
                    }
                }
            }
            self.pull_wait.insert(p, (wait, fails));
        }
        // 3. Движение: один сдвиг за тик, по возрастанию номера.
        let ids: Vec<u32> = self.stacks.iter().map(|t| t.id).collect();
        for id in ids {
            let Some(i) = self.index(id) else { continue };
            if !self.stacks[i].ready {
                // После загрузки: путь заново от текущей клетки, прогресс сохранён.
                let t = &self.stacks[i];
                if !zone.has(t.path[0][0], t.path[0][2]) {
                    continue;
                }
                if !is_tp(w, t.path[0]) {
                    self.drop_stack(i);
                    continue;
                }
                if !self.reroute(w, i, None, false) {
                    continue;
                }
            }
            let at = self.stacks[i].at();
            if !zone.has(at[0], at[2]) {
                continue;
            }
            let before = self.stacks[i].progress;
            self.stacks[i].progress += SPEED;
            let now = self.stacks[i].progress;
            if now >= 100 {
                self.on_exit(w, i);
            } else if before < 50 && now >= 50 {
                self.on_center(w, i);
            }
        }
    }

    /// Выход из клетки (прогресс ≥ 100): передача в следующий транспортёр или вставка в цель; отказ — новый путь.
    fn on_exit(&mut self, w: &mut WorldData, i: usize) {
        let t = &self.stacks[i];
        let at = t.at();
        let refused;
        if !t.is_last() {
            let next = t.next().unwrap();
            if linked(w, at, next) && enters(w, next, t.color) {
                let s = &mut self.stacks[i];
                s.step += 1;
                s.from = at;
                s.progress -= 100;
                return;
            }
            refused = Some(next);
        } else if let Some(target) = t.target() {
            let f = dir(at, target).unwrap_or(F_UP);
            let item = t.item;
            let n = if is_tp(w, target) || arm_at(w, at, f) == ARM_NONE { 0 } else { w.push_items(at, f, item).min(item.count) };
            if n >= item.count {
                let t = self.stacks.remove(i);
                self.ended.push(t.id);
                self.delivered += 1;
                return;
            }
            self.stacks[i].item.count -= n;
            refused = Some(target);
        } else {
            refused = None;
        }
        self.reroute(w, i, refused, true);
    }

    /// Центр клетки (прогресс перешёл 50): последний — примет ли цель (простой — всегда ищет заново); не последний —
    /// пустит ли следующий. Нет — новый путь, стек поворачивает в центре.
    fn on_center(&mut self, w: &mut WorldData, i: usize) {
        let t = &self.stacks[i];
        let at = t.at();
        let ok = if t.is_last() {
            match t.target() {
                Some(q) => {
                    let f = dir(at, q).unwrap_or(F_UP);
                    arm_at(w, at, f) != ARM_NONE && !is_tp(w, q) && self.predict(w, q, f ^ 1, &t.item, Some(t.id)) > 0
                }
                None => false,
            }
        } else {
            let n = t.next().unwrap();
            linked(w, at, n) && enters(w, n, t.color)
        };
        if !ok {
            self.reroute(w, i, None, false);
        }
    }

    // ------------------------------------------------------------ сохранение и отладка

    /// Стеки для записи: (клетка, откуда, прогресс, предмет, цвет, цель-тип, дом, направление простоя).
    pub(crate) fn saved(&self) -> Vec<(Pos, Pos, u16, ItemStack, u8, Goal, Option<Pos>, Option<u8>)> {
        self.stacks.iter().map(|t| (t.at(), t.from, t.progress, t.item, t.color, t.goal, t.home, t.idle_dir)).collect()
    }
    /// После загрузки: стеки без пути (посчитается в первом тике).
    pub(crate) fn restore(&mut self, list: Vec<(Pos, Pos, u16, ItemStack, u8, Goal, Option<Pos>, Option<u8>)>) {
        for (at, from, progress, item, color, goal, home, idle_dir) in list {
            let id = self.next_id;
            self.next_id += 1;
            self.stacks.push(Transit { id, item, color, progress: progress.min(99), from, path: vec![at], step: 0, goal, home, idle_dir, ready: false });
        }
    }
    /// Отладка F3 у транспортёра p: [стеков в пути всего, в этой клетке, доставлено, выпало].
    pub(crate) fn debug_at(&self, p: Pos) -> [u64; 4] {
        let here = self.stacks.iter().filter(|t| t.at() == p).count() as u64;
        [self.stacks.len() as u64, here, self.delivered, self.dropped]
    }
}

// ---------------------------------------------------------------- клиент: копия стеков для рисования

/// Стек в пути у клиента (Msg::Transit): путь — от текущей клетки.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TransitMsg {
    pub(crate) id: u32,
    pub(crate) item: ItemStack,
    pub(crate) color: u8,
    pub(crate) progress: u16,
    pub(crate) from: Pos,
    pub(crate) path: Vec<Pos>,
    pub(crate) idle: bool,
}

impl TransitMsg {
    pub(crate) fn of(t: &Transit) -> TransitMsg {
        TransitMsg {
            id: t.id,
            item: t.item,
            color: t.color,
            progress: t.progress,
            from: t.from,
            path: t.path[t.step as usize..].to_vec(),
            idle: t.goal == Goal::Idle,
        }
    }
}

/// Стеки, которые видит клиент: сдвиг за тик тем же правилом, что у сервера, без проверок (их решает сервер и шлёт
/// новое состояние); у последнего транспортёра стоп на грани до конца с сервера.
#[derive(Default)]
pub(crate) struct View {
    pub(crate) list: Vec<Transit>,
}

impl View {
    pub(crate) fn apply(&mut self, msgs: Vec<TransitMsg>) {
        for m in msgs {
            if m.path.is_empty() {
                continue;
            }
            let t = Transit {
                id: m.id,
                item: m.item,
                color: m.color,
                progress: m.progress,
                from: m.from,
                path: m.path,
                step: 0,
                goal: if m.idle { Goal::Idle } else { Goal::Dest },
                home: None,
                idle_dir: None,
                ready: true,
            };
            match self.list.binary_search_by_key(&t.id, |x| x.id) {
                Ok(i) => self.list[i] = t,
                Err(i) => self.list.insert(i, t),
            }
        }
    }
    pub(crate) fn end(&mut self, ids: &[u32]) {
        self.list.retain(|t| !ids.contains(&t.id));
    }
    /// Тик клиента: +скорость; на выходе — в следующую клетку пути, у последней — стоп на 100.
    pub(crate) fn advance(&mut self) {
        for t in &mut self.list {
            if t.progress >= 100 && t.is_last() {
                continue;
            }
            t.progress += SPEED;
            if t.progress >= 100 && !t.is_last() {
                t.from = t.at();
                t.step += 1;
                t.progress -= 100;
            } else if t.progress > 100 {
                t.progress = 100;
            }
        }
    }
    /// Для item_draws: по 8 чисел на стек — низ, поворот (вдоль оси под 45°), подъём 0, предмет, число, свет.
    pub(crate) fn draws(&self, alpha: f64, view: [f64; 3], out: &mut Vec<f32>) {
        for t in &self.list {
            let p = t.draw_pos(alpha, if t.progress >= 100 && t.is_last() { 0 } else { SPEED });
            let d2 = (p[0] - view[0]).powi(2) + (p[1] - view[1]).powi(2) + (p[2] - view[2]).powi(2);
            if d2 > 64.0 * 64.0 {
                continue;
            }
            let along_x = t.next().map_or(false, |n| n[0] != t.at()[0]) || (t.from[0] != t.at()[0]);
            let yaw = if along_x { std::f32::consts::FRAC_PI_4 } else { 3.0 * std::f32::consts::FRAC_PI_4 };
            out.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, yaw, 0.0, t.item.item as f32, t.item.count as f32, 1.0]);
        }
    }
}

#[cfg(test)]
#[path = "logistics_tests.rs"]
mod tests;
