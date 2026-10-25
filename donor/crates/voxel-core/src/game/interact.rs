//! Взаимодействие с блоками: луч по обводкам (ASP-04 §2), ломание — машина состояний ClientPlayerInteractionManager
//! (ASP-04 §4), ставление ПКМ с состояниями (ASP-05 §1–3: ось бревна, печь и сундук лицом к игроку, факел на пол или
//! стену, лестница, снежный слой, листва игрока) и использование блока (окна, ASP-09/10), выбор блока средней кнопкой
//! (doItemPick). Что делает блок — таблица (registry/blocks.rs: place, on_use, support); запись в мир по правилам игры
//! (соседи, тики, опад, рассыпание) — Sim::set_world (sim.rs).
//!
//! Сеть (MP-0, design.md §2.2): действия — свободные функции над контекстом ActCtx в двух режимах. Клиент выполняет их
//! в режиме Predict над своим зеркалом мира (ощущение — без задержки), сервер — те же функции в режиме Auth над своим
//! миром (решение). Машина ломания и ввод фазы тика — у клиента (client.rs), решение по действиям — у сервера
//! (server.rs).

use std::collections::HashMap;

use crate::abi::{EV_BROKEN, EV_PLACED, EV_TOOL_BROKE};
use crate::cell::{block_of, cell_of, offset, BlockId, Cell, State, DIRS, F_DOWN, F_EAST, F_NORTH, F_SOUTH, F_UP, F_WEST, H};
use crate::game::entity::FallingBlock;
use crate::game::fluids::{fluid_height, fluid_of};
use crate::game::inventory::Inventory;
use crate::game::player::Player;
use crate::game::screen::Menu;
use crate::game::sim::Sim;
use crate::game::view::{Events, To};
use crate::registry::blocks::{
    BeKind,
    bdef, collision_of, face_full, facing_of, outline_at, plant_supported, replaceable_by, snow_layers, solid_block,
    top_center, OnUse, Place, Shape, Support, HOE, ST_PERSIST, bounding_dy, main_of, multiblock_parts,
};
use crate::registry::ids::{AIR, BOUNDING, CACTUS, SAND, WATER};
use crate::registry::items::{block_item, break_delta, can_harvest, idef, ItemId, ItemKind};
use crate::registry::shapes::Aabb;
use crate::world::stack::ItemStack;
use crate::world::WorldData;

/// Порядок обновления формы соседей (AbstractBlock.DIRECTIONS): W, E, N, S, D, U.
pub(crate) const UPDATE_ORDER: [u8; 6] = [F_WEST, F_EAST, F_NORTH, F_SOUTH, F_DOWN, F_UP];

/// Дальность удара и ПКМ по сущности (entity_interaction_range).
pub(crate) const ENTITY_REACH: f64 = 3.0;
pub(crate) const REACH: f64 = 4.5; // player.block_interaction_range
pub(crate) const REACH_CREATIVE: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    pub(crate) pos: [i32; 3],
    pub(crate) face: u8,
    pub(crate) point: [f64; 3],
    /// Начало луча уже внутри формы (голова в высокой траве).
    pub(crate) inside: bool,
}

/// Грань, в которую смотрит вектор (Direction.getFacing): наибольшая проекция, порядок D, U, N, S, W, E.
pub(crate) fn facing(d: [f64; 3]) -> u8 {
    let mut best = (f32::MIN_POSITIVE, F_NORTH);
    for f in 0..6u8 {
        let v = DIRS[f as usize];
        let g = (d[0] * v[0] as f64 + d[1] * v[1] as f64 + d[2] * v[2] as f64) as f32;
        if g > best.0 {
            best = (g, f);
        }
    }
    best.1
}

/// Попадание отрезка a→b в форму клетки (VoxelShape.raycast + Box.raycast); коробки — в мировых координатах.
pub(crate) fn hit_boxes(cell: [i32; 3], boxes: &[Aabb], a: [f64; 3], b: [f64; 3]) -> Option<Hit> {
    if boxes.is_empty() {
        return None;
    }
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1e-7 {
        return None;
    }
    let p = [a[0] + d[0] * 0.001, a[1] + d[1] * 0.001, a[2] + d[2] * 0.001];
    if boxes.iter().any(|s| (0..3).all(|k| p[k] >= s.lo[k] && p[k] < s.hi[k])) {
        return Some(Hit { pos: cell, face: facing(d) ^ 1, point: p, inside: true });
    }
    let mut best_t = 1.0;
    let mut best_face = None;
    for s in boxes {
        for k in 0..3 {
            // Плоскость входа: min при движении +, max при −; грань входа смотрит навстречу лучу.
            let (plane, face) = if d[k] > 1e-7 {
                (s.lo[k], [F_WEST, F_DOWN, F_NORTH][k])
            } else if d[k] < -1e-7 {
                (s.hi[k], [F_EAST, F_UP, F_SOUTH][k])
            } else {
                continue;
            };
            let t = (plane - a[k]) / d[k];
            if !(0.0 < t && t < best_t) {
                continue;
            }
            let ok = (0..3).filter(|&j| j != k).all(|j| {
                let c = a[j] + t * d[j];
                s.lo[j] - 1e-7 < c && c < s.hi[j] + 1e-7
            });
            if ok {
                best_t = t;
                best_face = Some(face);
            }
        }
    }
    best_face.map(|face| Hit { pos: cell, face, inside: false, point: [a[0] + best_t * d[0], a[1] + best_t * d[1], a[2] + best_t * d[2]] })
}

/// Цель взгляда (BlockView.raycast, ShapeType.OUTLINE, жидкости насквозь): DDA по клеткам на отрезке, удлинённом на
/// 1e-7; годится, только если точка попадания строго ближе reach (GameRenderer.ensureTargetInRange).
pub(crate) fn raycast(w: &mut WorldData, eye: [f64; 3], dir: [f64; 3], reach: f64, offsets: bool) -> Option<Hit> {
    raycast_ex(w, eye, dir, reach, offsets, false)
}

/// Тот же луч; sources — луч пустого ведра (FluidHandling.SOURCE_ONLY): к обводкам блоков добавляется коробка источника
/// жидкости 0…высота (текучая и падающая жидкость не видна).
pub(crate) fn raycast_ex(w: &mut WorldData, eye: [f64; 3], dir: [f64; 3], reach: f64, offsets: bool, sources: bool) -> Option<Hit> {
    let end = [eye[0] + dir[0] * reach, eye[1] + dir[1] * reach, eye[2] + dir[2] * reach];
    if end == eye {
        return None;
    }
    let lerp = |t: f64, x: f64, y: f64| x + t * (y - x);
    let s: [f64; 3] = std::array::from_fn(|k| lerp(-1e-7, eye[k], end[k]));
    let e: [f64; 3] = std::array::from_fn(|k| lerp(-1e-7, end[k], eye[k]));
    let mut boxes = Vec::with_capacity(2);
    let mut test = |c: [i32; 3], w: &mut WorldData| {
        boxes.clear();
        let cell = w.get_cell(c[0], c[1], c[2]);
        outline_at(cell, c[0], c[1], c[2], offsets, &mut boxes);
        if sources && fluid_of(cell).is_some_and(|f| f.still) {
            let h = fluid_height(cell, w.get_cell(c[0], c[1] + 1, c[2]));
            let o = [c[0] as f64, c[1] as f64, c[2] as f64];
            boxes.push(Aabb { lo: o, hi: [o[0] + 1.0, o[1] + h, o[2] + 1.0] });
        }
        hit_boxes(c, &boxes, eye, end)
    };
    let mut c = [s[0].floor() as i32, s[1].floor() as i32, s[2].floor() as i32];
    let mut hit = test(c, w);
    if hit.is_none() {
        let d: [f64; 3] = std::array::from_fn(|k| e[k] - s[k]);
        let step: [i32; 3] = std::array::from_fn(|k| if d[k] > 0.0 { 1 } else if d[k] < 0.0 { -1 } else { 0 });
        let td: [f64; 3] = std::array::from_fn(|k| if step[k] == 0 { f64::MAX } else { step[k] as f64 / d[k] });
        let mut tm: [f64; 3] = std::array::from_fn(|k| {
            let fr = s[k] - s[k].floor();
            td[k] * if step[k] > 0 { 1.0 - fr } else { fr }
        });
        while hit.is_none() && (tm[0] <= 1.0 || tm[1] <= 1.0 || tm[2] <= 1.0) {
            // Ничья: X уступает Y и Z, Y уступает Z.
            let k = if tm[0] < tm[1] {
                if tm[0] < tm[2] {
                    0
                } else {
                    2
                }
            } else if tm[1] < tm[2] {
                1
            } else {
                2
            };
            c[k] += step[k];
            tm[k] += td[k];
            hit = test(c, w);
        }
    }
    hit.filter(|h| (0..3).map(|k| (h.point[k] - eye[k]).powi(2)).sum::<f64>() < reach * reach)
}

/// Мешает ли сущность с коробкой ent форме блока s (CollisionView.canPlace): перекрытие больше 1e-7 по каждой оси
/// (точки ближе 1e-7 сливаются — касание и микроперекрытие не считаются).
pub(crate) fn blocks_placement(s: &Aabb, ent: &Aabb) -> bool {
    (0..3).all(|a| s.hi[a].min(ent.hi[a]) - s.lo[a].max(ent.lo[a]) > 1e-7)
}

/// Состояние ломания (ClientPlayerInteractionManager).
pub(crate) struct Mining {
    /// Последняя начатая позиция; не сбрасывается ни при поломке, ни при отмене.
    pub(crate) pos: [i32; 3],
    /// Предмет в руке на начало (тип + износ, без количества).
    pub(crate) tool: (ItemId, u16),
    /// Именно f32: бревно рукой — 61 тик.
    pub(crate) progress: f32,
    pub(crate) sound: f32,
    /// 5 тиков после обычной поломки; убывает только при удержании на блоке.
    pub(crate) cooldown: i32,
    pub(crate) breaking: bool,
    /// Стадия трещин (setBlockBreakingInfo): 0…9 или −1.
    pub(crate) stage: i32,
}

impl Mining {
    pub(crate) fn new() -> Mining {
        Mining { pos: [-1, -1, -1], tool: (0, 0), progress: 0.0, sound: 0.0, cooldown: 0, breaking: false, stage: -1 }
    }
    pub(crate) fn stage_now(&self) -> i32 {
        if self.progress > 0.0 {
            (self.progress * 10.0) as i32
        } else {
            -1
        }
    }
}

/// Итог ПКМ по блоку (ActionResult): SUCCESS — махнуть, PASS — дальше «в воздух», FAIL — выход.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Use {
    Swing,
    Pass,
    Fail,
}

/// Предсказанные записи клиента (Minecraft PendingUpdateManager): клетка → номер предсказания, ячейка до него и
/// последняя ячейка, присланная сервером, пока предсказание не решено. Ack{seq} решает все до seq: в клетку ставится
/// присланное (или прежнее, если сервер клетку не трогал) — совпало с предсказанием, значит, отката не было.
#[derive(Default)]
pub(crate) struct Pending {
    pub(crate) cells: HashMap<[i32; 3], PendingCell>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingCell {
    pub(crate) seq: u32,
    pub(crate) old: Cell,
    pub(crate) server: Option<Cell>,
}

impl Pending {
    /// Запомнить предсказание seq в клетке p (ячейка до первого из нерешённых — old).
    pub(crate) fn record(&mut self, p: [i32; 3], seq: u32, old: Cell) {
        self.cells.entry(p).and_modify(|e| e.seq = seq).or_insert(PendingCell { seq, old, server: None });
    }
}

/// Режим действия (design.md §2.2): одна и та же функция действия у клиента предсказывает, у сервера решает.
pub(crate) enum Mode<'a> {
    /// Клиент: запись в своё зеркало — сырая, помечена номером seq (откат по Ack). Предметы не создаются, ГСЧ дропа не
    /// тратится, тики не планируются, соседи не будятся — последствия (растение сверху, песок, дроп) придут с сервера.
    /// falling — зеркало падающих блоков («не в себя»).
    Predict { seq: u32, pending: &'a mut Pending, falling: &'a [FallingBlock] },
    /// Сервер: запись по правилам игры (Sim::set_world), дроп, тики. События действия — всем, кроме самого игрока actor
    /// (он их уже предсказал); последствия в мире — всем.
    Auth { sim: &'a mut Sim, actor: u32 },
}

/// Контекст действия игрока над миром: мир этой стороны, тело (позиция — на конец прошлого тика, взгляд — этого),
/// инвентарь, приёмник событий, режим. Что действие просит сделать после себя (открыть окно блока, показать сообщение
/// над хотбаром), — в полях open и height_msg: окно открывает только сервер (клиент лишь машет рукой и ждёт WinOpen),
/// сообщение показывает только клиент.
pub(crate) struct ActCtx<'a> {
    pub(crate) w: &'a mut WorldData,
    pub(crate) player: &'a Player,
    pub(crate) inv: &'a mut Inventory,
    pub(crate) ev: &'a mut Events,
    pub(crate) mode: Mode<'a>,
    /// Тела других игроков («не в себя» — и не в них; сервер). Клиент их не проверяет: решит сервер.
    pub(crate) bodies: &'a [Aabb],
    pub(crate) open: Option<Menu>,
    pub(crate) height_msg: bool,
    /// Запись упёрлась в защиту Перекрёстка (gen/spawn.rs): клиент покажет сообщение над хотбаром.
    pub(crate) zone_msg: bool,
    /// Нажата кнопка неба (G15, gen/spawn.rs sky_button): день (true) или ночь — у клиента меняется небо нажавшего.
    pub(crate) sky: Option<bool>,
}

impl<'a> ActCtx<'a> {
    pub(crate) fn new(w: &'a mut WorldData, player: &'a Player, inv: &'a mut Inventory, ev: &'a mut Events, mode: Mode<'a>) -> ActCtx<'a> {
        ActCtx { w, player, inv, ev, mode, bodies: &[], open: None, height_msg: false, zone_msg: false, sky: None }
    }
    /// Творческий режим (G17) и отладочный полёт сквозь стены ломают и ставят как креатив (ASP-04 §11.15): мгновенно,
    /// дальность 5, без дропа и расхода (game/rules.rs). Полёт выживания (F) — как обычно.
    pub(crate) fn creative(&self) -> bool {
        crate::game::rules::creative(self.player)
    }
    /// Событие самого действия: у клиента — себе, у сервера — всем, кроме игрока.
    pub(crate) fn emit(&mut self, code: i32, p: [i32; 3], arg: i32) {
        match self.mode {
            Mode::Predict { .. } => self.ev.push(code, p, arg),
            Mode::Auth { actor, .. } => self.ev.push_to(To::Except(actor), code, p, arg),
        }
    }
    /// Записать ячейку: сервер — по правилам игры, клиент — сырой записью с номером предсказания. false — вне высоты или
    /// ячейка та же.
    pub(crate) fn write(&mut self, pos: [i32; 3], cell: Cell) -> bool {
        // Перекрёсток: игрок не меняет там ничего — ни ломанием, ни ставлением, ни ведром (spawn.md §5).
        if protected(self.w, pos) {
            self.zone_msg = true;
            return false;
        }
        match &mut self.mode {
            Mode::Auth { sim, actor } => {
                let old = if (0..H).contains(&pos[1]) { self.w.get_cell(pos[0], pos[1], pos[2]) } else { 0 };
                let done = sim.set_world(self.w, pos, cell, self.ev);
                if done {
                    // Журнал правок игроков (/rollback): что было и что стало.
                    let now = self.w.get_cell(pos[0], pos[1], pos[2]);
                    sim.by_player.push((*actor, pos, old, now));
                }
                done
            }
            Mode::Predict { seq, pending, .. } => {
                if pos[1] < 0 || pos[1] >= H {
                    return false;
                }
                let old = self.w.get_cell(pos[0], pos[1], pos[2]);
                if old == cell {
                    return false;
                }
                pending.record(pos, *seq, old);
                self.w.set_cell(pos[0], pos[1], pos[2], cell);
                true
            }
        }
    }
    /// Дроп блока, сломанного игроком (только сервер).
    pub(crate) fn drop_loot(&mut self, cell: Cell, pos: [i32; 3]) {
        if let Mode::Auth { sim, .. } = &mut self.mode {
            sim.drop_loot(cell, pos, true);
        }
    }
    /// Поставленная машина Mekanism из предмета с данными (номер в damage стека): энергия, слоты, бак — только сервер.
    /// В творческом стек не тратится — данные копируются.
    pub(crate) fn place_machine_data(&mut self, at: [i32; 3], id: u16) {
        if id == 0 {
            return;
        }
        let creative = self.creative();
        if let Mode::Auth { sim, .. } = &mut self.mode {
            let d = if creative { sim.mek_items.get(id).cloned() } else { sim.mek_items.take(id) };
            if let (Some(d), Some(m)) = (d, self.w.be_mut().mek_mut(at)) {
                d.apply(m);
            }
        }
    }
    /// Летящие падающие блоки этой стороны.
    pub(crate) fn falling(&self) -> &[FallingBlock] {
        match &self.mode {
            Mode::Predict { falling, .. } => falling,
            Mode::Auth { sim, .. } => &sim.falling,
        }
    }
}

/// Клетка в защищённой зоне Перекрёстка (gen/spawn.rs: цилиндр по всей высоте): ломать, ставить и записывать в неё
/// игроку нельзя. Одна проверка для клиента (предсказание отказа) и сервера (Auth).
#[inline]
pub(crate) fn protected(w: &WorldData, pos: [i32; 3]) -> bool {
    w.gen().site.protects(pos[0], pos[2])
}

/// Клетка ступней игрока (для зоны Перекрёстка).
#[inline]
pub(crate) fn feet(p: &Player) -> [i32; 3] {
    [p.pos[0].floor() as i32, p.pos[1].floor() as i32, p.pos[2].floor() as i32]
}

/// Предмет в руке на начало ломания (тип + износ, без количества).
pub(crate) fn tool_key(inv: &Inventory) -> (ItemId, u16) {
    let h = inv.hand();
    (h.id(), if h.is_empty() { 0 } else { h.damage })
}

/// Глаза под водой (ломание медленнее в 5 раз).
pub(crate) fn eyes_in_water(w: &mut WorldData, p: &Player) -> bool {
    let ey = p.pos[1] + p.eye_height() as f64 - 0.11111111f32 as f64;
    let (x, y, z) = (p.pos[0].floor() as i32, ey.floor() as i32, p.pos[2].floor() as i32);
    let c = w.get_cell(x, y, z);
    if block_of(c) != WATER {
        return false;
    }
    let h = fluid_height(c, w.get_cell(x, y + 1, z)) as f32;
    (y as f32 + h) as f64 > ey
}

/// Прогресс ломания блока b за тик (delta каждый тик заново: прыжок, вода).
pub(crate) fn mine_delta(w: &mut WorldData, p: &Player, inv: &Inventory, b: BlockId) -> f32 {
    let water = eyes_in_water(w, p);
    break_delta(inv.hand().id(), bdef(b), p.on_ground, water)
}

/// Поломка игроком: событие 2001, удаление с обновлением соседей (содержимое печи и сундука рассыпается здесь —
/// до дропа самого блока), износ (до него — canHarvest), дроп.
pub(crate) fn break_block(c: &mut ActCtx, pos: [i32; 3]) {
    // Многоблок (MEK-03 §3.5): любая часть ломается как основание — его дроп, инструмент и износ.
    let pos = main_of(c.w.get_cell(pos[0], pos[1], pos[2]), pos);
    let cell = c.w.get_cell(pos[0], pos[1], pos[2]);
    let b = block_of(cell);
    if b == AIR {
        return;
    }
    if protected(c.w, pos) {
        c.zone_msg = true;
        return;
    }
    let hand = c.inv.hand();
    let def = bdef(b);
    let harvest = can_harvest(hand.id(), def);
    c.emit(EV_BROKEN, pos, b as i32);
    c.write(pos, AIR as Cell);
    remove_parts(c, pos, b);
    if c.creative() {
        return;
    }
    // Износ (Item.postMine): инструмент −1, меч −2 за блок с твёрдостью ≠ 0.
    let per_block = match idef(hand.id()).kind {
        ItemKind::Tool { .. } | ItemKind::Shears => 1,
        ItemKind::Sword { .. } => 2,
        _ => 0,
    };
    if per_block > 0 && def.hardness != 0.0 {
        let max = idef(hand.id()).max_damage;
        let h = c.inv.hand_mut();
        h.damage += per_block;
        if h.damage >= max {
            h.count -= 1;
            if h.count <= 0 {
                *h = ItemStack::EMPTY;
            }
            c.emit(EV_TOOL_BROKE, pos, hand.id() as i32);
        }
    }
    if harvest {
        c.drop_loot(cell, pos);
    }
}

/// Снять части многоблока над убранным основанием b в pos (у клиента — предсказание, у сервера Sim::set_world снимет и
/// сам: части без основания не живут).
fn remove_parts(c: &mut ActCtx, pos: [i32; 3], b: BlockId) {
    for dy in multiblock_parts(b) {
        let q = [pos[0], pos[1] + dy, pos[2]];
        let pc = c.w.get_cell(q[0], q[1], q[2]);
        if block_of(pc) == BOUNDING && bounding_dy(pc) == dy {
            c.write(q, AIR as Cell);
        }
    }
}

/// interactBlock: сначала использование блока (onUse — только главной рукой; Shift, когда хоть в одной руке
/// что-то есть, отменяет), потом предмет. Верстак, печь — окно (взмах, без звука); сундук — окно, если сверху не
/// сплошной блок (иначе только взмах). Окно открывает сервер (c.open), клиент только машет.
pub(crate) fn interact_block(c: &mut ActCtx, hand: usize, h: Hit) -> Use {
    // Конфигуратор и улучшения Mekanism по машине — до окна блока (game/mek_tool.rs).
    if let Some(r) = crate::game::mek_tool::use_on_machine(c, hand, h) {
        return r;
    }
    let stack = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let holding = !c.inv.hand().is_empty() || !c.inv.offhand.is_empty();
    if hand == 0 && !(c.player.sneaking() && holding) {
        // Кнопка неба на Перекрёстке (G15): как кнопка Minecraft — раньше ставления; мир не меняется ни у кого.
        if let Some(day) = c.w.gen().site.sky_button(h.pos) {
            c.sky = Some(day);
            return Use::Swing;
        }
        match bdef(c.w.get_block(h.pos[0], h.pos[1], h.pos[2])).on_use {
            OnUse::Crafting => {
                c.open = Some(Menu::crafting(h.pos));
                return Use::Swing;
            }
            OnUse::Furnace => {
                c.open = Some(Menu::furnace(h.pos));
                return Use::Swing;
            }
            OnUse::Machine => {
                // ПКМ по части многоблока — окно основания.
                let at = main_of(c.w.get_cell(h.pos[0], h.pos[1], h.pos[2]), h.pos);
                if let Some(BeKind::Mek(k)) = bdef(c.w.get_block(at[0], at[1], at[2])).be {
                    c.open = Some(Menu::mek(at, k));
                }
                return Use::Swing;
            }
            OnUse::Chest => {
                if !solid_block(c.w.get_cell(h.pos[0], h.pos[1] + 1, h.pos[2])) {
                    c.open = Some(Menu::chest(h.pos));
                }
                return Use::Swing;
            }
            OnUse::None => {}
        }
    }
    if stack.is_empty() {
        return Use::Pass;
    }
    let b = match idef(stack.id()).kind {
        ItemKind::Block(b) => b,
        ItemKind::Seed { .. } => return crate::game::farm::plant(c, hand, h),
        ItemKind::Tool { mask, .. } if mask & HOE != 0 => return crate::game::farm::till(c, hand, h),
        _ => return Use::Pass,
    };
    let r = place(c, hand, b, h);
    // Сервер: клик по верхней грани блока на высоте ≥ 127 с блоком в руке — сообщение над хотбаром.
    if r != Use::Swing && h.face == F_UP && h.pos[1] >= H - 1 {
        c.height_msg = true;
    }
    r
}

/// BlockItem.place: клетка (заменяемая кликнутая или соседняя по грани), состояние ставления, опора, «не в себя»,
/// запись, звук, стек −1.
pub(crate) fn place(c: &mut ActCtx, hand: usize, b: BlockId, h: Hit) -> Use {
    let item = b as ItemId;
    let clicked = c.w.get_cell(h.pos[0], h.pos[1], h.pos[2]);
    let in_place = replaceable_by(clicked, item);
    let at = if in_place { h.pos } else { offset(h.pos, h.face) };
    if !in_place && !replaceable_by(c.w.get_cell(at[0], at[1], at[2]), item) {
        return Use::Fail;
    }
    let Some(cell) = placement_state(c.w, c.player, b, at, h, in_place) else { return Use::Fail };
    let nb = block_of(cell);
    if !can_place_at(c.w, nb, at) {
        return Use::Fail;
    }
    // Многоблок (MEK-03 §3.5): клетки частей над основанием — в мире и заменяемы, иначе предмет не ставится вовсе.
    let parts = multiblock_parts(nb);
    for dy in parts.clone() {
        let q = [at[0], at[1] + dy, at[2]];
        if q[1] >= H || !bdef(c.w.get_block(q[0], q[1], q[2])).replaceable || protected(c.w, q) {
            return Use::Fail;
        }
    }
    // «Не в себя»: форма коллизии против коробки игрока на конец прошлого тика, других игроков и летящих падающих блоков.
    let o = [at[0] as f64, at[1] as f64, at[2] as f64];
    let pb = c.player.bbox();
    let part_cells: Vec<(f64, Cell)> = parts.clone().map(|dy| (dy as f64, cell_of(BOUNDING, dy as State))).collect();
    for (dy, pc) in std::iter::once((0.0, cell)).chain(part_cells.iter().copied()) {
        for s in collision_of(pc) {
            let s = s.moved([o[0], o[1] + dy, o[2]]);
            if blocks_placement(&s, &pb) || c.bodies.iter().any(|b| blocks_placement(&s, b)) || c.falling().iter().any(|f| blocks_placement(&s, &f.bbox())) {
                return Use::Fail;
            }
        }
    }
    if !c.write(at, cell) {
        return Use::Fail; // вне высоты мира 0…127
    }
    for dy in parts {
        c.write([at[0], at[1] + dy, at[2]], cell_of(BOUNDING, dy as State));
    }
    c.emit(EV_PLACED, at, nb as i32);
    if matches!(bdef(nb).be, Some(BeKind::Mek(_))) {
        let id = if hand == 0 { c.inv.hand().damage } else { c.inv.offhand.damage };
        c.place_machine_data(at, id);
    }
    if !c.creative() {
        let s = if hand == 0 { c.inv.hand_mut() } else { &mut c.inv.offhand };
        s.count -= 1;
        if s.count <= 0 {
            *s = ItemStack::EMPTY;
        }
    }
    Use::Swing
}

/// Состояние ставления (getPlacementState): ось бревна — по грани клика; печь и сундук — лицом к игроку по yaw;
/// факел — по направлениям ставления: вниз — стоячий на опоре, вбок — настенный на полной грани (вверх — нельзя);
/// лестница — первое горизонтальное направление с полной гранью сзади (лицевой стороной лестницы лестницу не
/// поставить); снег на снегу — слой +1; листва игрока — «не опадает». None — нельзя (FAIL).
pub(crate) fn placement_state(w: &mut WorldData, player: &Player, b: BlockId, at: [i32; 3], h: Hit, in_place: bool) -> Option<Cell> {
    let (yaw, pitch) = (player.yaw, player.pitch);
    let dirs = placement_dirs(yaw, pitch, in_place, h.face);
    // Настенный вариант со стороной f (от стены): опора — блок за спиной, его грань в сторону f полная.
    let wall_ok = |w: &mut WorldData, f: u8| {
        let s = offset(at, f ^ 1);
        face_full(w.get_cell(s[0], s[1], s[2]), f)
    };
    let s: State = match bdef(b).place {
        Place::Axis => match h.face {
            F_WEST | F_EAST => 1,
            F_NORTH | F_SOUTH => 2,
            _ => 0,
        },
        Place::FacePlayer => (horizontal_facing(yaw) ^ 1) as State,
        Place::Torch { wall: wall_block } => {
            // VerticallyAttachableBlockItem: настенный вариант — первое горизонтальное направление с опорой (WallTorch
            // .getPlacementState); затем по порядку направлений первый годный, вверх пропускается.
            let wall = dirs.iter().copied().find(|&d| d >= F_NORTH && wall_ok(w, d ^ 1)).map(|d| d ^ 1);
            for d in dirs {
                if d == F_UP {
                    continue;
                }
                if d == F_DOWN {
                    if top_center(w.get_cell(at[0], at[1] - 1, at[2])) {
                        return Some(b as Cell);
                    }
                } else if let Some(f) = wall {
                    return Some(cell_of(wall_block, f as State));
                }
            }
            return None;
        }
        Place::Ladder => {
            if !in_place {
                let back = w.get_cell(h.pos[0], h.pos[1], h.pos[2]);
                if block_of(back) == b && facing_of(back) == h.face {
                    return None;
                }
            }
            (dirs.iter().copied().find(|&d| d >= F_NORTH && wall_ok(w, d ^ 1))? ^ 1) as State
        }
        Place::Layers => {
            let here = w.get_cell(at[0], at[1], at[2]);
            if block_of(here) == b {
                snow_layers(here).min(7) as State
            } else {
                0
            }
        }
        Place::Persistent => ST_PERSIST,
        Place::Plain => 0,
    };
    Some(cell_of(b, s))
}

/// Средняя кнопка в выживании (MinecraftClient.doItemPick): предмет блока в клетке pos; есть в хотбаре — выбрать
/// ту ячейку, есть в рюкзаке — поменять с ячейкой хотбара (первая пустая от выбранной, иначе выбранная) и выбрать её.
pub(crate) fn item_pick(w: &mut WorldData, inv: &mut Inventory, pos: [i32; 3], creative: bool) {
    let b = w.get_block(pos[0], pos[1], pos[2]);
    let item = if b == AIR { 0 } else { block_item(b) };
    if item == 0 {
        return;
    }
    match inv.slot_with_stack(&ItemStack::of(item, 1)) {
        Some(i) if i < 9 => inv.selected = i,
        Some(i) => inv.swap_with_hotbar(i),
        None if creative => {
            // Творческий (PlayerInventory.addPickBlock): копия блока в руку — в пустую ячейку хотбара начиная с выбранной,
            // все заняты — на место выбранной.
            let sel = inv.selected;
            let slot = (0..9).map(|k| (sel + k) % 9).find(|&k| inv.get(k).is_empty()).unwrap_or(sel);
            inv.selected = slot;
            inv.set(slot, ItemStack::of(item, 1));
        }
        None => {}
    }
}

/// Правило опоры (canPlaceAt) по таблице: растения — на #dirt или пашню (мёртвый куст — на #dirt или песок); кактус —
/// без твёрдых соседей сбоку, на кактусе или песке, без жидкости сверху; снежный слой — на полном верху. Кубы — всегда;
/// факелы и лестница проверены при выборе состояния.
pub(crate) fn can_place_at(w: &mut WorldData, b: BlockId, at: [i32; 3]) -> bool {
    let below = w.get_block(at[0], at[1] - 1, at[2]);
    match bdef(b).support {
        Support::Layers => snow_survives(w, at),
        Support::Plant | Support::DeadBush => plant_supported(b, below),
        Support::Cactus => {
            for f in [F_NORTH, F_SOUTH, F_WEST, F_EAST] {
                let n = offset(at, f);
                if !collision_of(w.get_cell(n[0], n[1], n[2])).is_empty() {
                    return false;
                }
            }
            (below == CACTUS || below == SAND) && w.get_block(at[0], at[1] + 1, at[2]) != WATER
        }
        Support::Crop => crate::game::farm::crop_survives(w, at),
        Support::None | Support::Floor | Support::Wall => true,
    }
}

/// Снежный слой держится (SnowBlock.canPlaceAt): у блока снизу верх коллизии — полный квадрат или там снег в 8 слоёв.
pub(crate) fn snow_survives(w: &mut WorldData, pos: [i32; 3]) -> bool {
    let below = w.get_cell(pos[0], pos[1] - 1, pos[2]);
    let full = collision_of(below).iter().any(|s| s.hi[1] >= 1.0 && s.lo[0] <= 0.0 && s.hi[0] >= 1.0 && s.lo[2] <= 0.0 && s.hi[2] >= 1.0);
    full || (bdef(block_of(below)).shape == Shape::Layers && snow_layers(below) == 8)
}

/// Горизонтальное направление взгляда (Entity.getHorizontalFacing): yaw 0 — юг, 90 — запад, 180 — север, 270 — восток.
pub(crate) fn horizontal_facing(yaw: f32) -> u8 {
    [F_SOUTH, F_WEST, F_NORTH, F_EAST][((yaw / 90.0 + 0.5).floor() as i32 & 3) as usize]
}

/// Шесть направлений по близости к взгляду (Direction.getEntityFacingOrder): три «куда смотрю» по убыванию модуля
/// компоненты, затем противоположные им в обратном порядке. Float, как в Java.
pub(crate) fn facing_order(yaw: f32, pitch: f32) -> [u8; 6] {
    let f = pitch * (std::f32::consts::PI / 180.0);
    let g = -yaw * (std::f32::consts::PI / 180.0);
    let (h, i) = (libm::sinf(f), libm::cosf(f));
    let (j, k) = (libm::sinf(g), libm::cosf(g));
    let (bl, bl2, bl3) = (j > 0.0, h < 0.0, k > 0.0);
    let l = if bl { j } else { -j };
    let m = if bl2 { -h } else { h };
    let n = if bl3 { k } else { -k };
    let (o, p) = (l * i, n * i);
    let d1 = if bl { F_EAST } else { F_WEST };
    let d2 = if bl2 { F_UP } else { F_DOWN };
    let d3 = if bl3 { F_SOUTH } else { F_NORTH };
    let [a, b, c] = if l > n {
        if m > o {
            [d2, d1, d3]
        } else if p > m {
            [d1, d3, d2]
        } else {
            [d1, d2, d3]
        }
    } else if m > p {
        [d2, d3, d1]
    } else if o > m {
        [d3, d1, d2]
    } else {
        [d3, d2, d1]
    };
    [a, b, c, c ^ 1, b ^ 1, a ^ 1]
}

/// Направления ставления (ItemPlacementContext.getPlacementDirections): порядок взгляда; если кликнутая клетка не
/// заменяется — направление «в кликнутый блок» (против грани клика) переносится в начало.
pub(crate) fn placement_dirs(yaw: f32, pitch: f32, in_place: bool, side: u8) -> [u8; 6] {
    let mut d = facing_order(yaw, pitch);
    if !in_place {
        if let Some(i) = d.iter().position(|&x| x == side ^ 1) {
            d[..=i].rotate_right(1);
        }
    }
    d
}
