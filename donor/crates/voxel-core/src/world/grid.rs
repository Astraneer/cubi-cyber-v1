//! Энергосети Mekanism (E14, MEK-02; решения после MEK-02 в mekanism/decisions.md): реестр сетей из кабелей.
//!
//! Энергию хранит сеть, а не кабель: соединённые кабели — один запас, ёмкость = 8 000 Дж × кабелей. Генератор или куб,
//! толкающий в любой кабель сети, кладёт прямо в этот запас (insert_energy, фаза блок-сущностей). Фаза сетей идёт
//! после всех блок-сущностей (Sim::tick_world): сначала свести правки (commit: поставленные кабели — присоединение или
//! слияние в самую большую сеть, снятые — вычесть и при ≥ 2 связанных соседях разрезать), затем каждая сеть по
//! возрастанию номера: свечение (целое 0…1000, шаг 20 — §17.7) → раздача всего запаса приёмникам «ровным делением»
//! (split_fair, §4.2; просьба машины — свободное место батареи; одна машина — одна цель, даже если касается сети
//! несколькими гранями). Потерь нет; пропускная за тик = ёмкость сети.
//!
//! Связь между кабелями — любое соседство (режимов сторон NONE/PUSH/PULL в M1 нет — конфигуратор, срез M3). Рукава в
//! состоянии ячейки (12 бит, 2 на сторону мимо бита 7 — waterlogged) — только вид и форма: их считает Sim::set_world по
//! соседям (кабель или грань машины с энергией), воркеру меша и лучу хватает ячейки.
//! Членство кабеля от активной зоны не зависит; приёмники вне зоны пропускаются (§14.5). Разрез: запас обрезается по
//! ёмкости оставшихся и делится пропорционально ёмкости частей, номер остаётся у большей; свечение части наследуют.
//! Порядок — только номерами и позициями (HashMap не обходится там, где от порядка зависит итог): детерминизм сервера.
//!
//! Сеть живёт только у сервера (on); клиент получает свечение пакетом (Msg::NetGlow: «член сети, свечение»).
//!
//! M5 (MEK-14, G14): тот же реестр ведёт механические трубы (жидкость) и напорные трубки (газ) — Kind сети. Скелет
//! общий: слияние, разрез, приёмники, ровное деление; у жидкости и газа запас — мБ одного вещества (sub), ёмкость —
//! 2 000 / 4 000 мБ на трубу / трубку, стороны PULL тянут из соседей до 250 / 750 мБ/т (до раздачи), сеть с одним
//! веществом не сливается с сетью другого, пока одна не опустеет. Транспортёры (Kind::Item) в сети не входят: их
//! стеки и пути — world/logistics.rs (связи читаются из рукавов в ячейках).
//! Режимы сторон передатчиков (NORMAL → PUSH → PULL → NONE, конфигуратор) и цвета транспортёров — разреженные карты
//! modes / colors здесь (только отличные от умолчаний; сохраняются). Рукава в состоянии ячейки несут видимый режим:
//! 0 нет, 1 труба (к передатчику того же вида или NORMAL к приёмнику), 2 сопло (PUSH), 3 фланец (PULL) — их считает
//! WorldData::transmitter_arms (сервер), меш и сеть читают только ячейку. Связь двух передатчиков — рукав с обеих сторон.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::cell::{block_of, offset, BlockId, Cell, State};
use crate::registry::ids::{LOGISTICAL_TRANSPORTER, MECHANICAL_PIPE, PRESSURIZED_TUBE, UNIVERSAL_CABLE};

pub(crate) type Pos = [i32; 3];

/// Ёмкость базового кабеля (CableTier.BASIC): 8 000 Дж; она же его доля пропускной за тик.
pub(crate) const CABLE_CAP: u64 = 8000;
/// Шаг свечения за тик (0.02) в тысячных.
pub(crate) const GLOW_STEP: u16 = 20;

// ---------------------------------------------------------------- рукава в состоянии ячейки (MEK-02 §14.2)

/// Нет рукава.
pub(crate) const ARM_NONE: u16 = 0;
/// Труба (к кабелю или к машине с энергией на этой грани).
pub(crate) const ARM_TUBE: u16 = 1;
/// Сопло — сторона PUSH к приёмнику.
pub(crate) const ARM_PUSH: u16 = 2;
/// Фланец — сторона PULL к источнику.
pub(crate) const ARM_PULL: u16 = 3;

/// Режимы сторон передатчика (ConnectionType): обычный, только толкать, тянуть, нет связи.
pub(crate) const M_NORMAL: u8 = 0;
pub(crate) const M_PUSH: u8 = 1;
pub(crate) const M_PULL: u8 = 2;
pub(crate) const M_NONE: u8 = 3;
/// Имя режима стороны (сообщение конфигуратора).
pub(crate) fn link_name(m: u8) -> &'static str {
    ["обычный", "толкать", "тянуть", "нет"][(m & 3) as usize]
}

/// Вид передатчика: энергия (кабель), предметы (транспортёр), жидкость (труба), газ (трубка).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub(crate) enum Kind {
    Energy,
    Item,
    Fluid,
    Gas,
}
/// Вид передатчика блока b.
#[inline]
pub(crate) fn kind_of(b: BlockId) -> Option<Kind> {
    match b {
        UNIVERSAL_CABLE => Some(Kind::Energy),
        LOGISTICAL_TRANSPORTER => Some(Kind::Item),
        MECHANICAL_PIPE => Some(Kind::Fluid),
        PRESSURIZED_TUBE => Some(Kind::Gas),
        _ => None,
    }
}
/// Вид передатчика ячейки.
#[inline]
pub(crate) fn tkind(c: Cell) -> Option<Kind> {
    kind_of(block_of(c))
}
/// Передатчик любого вида.
#[inline]
pub(crate) fn is_transmitter(c: Cell) -> bool {
    tkind(c).is_some()
}
/// Передатчик, которого ведёт реестр сетей (всё, кроме транспортёров).
#[inline]
fn grid_kind(c: Cell) -> Option<Kind> {
    tkind(c).filter(|&k| k != Kind::Item)
}
/// Ёмкость одного передатчика вида k (Дж или мБ): кабель 8 000, труба 2 000, трубка 4 000 (базовые ярусы).
pub(crate) fn unit_cap(k: Kind) -> u64 {
    match k {
        Kind::Energy => CABLE_CAP,
        Kind::Fluid => PIPE_CAP,
        Kind::Gas => TUBE_CAP,
        Kind::Item => 0,
    }
}
/// Ёмкость базовой механической трубы, мБ (PipeTier.BASIC) и забор стороны PULL за тик.
pub(crate) const PIPE_CAP: u64 = 2_000;
pub(crate) const PIPE_PULL: u64 = 250;
/// Ёмкость базовой напорной трубки, мБ (TubeTier.BASIC) и забор стороны PULL за тик.
pub(crate) const TUBE_CAP: u64 = 4_000;
pub(crate) const TUBE_PULL: u64 = 750;
/// Цвета транспортёров соединяются: один без цвета или цвета равны (MEK-14 §8).
#[inline]
pub(crate) fn colors_link(a: u8, b: u8) -> bool {
    a == 0 || b == 0 || a == b
}

/// 12 бит рукавов из состояния (биты 0–6 и 8–12; бит 7 — waterlogged, 13–15 свободны).
#[inline]
pub(crate) fn arms_of(s: State) -> u16 {
    (s & 0x7F) | ((s >> 1) & 0xF80)
}
/// Состояние с рукавами a (бит 7 и биты 13–15 не трогаются).
#[inline]
pub(crate) fn with_arms(s: State, a: u16) -> State {
    (s & !0x1F7F) | (a & 0x7F) | ((a & 0xF80) << 1)
}
/// Рукав на стороне f (F_* из cell.rs).
#[inline]
pub(crate) fn arm(a: u16, f: u8) -> u16 {
    (a >> (2 * f)) & 3
}
/// Маска «есть рукав» по сторонам (бит f) — форма и модель кабеля.
#[inline]
pub(crate) fn arm_mask(s: State) -> u8 {
    let a = arms_of(s);
    (0..6u8).fold(0, |m, f| if arm(a, f) != ARM_NONE { m | 1 << f } else { m })
}
/// Рукава из маски (все — трубы).
pub(crate) fn arms_from_mask(m: u8) -> u16 {
    (0..6u8).fold(0, |a, f| if m >> f & 1 != 0 { a | ARM_TUBE << (2 * f) } else { a })
}

/// Энергетический передатчик (кабель) — ячейка.
#[inline]
pub(crate) fn is_cable(c: Cell) -> bool {
    block_of(c) == UNIVERSAL_CABLE
}

// ---------------------------------------------------------------- ровное деление (MEK-02 §4.2)

/// Ровное деление суммы total между целями с просьбами req (просьба больше остатка — как «сколько примет»): скромные
/// получают всё, остальным — поровну, первым по порядку на 1 больше (EmitUtils.sendToAcceptors / Target). Ответ —
/// сколько получит каждая; не взятое остаётся у отдающего.
pub(crate) fn split_fair(total: u64, req: &[u64]) -> Vec<u64> {
    let mut give = vec![0u64; req.len()];
    if req.is_empty() || total == 0 {
        return give;
    }
    let mut left = total;
    let mut n = req.len() as u64;
    let mut share = left / n;
    let mut rem = left % n;
    let mut waiting: Vec<usize> = Vec::new();
    let mut changed = false;
    // Проход 1: скромные — сразу.
    for (i, &r) in req.iter().enumerate() {
        let want = r.min(left);
        if share > 0 && want <= share {
            give[i] = want;
            left -= want;
            n -= 1;
            if want != share && n > 0 {
                let s = left / n;
                changed |= s != share;
                share = s;
                rem = left % n;
            }
        } else {
            waiting.push(i);
        }
    }
    // Проход 2: доля выросла — ждущие, что теперь укладываются.
    while changed && n > 0 {
        changed = false;
        let mut k = 0;
        while k < waiting.len() {
            let i = waiting[k];
            let want = req[i].min(left);
            if share > 0 && want <= share {
                give[i] = want;
                left -= want;
                n -= 1;
                waiting.remove(k);
                if n > 0 {
                    let s = left / n;
                    changed |= s != share;
                    share = s;
                    rem = left % n;
                }
            } else {
                k += 1;
            }
        }
    }
    // Проход 3: остальным — доля (+1, пока есть остаток).
    let mut k = 0;
    while k < waiting.len() {
        let i = waiting[k];
        let offer = if rem > 0 { share + 1 } else { share };
        if offer == 0 {
            break;
        }
        let want = req[i].min(left);
        if want <= offer {
            give[i] = want;
            left -= want;
            n -= 1;
            waiting.remove(k);
            if n > 0 {
                share = left / n;
                rem = left % n;
            }
        } else {
            give[i] = offer;
            left -= offer;
            if rem > 0 && offer == share + 1 {
                rem -= 1;
            }
            k += 1;
        }
    }
    // Остаток (кто-то взял меньше предложенного) — ждущим по порядку.
    for &i in &waiting {
        if left == 0 {
            break;
        }
        let more = (req[i] - give[i]).min(left);
        give[i] += more;
        left -= more;
    }
    give
}

// ---------------------------------------------------------------- реестр

#[derive(Clone, Copy, Debug)]
struct Member {
    net: u32,
    kind: Kind,
    /// Связи с соседями-членами по сторонам (бит f).
    links: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct Net {
    pub(crate) kind: Kind,
    /// Вещество жидкости или газа (recipes SUB_*; 0 — пусто). У энергии 0.
    pub(crate) sub: u8,
    pub(crate) members: Vec<Pos>,
    pub(crate) cap: u64,
    pub(crate) stored: u64,
    /// Свечение 0…1000 (currentScale × 1000).
    pub(crate) glow: u16,
    /// В прошлом тике что-то роздано (разгорание).
    pub(crate) sent: bool,
    /// Приёмники (машины с входом энергии на грани к кабелю), по возрастанию позиции.
    pub(crate) targets: Vec<Pos>,
    targets_dirty: bool,
    /// Роздано за прошлый тик (окно F3, проверки).
    pub(crate) flow: u64,
    /// Стороны PULL (жидкость, газ): (источник, его грань к передатчику), по возрастанию.
    pub(crate) pulls: Vec<(Pos, u8)>,
}

impl Default for Net {
    fn default() -> Self {
        Net {
            kind: Kind::Energy,
            sub: 0,
            members: Vec::new(),
            cap: 0,
            stored: 0,
            glow: 0,
            sent: false,
            targets: Vec::new(),
            targets_dirty: false,
            flow: 0,
            pulls: Vec::new(),
        }
    }
}

impl Net {
    /// Наименьшая позиция члена — «ключ» сети для клиента (свечение по любому члену).
    pub(crate) fn anchor(&self) -> Pos {
        self.members.iter().copied().min().unwrap_or([0; 3])
    }
}

/// Что сеть спрашивает у мира: ячейки соседей и приёмники (машины Mekanism — world/mek).
pub(crate) trait GridWorld {
    fn cell(&mut self, p: Pos) -> Cell;
    /// Принимает ли машина в клетке p ресурс вида k с мировой грани f (грань машины, обращённая к передатчику).
    fn accepts(&mut self, p: Pos, f: u8, k: Kind) -> bool;
    /// Свободное место машины p под ресурс вида k (энергия — батарея; жидкость, газ — бак под вещество sub).
    fn room(&mut self, p: Pos, k: Kind, sub: u8) -> u64;
    /// Положить n в машину p (ответ — сколько принято).
    fn give(&mut self, p: Pos, k: Kind, sub: u8, n: u64) -> u64;
    /// Взять до max из бака машины p через её грань f (сторона PULL трубы, трубки): вещество want (0 — любое).
    /// Ответ — (вещество, сколько).
    fn extract(&mut self, _p: Pos, _f: u8, _k: Kind, _want: u8, _max: u64) -> (u8, u64) {
        (0, 0)
    }
    /// Машина в активной зоне (вне зоны — не получает).
    fn in_zone(&self, p: Pos) -> bool;
}

#[derive(Default)]
pub(crate) struct Grid {
    /// Ведёт ли этот мир сети (мир сервера).
    pub(crate) on: bool,
    at: HashMap<Pos, Member>,
    nets: Vec<Option<Net>>,
    free: Vec<u32>,
    added: Vec<Pos>,
    removed: Vec<Pos>,
    /// Клетки, у соседей которых могли поменяться приёмники (поставили или убрали машину, повернули).
    touched: Vec<Pos>,
    /// Передатчики, у которых сменились связи без смены блока (режим стороны; опустевшая сеть жидкости рядом с чужой):
    /// снять и поставить заново в фазе сетей.
    relinks: Vec<Pos>,
    /// Состав сетей менялся (клиентам — новый список свечения).
    pub(crate) layout_changed: bool,
    /// Режимы сторон передатчиков, отличные от «всё NORMAL»: 2 бита на сторону (M_*).
    pub(crate) modes: BTreeMap<Pos, u16>,
    /// Цвета транспортёров (1…18; без цвета — нет записи).
    pub(crate) colors: BTreeMap<Pos, u8>,
}

impl Grid {
    /// Из WorldData::set_cell: ячейка p стала другой. Сами сети меняются в фазе сетей (commit).
    pub(crate) fn on_cell(&mut self, p: Pos, old: Cell, new: Cell) {
        if !self.on {
            return;
        }
        let (was, is) = (tkind(old), tkind(new));
        if was.is_some() && was != is {
            self.modes.remove(&p);
            self.colors.remove(&p);
        }
        let (gw, gi) = (grid_kind(old), grid_kind(new));
        if gw.is_some() && gw != gi {
            self.removed.push(p);
        }
        if gi.is_some() && gw != gi {
            self.added.push(p);
        }
        if block_of(old) != block_of(new) || is.is_none() {
            self.touched.push(p); // машина рядом с передатчиком появилась, пропала или повернулась
        }
    }

    /// Режим стороны f передатчика p.
    pub(crate) fn mode(&self, p: Pos, f: u8) -> u8 {
        self.modes.get(&p).map_or(M_NORMAL, |&m| ((m >> (2 * f)) & 3) as u8)
    }
    /// Поставить режим стороны (конфигуратор): связи пересчитает фаза сетей (relink).
    pub(crate) fn set_mode(&mut self, p: Pos, f: u8, m: u8) {
        let v = self.modes.get(&p).copied().unwrap_or(0);
        let v = (v & !(3 << (2 * f))) | ((m as u16 & 3) << (2 * f));
        if v == 0 {
            self.modes.remove(&p);
        } else {
            self.modes.insert(p, v);
        }
    }
    /// Цвет транспортёра (0 — без цвета).
    pub(crate) fn color(&self, p: Pos) -> u8 {
        self.colors.get(&p).copied().unwrap_or(0)
    }
    pub(crate) fn set_color(&mut self, p: Pos, c: u8) {
        if c == 0 {
            self.colors.remove(&p);
        } else {
            self.colors.insert(p, c);
        }
    }
    /// Пересвязать передатчик p в фазе сетей (сменились его рукава без смены блока).
    pub(crate) fn relink(&mut self, p: Pos) {
        if self.on {
            self.relinks.push(p);
        }
    }
    /// Приёмники и источники у передатчика p могли смениться (режим стороны к машине) — пересчитать.
    pub(crate) fn touch(&mut self, p: Pos) {
        if self.on {
            self.touched.push(p);
        }
    }

    pub(crate) fn net(&self, id: u32) -> Option<&Net> {
        self.nets.get(id as usize).and_then(|n| n.as_ref())
    }
    fn net_mut(&mut self, id: u32) -> &mut Net {
        self.nets[id as usize].as_mut().expect("сеть")
    }
    /// Номер сети кабеля в p.
    pub(crate) fn net_at(&self, p: Pos) -> Option<u32> {
        self.at.get(&p).map(|m| m.net)
    }
    /// Сети по возрастанию номера.
    pub(crate) fn nets(&self) -> impl Iterator<Item = (u32, &Net)> {
        self.nets.iter().enumerate().filter_map(|(i, n)| n.as_ref().map(|n| (i as u32, n)))
    }
    pub(crate) fn net_count(&self) -> usize {
        self.nets.iter().filter(|n| n.is_some()).count()
    }
    pub(crate) fn cable_count(&self) -> usize {
        self.at.values().filter(|m| m.kind == Kind::Energy).count()
    }

    /// Генератор или куб толкает в кабель cable amount Дж (фаза блок-сущностей): принимает свободное место сети;
    /// ещё не сведённый кабель — 0 (генератор оставит у себя). simulate — только спросить.
    pub(crate) fn insert_energy(&mut self, cable: Pos, amount: u64, simulate: bool) -> u64 {
        let Some(m) = self.at.get(&cable).copied() else { return 0 };
        if m.kind != Kind::Energy {
            return 0;
        }
        let n = self.net_mut(m.net);
        let x = amount.min(n.cap - n.stored.min(n.cap));
        if !simulate {
            n.stored += x;
        }
        x
    }

    /// Машина толкает вещество sub в трубу или трубку pos (насос вверх, сепаратор газы): принимает свободное место сети,
    /// если сеть пуста или с тем же веществом. Ответ — сколько принято.
    pub(crate) fn insert_sub(&mut self, pos: Pos, k: Kind, sub: u8, amount: u64, simulate: bool) -> u64 {
        let Some(m) = self.at.get(&pos).copied() else { return 0 };
        if m.kind != k || sub == 0 {
            return 0;
        }
        let n = self.net_mut(m.net);
        if n.sub != 0 && n.sub != sub {
            return 0;
        }
        let x = amount.min(n.cap - n.stored.min(n.cap));
        if !simulate && x > 0 {
            n.stored += x;
            n.sub = sub;
        }
        x
    }

    /// Вид передатчика сети реестра в p (член сети).
    pub(crate) fn kind_at(&self, p: Pos) -> Option<Kind> {
        self.at.get(&p).map(|m| m.kind)
    }

    fn new_net(&mut self, kind: Kind) -> u32 {
        let net = Net { kind, targets_dirty: true, ..Default::default() };
        if let Some(id) = self.free.pop() {
            self.nets[id as usize] = Some(net);
            id
        } else {
            self.nets.push(Some(net));
            (self.nets.len() - 1) as u32
        }
    }
    fn drop_net(&mut self, id: u32) {
        self.nets[id as usize] = None;
        self.free.push(id);
        // Меньшие номера — первыми: порядок переиспользования детерминирован и не зависит от истории длины списка.
        self.free.sort_unstable_by(|a, b| b.cmp(a));
    }

    /// Фаза сетей, шаг 1: свести правки — снятые кабели, поставленные, приёмники у тронутых клеток.
    pub(crate) fn commit(&mut self, w: &mut dyn GridWorld) {
        if !self.on {
            return;
        }
        for p in std::mem::take(&mut self.removed) {
            if let Some(m) = self.at.get(&p).copied() {
                if grid_kind(w.cell(p)) != Some(m.kind) {
                    self.detach(p);
                }
            }
        }
        let mut re = std::mem::take(&mut self.relinks);
        re.sort_unstable();
        re.dedup();
        for p in re {
            // Единственный член сети уносит её запас с собой: после присоединения — вернуть.
            let mut keep: Option<(u64, u8, u16)> = None;
            let mut own = 0u8;
            if let Some(m) = self.at.get(&p).copied() {
                if let Some(n) = self.net(m.net) {
                    own = n.sub;
                    if n.members.len() == 1 {
                        keep = Some((n.stored, n.sub, n.glow));
                    }
                }
                self.detach(p);
            }
            let c = w.cell(p);
            if let Some(k) = grid_kind(c) {
                self.attach(p, k, arms_of(crate::cell::state_of(c)), own);
                if let (Some((stored, sub, glow)), Some(m)) = (keep, self.at.get(&p).copied()) {
                    let n = self.net_mut(m.net);
                    if n.sub == 0 || n.sub == sub {
                        n.stored = (n.stored + stored).min(n.cap);
                        if n.kind != Kind::Energy && stored > 0 {
                            n.sub = sub;
                        }
                        n.glow = n.glow.max(glow);
                    }
                }
            }
        }
        for p in std::mem::take(&mut self.added) {
            let c = w.cell(p);
            if let (false, Some(k)) = (self.at.contains_key(&p), grid_kind(c)) {
                self.attach(p, k, arms_of(crate::cell::state_of(c)), 0);
            }
        }
        for p in std::mem::take(&mut self.touched) {
            for f in 0..6u8 {
                if let Some(m) = self.at.get(&offset(p, f)).copied() {
                    self.net_mut(m.net).targets_dirty = true;
                }
            }
            if let Some(m) = self.at.get(&p).copied() {
                self.net_mut(m.net).targets_dirty = true;
            }
        }
        for id in 0..self.nets.len() as u32 {
            if self.net(id).is_some_and(|n| n.targets_dirty) {
                let (members, kind) = self.net(id).map(|n| (n.members.clone(), n.kind)).unwrap();
                let mut t: Vec<Pos> = Vec::new();
                let mut pulls: Vec<(Pos, u8)> = Vec::new();
                for c in members {
                    let a = arms_of(crate::cell::state_of(w.cell(c)));
                    for f in 0..6u8 {
                        let q = offset(c, f);
                        let code = arm(a, f);
                        if self.at.contains_key(&q) || code == ARM_NONE {
                            continue;
                        }
                        if code == ARM_PULL {
                            if kind != Kind::Energy {
                                pulls.push((q, f ^ 1));
                            }
                        } else if w.accepts(q, f ^ 1, kind) {
                            t.push(q);
                        }
                    }
                }
                t.sort_unstable();
                t.dedup();
                pulls.sort_unstable();
                pulls.dedup();
                let n = self.net_mut(id);
                n.targets = t;
                n.pulls = pulls;
                n.targets_dirty = false;
            }
        }
    }

    /// Поставленный передатчик вида k с рукавами arms: соседние сети того же вида, с которыми есть рукав (и, у жидкости и
    /// газа, совместимым веществом), — ни одной (новая), одна (вступить) или несколько (слить в самую большую). own —
    /// вещество, с которым передатчик уже был (пересвязка): чужие сети он не берёт.
    fn attach(&mut self, p: Pos, k: Kind, arms: u16, own: u8) {
        let mut found: Vec<u32> = Vec::new();
        let mut links = 0u8;
        let mut sub = own;
        for f in 0..6u8 {
            if arm(arms, f) == ARM_NONE {
                continue;
            }
            let Some(m) = self.at.get(&offset(p, f)).copied() else { continue };
            if m.kind != k {
                continue;
            }
            let s = self.net(m.net).map_or(0, |n| n.sub);
            if s != 0 && sub != 0 && s != sub {
                continue; // другое вещество — не сливаемся, пока одна не опустеет
            }
            if s != 0 {
                sub = s;
            }
            links |= 1 << f;
            if !found.contains(&m.net) {
                found.push(m.net);
            }
        }
        let id = match found.len() {
            0 => self.new_net(k),
            1 => found[0],
            _ => self.merge(&found),
        };
        for f in 0..6u8 {
            if links >> f & 1 != 0 {
                if let Some(m) = self.at.get_mut(&offset(p, f)) {
                    m.links |= 1 << (f ^ 1);
                }
            }
        }
        self.at.insert(p, Member { net: id, kind: k, links });
        let n = self.net_mut(id);
        n.members.push(p);
        n.cap += unit_cap(k);
        if found.is_empty() {
            n.glow = 0; // новая сеть: свечение = заполнение (0) — разгорится, когда пойдёт поток
        }
        n.targets_dirty = true;
        self.layout_changed = true;
    }

    /// Слить сети в самую большую (по числу кабелей; при равенстве — меньший номер): запасы складываются, свечение —
    /// среднее по ёмкости (MEK-02 §6.2, произведения целые).
    fn merge(&mut self, ids: &[u32]) -> u32 {
        let keep = *ids.iter().max_by(|&&a, &&b| {
            let (sa, sb) = (self.net(a).map_or(0, |n| n.members.len()), self.net(b).map_or(0, |n| n.members.len()));
            sa.cmp(&sb).then(b.cmp(&a))
        }).unwrap();
        let (mut cap, mut glow_w) = (0u64, 0u64);
        for &id in ids {
            let n = self.net(id).unwrap();
            cap += n.cap;
            glow_w += n.cap * n.glow as u64;
        }
        for &id in ids {
            if id == keep {
                continue;
            }
            let other = self.nets[id as usize].take().unwrap();
            self.free.push(id);
            for &m in &other.members {
                if let Some(x) = self.at.get_mut(&m) {
                    x.net = keep;
                }
            }
            let k = self.net_mut(keep);
            k.members.extend(other.members);
            k.cap += other.cap;
            k.stored += other.stored;
            k.sent |= other.sent;
            if k.sub == 0 {
                k.sub = other.sub;
            }
        }
        self.free.sort_unstable_by(|a, b| b.cmp(a));
        let k = self.net_mut(keep);
        k.glow = if cap == 0 { 0 } else { (glow_w / cap) as u16 };
        k.stored = k.stored.min(k.cap);
        k.targets_dirty = true;
        keep
    }

    /// Снятый кабель: вычесть; запас — не больше ёмкости оставшихся (лишнее пропадает); последний — сеть пропадает со
    /// всем запасом; при ≥ 2 соседях-кабелях — разрез (части по ёмкости, номер у большей, свечение наследуется).
    fn detach(&mut self, p: Pos) {
        let Some(m) = self.at.remove(&p) else { return };
        let mut seeds: Vec<Pos> = Vec::new();
        for f in 0..6u8 {
            if m.links >> f & 1 == 0 {
                continue;
            }
            let q = offset(p, f);
            if let Some(x) = self.at.get_mut(&q) {
                x.links &= !(1 << (f ^ 1));
                if x.net == m.net {
                    seeds.push(q);
                }
            }
        }
        let n = self.net_mut(m.net);
        if let Some(i) = n.members.iter().position(|&x| x == p) {
            n.members.swap_remove(i);
        }
        n.cap -= unit_cap(m.kind);
        n.targets_dirty = true;
        n.stored = n.stored.min(n.cap);
        let empty = n.members.is_empty();
        self.layout_changed = true;
        if empty {
            self.drop_net(m.net);
            return;
        }
        if seeds.len() >= 2 {
            self.split(m.net, &seeds);
        }
    }

    /// Разрез сети id после снятия кабеля: части — обходом в ширину от каждого соседа.
    fn split(&mut self, id: u32, seeds: &[Pos]) {
        let mut parts: Vec<Vec<Pos>> = Vec::new();
        let mut seen: HashSet<Pos> = HashSet::new();
        for &s in seeds {
            if seen.contains(&s) {
                continue;
            }
            let mut part = vec![s];
            seen.insert(s);
            let mut k = 0;
            while k < part.len() {
                let c = part[k];
                k += 1;
                let links = self.at.get(&c).map_or(0, |m| m.links);
                for f in 0..6u8 {
                    let q = offset(c, f);
                    if links >> f & 1 != 0 && !seen.contains(&q) && self.at.get(&q).is_some_and(|x| x.net == id) {
                        seen.insert(q);
                        part.push(q);
                    }
                }
            }
            parts.push(part);
        }
        if parts.len() < 2 {
            return;
        }
        // Номер остаётся у большей части (при равенстве — у первой по порядку соседей).
        let keep = (0..parts.len()).max_by(|&a, &b| parts[a].len().cmp(&parts[b].len()).then(b.cmp(&a))).unwrap();
        let (stored, glow, sent, cap, kind, sub) = {
            let n = self.net(id).unwrap();
            (n.stored, n.glow, n.sent, n.cap, n.kind, n.sub)
        };
        let unit = unit_cap(kind);
        let mut given = 0u64;
        for (i, part) in parts.iter_mut().enumerate() {
            if i == keep {
                continue;
            }
            let pcap = part.len() as u64 * unit;
            let share = if cap == 0 { 0 } else { (stored as u128 * pcap as u128 / cap as u128) as u64 };
            given += share;
            part.sort_unstable();
            let nid = self.new_net(kind);
            for &c in part.iter() {
                if let Some(x) = self.at.get_mut(&c) {
                    x.net = nid;
                }
            }
            let n = self.net_mut(nid);
            n.members = std::mem::take(part);
            n.cap = pcap;
            n.stored = share;
            n.sub = if share > 0 { sub } else { 0 };
            n.glow = glow;
            n.sent = sent;
            n.targets_dirty = true;
        }
        let mut kept = std::mem::take(&mut parts[keep]);
        kept.sort_unstable();
        let n = self.net_mut(id);
        n.cap = kept.len() as u64 * unit;
        n.members = kept;
        n.stored = stored - given;
        if n.stored == 0 && kind != Kind::Energy {
            n.sub = 0;
        }
        n.targets_dirty = true;
    }

    /// Фаза сетей, шаг 2: каждая сеть по возрастанию номера — свечение, затем раздача всего запаса приёмникам в зоне.
    pub(crate) fn tick(&mut self, w: &mut dyn GridWorld) {
        if !self.on {
            return;
        }
        let mut remerge: Vec<Pos> = Vec::new();
        for id in 0..self.nets.len() {
            let Some(n) = self.nets[id].as_mut() else { continue };
            let kind = n.kind;
            // Жидкость, газ: стороны PULL тянут из соседних баков до 250 / 750 мБ/т каждая, пока есть место.
            if kind != Kind::Energy && !n.pulls.is_empty() {
                let rate = if kind == Kind::Fluid { PIPE_PULL } else { TUBE_PULL };
                let pulls = n.pulls.clone();
                for (q, face) in pulls {
                    let n = self.nets[id].as_mut().unwrap();
                    let free = n.cap - n.stored.min(n.cap);
                    if free == 0 {
                        break;
                    }
                    if !w.in_zone(q) {
                        continue;
                    }
                    let want = n.sub;
                    let (s, x) = w.extract(q, face, kind, want, rate.min(free));
                    let n = self.nets[id].as_mut().unwrap();
                    if x > 0 && (want == 0 || s == want) {
                        n.stored += x;
                        n.sub = s;
                    }
                }
            }
            let n = self.nets[id].as_mut().unwrap();
            let had = n.sub;
            // Свечение (EnergyNetwork.computeContentScale): не ниже заполнения; поток — +0.02, нет — −0.02 до заполнения.
            let f = if n.cap == 0 { 1000 } else { (n.stored.min(n.cap) as u128 * 1000 / n.cap as u128) as u16 };
            let mut r = n.glow.max(f);
            if n.sent && r < 1000 {
                r = (r + GLOW_STEP).min(1000);
            } else if !n.sent && r > 0 {
                r = r.saturating_sub(GLOW_STEP).max(f);
            }
            n.glow = r;
            let targets: Vec<Pos> = n.targets.iter().copied().filter(|&t| w.in_zone(t)).collect();
            let stored = n.stored;
            let sub = n.sub;
            let mut sent = 0;
            if stored > 0 && !targets.is_empty() && (kind == Kind::Energy || sub != 0) {
                let req: Vec<u64> = targets.iter().map(|&t| w.room(t, kind, sub)).collect();
                let give = split_fair(stored, &req);
                for (k, &t) in targets.iter().enumerate() {
                    if give[k] > 0 {
                        sent += w.give(t, kind, sub, give[k]);
                    }
                }
            }
            let n = self.nets[id].as_mut().unwrap();
            n.stored -= sent;
            n.sent = sent > 0;
            n.flow = sent;
            // Пустая сеть без потока забывает вещество (во время потока — помнит: lastFluid мода).
            if kind != Kind::Energy && n.stored == 0 && !n.sent {
                n.sub = 0;
                if had != 0 {
                    remerge.extend(n.members.iter().copied()); // опустела: можно слиться с соседней сетью другого вещества
                }
            }
        }
        // Опустевшие сети жидкости и газа: члены, у которых рукав к члену чужой сети того же вида, — пересвязать.
        for p in remerge {
            let Some(m) = self.at.get(&p).copied() else { continue };
            let a = arms_of(crate::cell::state_of(w.cell(p)));
            let other = (0..6u8).any(|f| {
                arm(a, f) != ARM_NONE && m.links >> f & 1 == 0 && self.at.get(&offset(p, f)).is_some_and(|x| x.kind == m.kind && x.net != m.net)
            });
            if other {
                self.relinks.push(p);
            }
        }
    }

    /// Свечение по сетям для клиента: (наименьший член, свечение | вещество << 10) по возрастанию номера сети. У
    /// энергии вещества нет — число прежнее.
    pub(crate) fn glow_list(&self) -> Vec<(Pos, u16)> {
        self.nets().map(|(_, n)| (n.anchor(), n.glow.min(1000) | (n.sub as u16 & 63) << 10)).collect()
    }

    // ---------------------------------------------------------------- сохранение (одна запись на сеть)

    /// Сети с запасом: (любой член — наименьший, запас).
    pub(crate) fn saved(&self) -> Vec<(Pos, u64)> {
        self.nets().filter(|(_, n)| n.stored > 0 && n.kind == Kind::Energy).map(|(_, n)| (n.anchor(), n.stored)).collect()
    }
    /// Сети жидкости и газа с запасом: (наименьший член, вещество, мБ) — запись logistics (save).
    pub(crate) fn saved_subs(&self) -> Vec<(Pos, u8, u64)> {
        self.nets().filter(|(_, n)| n.stored > 0 && n.kind != Kind::Energy).map(|(_, n)| (n.anchor(), n.sub, n.stored)).collect()
    }
    /// После загрузки и commit: вещество в сеть члена (с обрезкой по ёмкости), свечение = заполнение.
    pub(crate) fn restore_subs(&mut self, list: &[(Pos, u8, u64)]) {
        for &(p, sub, v) in list {
            if let Some(m) = self.at.get(&p).copied() {
                let n = self.net_mut(m.net);
                if n.kind == Kind::Energy || (n.sub != 0 && n.sub != sub) {
                    continue;
                }
                n.sub = sub;
                n.stored = (n.stored + v).min(n.cap);
                n.glow = (n.stored as u128 * 1000 / n.cap.max(1) as u128) as u16;
            }
        }
    }
    /// После загрузки и commit: положить запас в сеть члена (с обрезкой по ёмкости), свечение = заполнение.
    pub(crate) fn restore(&mut self, list: &[(Pos, u64)]) {
        for &(p, e) in list {
            if let Some(m) = self.at.get(&p).copied().filter(|m| m.kind == Kind::Energy) {
                let n = self.net_mut(m.net);
                n.stored = (n.stored + e).min(n.cap);
                n.glow = (n.stored as u128 * 1000 / n.cap.max(1) as u128) as u16;
            }
        }
    }
    /// Передатчик снова в очередь присоединения (загрузка мира: передатчики пришли правками мимо set_cell).
    pub(crate) fn queue_cable(&mut self, p: Pos) {
        if self.on {
            self.added.push(p);
        }
    }
    /// Отладка F3: [сетей, кабелей, приёмников, запас, ёмкость, поток, свечение] сети кабеля p.
    pub(crate) fn debug_at(&self, p: Pos) -> Option<[u64; 7]> {
        let id = self.net_at(p)?;
        let n = self.net(id)?;
        Some([id as u64, n.members.len() as u64, n.targets.len() as u64, n.stored, n.cap, n.flow, n.glow as u64])
    }
}
