//! Симуляция мира (Ф1, ARCHITECTURE-REVIEW P1-5): мир, время и сутки, предметы и падающие блоки, блок-сущности, очередь
//! тиков блоков, ГСЧ дропа и тиков, рецепты, номера сущностей. Здесь же запись в мир по правилам игры
//! (Sim::set_world: соседи, тики, опад, рассыпание) — одна дорога записи для игроков, тиков и сети. Живёт на сервере
//! (server.rs: ServerSim); события тика симуляция пишет в переданный ей приёмник (Events, адресат — всем).
//!
//! Сутки — 24000 тиков (решение после ASP-18: время мира — в Rust). day — время суток Minecraft (getTimeOfDay):
//! 0 — 6:00 (восход), 6000 — полдень, 12000 — закат, 18000 — полночь; идёт +1 за тик мира, если day_cycle
//! (doDaylightCycle). Небо в JS берёт Game::time_of_day — долю суток от полуночи, как прежнее dayTime.

use crate::abi::EV_BROKEN;
use crate::cell::{block_of, cell_of, offset, state_of, Cell, F_DOWN, F_UP, H};
use crate::game::crafting::RecipeBook;
use crate::game::entity::{FallingBlock, Items};
use crate::game::mob::Mobs;
use crate::game::interact::{can_place_at, snow_survives, UPDATE_ORDER};
use crate::game::physics::Bufs;
use crate::game::view::Events;
use crate::game::{GameRng, Ids};
use crate::registry::blocks::{
    bdef, bounding_dy, face_full, facing_of, is_falling, is_leaves, is_log, main_of, multiblock_parts, plant_supported,
    snow_layers, top_center, BeKind, Loot, Support, ST_DECAY, ST_LIT,
};
use crate::registry::ids::{AIR, BOUNDING};
use crate::registry::items::{block_item, I_SNOWBALL};
use crate::world::stack::ItemStack;
use crate::world::ticks::{TickQueue, Zone};
use crate::world::grid::{arms_of, is_transmitter, with_arms};
use crate::world::blockentity::BlockEntity;
use crate::world::mek::items::MekItems;
use crate::world::mek::MekBe;
use crate::world::{World, WorldData};

/// Тиков в сутках.
pub(crate) const DAY_TICKS: u64 = 24000;

/// Доля суток от полуночи (0.5 — полдень) по времени суток day на доле тика alpha: небо в кадре (идут сутки — плавно
/// между тиками).
pub(crate) fn time_of_day(day: u64, cycle: bool, alpha: f64) -> f64 {
    let t = day as f64 + if cycle { alpha } else { 0.0 };
    (t / DAY_TICKS as f64 + 0.25).rem_euclid(1.0)
}

/// Время суток по доле от полуночи (с точностью до тика).
pub(crate) fn set_time_of_day(f: f64) -> u64 {
    let t = ((f - 0.25).rem_euclid(1.0) * DAY_TICKS as f64).round() as u64;
    t % DAY_TICKS
}

pub(crate) struct Sim {
    /// Мир сервера (свой, не общий с отрисовкой клиента): правки, кэш, журнал записей для кадров.
    pub(crate) world: World,
    /// Время мира в тиках (getTime).
    pub(crate) time: u64,
    /// Время суток в тиках Minecraft, 0…23999 (см. шапку).
    pub(crate) day: u64,
    /// Идёт ли время суток (doDaylightCycle).
    pub(crate) day_cycle: bool,
    /// Предметы в мире (entity.rs, ASP-07).
    pub(crate) items: Items,
    /// Рецепты (crafting.rs, ASP-09).
    pub(crate) book: RecipeBook,
    /// Запланированные тики блоков (world/ticks.rs, E12).
    pub(crate) ticks: TickQueue,
    /// Дроп и сущности.
    pub(crate) rng: GameRng,
    /// Случайные тики.
    pub(crate) tick_rng: GameRng,
    /// Падающие блоки (entity.rs, ASP-05 §5).
    pub(crate) falling: Vec<FallingBlock>,
    pub(crate) fall_bufs: Bufs,
    /// Номера сущностей — общий счётчик игроков, предметов и падающих блоков (1 — первый вошедший игрок).
    pub(crate) ids: Ids,
    /// Активная зона этого тика (E13, P1-9): объединение квадратов игроков.
    pub(crate) zone: Zone,
    /// Случайных клеток на секцию 16³ за тик (gamerule randomTickSpeed; 3 по умолчанию). Отладка поднимает для приёмки роста.
    pub(crate) random_tick_speed: u32,
    /// Клетки, где пашня стала землёй (set_to_dirt): сервер поднимает стоящих на них игроков на 1/16 (иначе проваливаются).
    pub(crate) lifted: Vec<[i32; 3]>,
    /// Машины Mekanism в предметах (world/mek/items.rs): номер в damage стека → энергия, слоты, бак.
    pub(crate) mek_items: MekItems,
    /// Сломанные машины этого действия: ждут дропа (drop_loot кладёт их данные в предмет); без дропа — содержимое
    /// рассыпается в начале тика мира.
    pub(crate) mek_broken: Vec<([i32; 3], Box<MekBe>)>,
    /// Мобы (game/mob, G6): тикают после предметов в фазе мира (ServerSim зовёт tick_mobs со снимком игроков).
    pub(crate) mobs: Mobs,
    /// Правки игроков этого тика (G17, /rollback): (кто, клетка, было, стало) — пишет ActCtx::write у сервера, забирает
    /// в свой журнал ServerSim (game/server/admin.rs). Последствия (течение, опад) сюда не входят.
    pub(crate) by_player: Vec<(u32, [i32; 3], Cell, Cell)>,
}

impl Sim {
    pub(crate) fn new(world: &World, seed: u64) -> Sim {
        world.d.borrow_mut().grid.on = true; // мир сервера ведёт энергосети (world/grid.rs)
        world.d.borrow_mut().logistics.on = true; // и предметы в пути (world/logistics.rs)
        Sim {
            world: world.share(),
            time: 0,
            day: 0,
            day_cycle: true,
            items: Items::new(),
            book: RecipeBook::standard(),
            ticks: TickQueue::default(),
            rng: GameRng(seed ^ 0x5EED_1337),
            tick_rng: GameRng(seed ^ 0x7105_7105),
            falling: Vec::new(),
            fall_bufs: Bufs::default(),
            ids: Ids(1),
            zone: Zone::default(),
            random_tick_speed: 3,
            lifted: Vec::new(),
            mek_items: MekItems::default(),
            mek_broken: Vec::new(),
            mobs: Mobs::new(seed),
            by_player: Vec::new(),
        }
    }

    /// 3. Мир (ServerWorld.tick): время +1 (и сутки, если идут) → запланированные тики блоков → случайные тики →
    /// сущности (предметы, падающие блоки) → блок-сущности (печи в порядке появления). Всё — в активной зоне zone
    /// (объединение квадратов 9×9 кусков вокруг игроков, E13, P1-9).
    pub(crate) fn tick_world(&mut self, zone: Zone, ev: &mut Events) {
        self.time += 1;
        if self.day_cycle {
            self.day = (self.day + 1) % DAY_TICKS;
        }
        self.zone = zone;
        let zone = std::mem::take(&mut self.zone);
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        self.scatter_removed(&mut w); // сущности, убранные правками мимо игры (JS)
        self.scatter_broken_machines();
        w.logistics.set_zone(&zone);
        self.run_scheduled(&mut w, &zone, ev);
        self.random_ticks(&mut w, &zone, ev);
        self.items.tick(&mut w, &mut self.rng, &zone);
        self.tick_falling(&mut w, &zone, ev);
        self.tick_block_entities(&mut w, &zone, ev);
        w.tick_grid(&zone); // сети — после всех блок-сущностей (решение после MEK-02)
        // Выпавшие из транспортёров стеки (сломан, нет пути) — предметами в мире.
        for (s, p) in std::mem::take(&mut w.logistics.drops) {
            let cell = [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
            self.items.drop_from_block(cell, s, &mut self.rng, &mut self.ids); // Block.popResource: точка в клетке стека
        }
        self.zone = zone;
    }

    /// 3½. Блок-сущности внутри зоны в порядке появления: печи (AbstractFurnaceBlockEntity.tick) и машины Mekanism
    /// (world/mek: машина на время тика вынимается — соседи меняются сразу). Горение или «активна» сменились — LIT в
    /// мир (тот же блок — сущность цела; меш перестроится, свет — со срезом света).
    pub(crate) fn tick_block_entities(&mut self, w: &mut WorldData, zone: &Zone, ev: &mut Events) {
        for p in w.be_mut().ticking_list() {
            if !zone.has(p[0], p[2]) {
                continue;
            }
            let c = w.get_cell(p[0], p[1], p[2]);
            let on = match bdef(block_of(c)).be {
                Some(BeKind::Furnace) => w.be_mut().furnace_mut(p).and_then(|f| f.tick()),
                Some(BeKind::Mek(_)) => {
                    let Some(mut m) = w.be_mut().mek_take(p) else { continue };
                    let on = m.tick(p, facing_of(c), w);
                    w.be_mut().mek_put(p, m);
                    // Насос выкачал лаву: воздух дорогой игры (соседи и жидкости узнают).
                    for q in std::mem::take(&mut w.drained) {
                        self.set_world(w, q, AIR as Cell, ev);
                    }
                    on
                }
                _ => None,
            };
            let Some(on) = on else { continue };
            let s = if on { state_of(c) | ST_LIT } else { state_of(c) & !ST_LIT };
            self.set_world(w, p, cell_of(block_of(c), s), ev);
        }
    }

    /// World.breakBlock(pos, drop = true) без игрока: событие 2001 (звук и частицы разрушения), дроп без инструмента,
    /// воздух с обновлениями соседей. Факел и лестница без опоры, растение без земли, кактус у стены.
    pub(crate) fn break_naturally(&mut self, w: &mut WorldData, pos: [i32; 3], ev: &mut Events) {
        let c = w.get_cell(pos[0], pos[1], pos[2]);
        if block_of(c) == AIR {
            return;
        }
        ev.push(EV_BROKEN, pos, block_of(c) as i32);
        if matches!(bdef(block_of(c)).be, Some(BeKind::Mek(_))) {
            // Машина: сначала убрать (сущность ждёт дропа в mek_broken), затем дроп с её данными.
            self.set_world(w, pos, AIR as Cell, ev);
            self.drop_loot(c, pos, false);
            return;
        }
        self.drop_loot(c, pos, false);
        self.set_world(w, pos, AIR as Cell, ev);
    }

    /// Дроп по таблице (Block.dropStacks): каждый стек — отдельный предмет в мире (Block.dropStack, задержка 10).
    /// by_player — сломал игрок (снежному слою без игрока таблица ничего не даёт).
    pub(crate) fn drop_loot(&mut self, cell: Cell, pos: [i32; 3], by_player: bool) {
        let b = block_of(cell);
        let mut stacks: Vec<ItemStack> = Vec::new();
        match bdef(b).loot {
            Loot::None => {}
            Loot::Itself => {
                let it = block_item(b);
                if it != 0 {
                    let mut s = ItemStack::of(it, 1);
                    // Машина Mekanism — с энергией и содержимым в предмете (номер записи в damage, world/mek/items.rs).
                    if let Some(i) = self.mek_broken.iter().position(|(q, _)| *q == pos) {
                        let (_, m) = self.mek_broken.remove(i);
                        s.damage = self.mek_items.stash(&m);
                    }
                    stacks.push(s);
                }
            }
            Loot::SnowLayers => {
                if by_player {
                    stacks.push(ItemStack::of(I_SNOWBALL, snow_layers(cell) as i32));
                }
            }
            Loot::Crop => stacks.extend(self.crop_loot(cell)),
            Loot::Table(rows) => {
                let mut done = 0u32;
                for r in rows {
                    if r.alt != 0 && done & (1 << r.alt) != 0 {
                        continue;
                    }
                    if r.chance >= 1.0 || self.rng.next_f32() < r.chance {
                        let n = if r.min == r.max { r.min as i32 } else { self.rng.range(r.min as i32, r.max as i32) };
                        if r.alt != 0 {
                            done |= 1 << r.alt;
                        }
                        if n > 0 {
                            stacks.push(ItemStack::of(r.item, n));
                        }
                    }
                }
            }
        }
        for s in stacks {
            self.items.drop_from_block(pos, s, &mut self.rng, &mut self.ids);
        }
    }

    /// Записать ячейку (World.setBlockState, NOTIFY_ALL), порядок как в Minecraft: запись → убранная блок-сущность
    /// рассыпает содержимое (onStateReplaced) → новый блок (onBlockAdded: песок и гравий планируют падение через 2) →
    /// обновления формы 6 соседям (W, E, N, S, D, U) → пересчёт опада листвы, если менялись бревно или листва.
    /// false — вне высоты или ячейка та же.
    pub(crate) fn set_world(&mut self, w: &mut WorldData, pos: [i32; 3], cell: Cell, ev: &mut Events) -> bool {
        if pos[1] < 0 || pos[1] >= H {
            return false;
        }
        let old = w.get_cell(pos[0], pos[1], pos[2]);
        if old == cell {
            return false;
        }
        w.set_cell(pos[0], pos[1], pos[2], cell);
        if is_transmitter(cell) {
            fix_arms(w, pos); // рукава — по соседям сразу (вид и форма; сеть сведётся в фазе сетей)
        }
        self.scatter_removed(w);
        let (ob, nb) = (block_of(old), block_of(cell));
        if is_falling(nb) && ob != nb {
            self.ticks.schedule(self.time, pos, nb, 2);
        }
        for f in UPDATE_ORDER {
            self.shape_update(w, offset(pos, f), f ^ 1, ev);
        }
        // Многоблок (MEK-03 §3.5): убрали основание — снять его части; часть заменили чем-то — основание ломается с дропом.
        if ob != nb {
            for dy in multiblock_parts(ob) {
                let q = [pos[0], pos[1] + dy, pos[2]];
                let pc = w.get_cell(q[0], q[1], q[2]);
                if block_of(pc) == BOUNDING && bounding_dy(pc) == dy {
                    self.set_world(w, q, AIR as Cell, ev);
                }
            }
            if ob == BOUNDING {
                let m = main_of(old, pos);
                if multiblock_parts(w.get_block(m[0], m[1], m[2])).contains(&(pos[1] - m[1])) {
                    self.break_naturally(w, m, ev);
                }
            }
        }
        // Опад: дистанция листвы могла измениться (опавший лист — уже «опадающий», от него ничьи пути не шли).
        let decayed_leaf = is_leaves(ob) && state_of(old) & ST_DECAY != 0;
        if ob != nb && (is_log(ob) || is_leaves(ob) || is_log(nb) || is_leaves(nb)) && !decayed_leaf {
            self.leaves_recheck(w, pos);
        }
        // Жидкости (ASP-15): новая жидкость и соседи-жидкости ставят себе тик, лава у воды твердеет.
        self.fluid_wake(w, pos, cell, ev);
        true
    }

    /// Содержимое убранных блок-сущностей — рассыпать (ItemScatterer.spawn); сундук открыт — окно закроется само.
    /// Машина Mekanism не рассыпается: ждёт дропа своего блока (drop_loot) в mek_broken.
    pub(crate) fn scatter_removed(&mut self, w: &mut WorldData) {
        for (p, be) in w.take_removed() {
            if let BlockEntity::Mek(m) = be {
                self.mek_broken.push((p, m));
                continue;
            }
            for s in be.stacks() {
                self.items.scatter(p, *s, &mut self.rng, &mut self.ids);
            }
        }
    }

    /// Машины, сломанные без дропа (рукой без кирки, отладкой): содержимое рассыпается, энергия теряется.
    pub(crate) fn scatter_broken_machines(&mut self) {
        for (p, m) in std::mem::take(&mut self.mek_broken) {
            for s in &m.slots[..m.nslots()] {
                self.items.scatter(p, *s, &mut self.rng, &mut self.ids);
            }
        }
    }

    /// Обновление формы соседа (getStateForNeighborUpdate) — from: сторона, откуда пришло обновление. Без опоры:
    /// растение — с любой стороны, стоячий факел — снизу, настенный факел и лестница — со стороны опоры, снежный слой —
    /// с любой → ломается с дропом (World.breakBlock). Песок и гравий — тик через 2; кактус без опоры — тик через 1.
    pub(crate) fn shape_update(&mut self, w: &mut WorldData, pos: [i32; 3], from: u8, ev: &mut Events) {
        let c = w.get_cell(pos[0], pos[1], pos[2]);
        let b = block_of(c);
        if is_transmitter(c) {
            fix_arms(w, pos); // сосед-передатчик или машина появились или пропали — рукава заново (только состояние)
            return;
        }
        let below = w.get_cell(pos[0], pos[1] - 1, pos[2]);
        let survives = match bdef(b).support {
            Support::Floor => from != F_DOWN || top_center(below),
            Support::Wall => {
                let f = facing_of(c);
                let s = offset(pos, f ^ 1);
                from != f ^ 1 || face_full(w.get_cell(s[0], s[1], s[2]), f)
            }
            Support::Layers => snow_survives(w, pos),
            Support::Plant | Support::DeadBush => plant_supported(b, block_of(below)),
            Support::Crop => crate::game::farm::crop_survives(w, pos),
            Support::None | Support::Cactus => true,
        };
        if !survives {
            self.break_naturally(w, pos, ev);
            return;
        }
        if is_falling(b) {
            self.ticks.schedule(self.time, pos, b, 2);
        } else if bdef(b).support == Support::Cactus && !can_place_at(w, b, pos) {
            self.ticks.schedule(self.time, pos, b, 1);
        } else if bdef(b).farmland && from == F_UP && crate::game::fluids::is_solid(w.get_cell(pos[0], pos[1] + 1, pos[2])) {
            self.ticks.schedule(self.time, pos, b, 1); // FarmlandBlock.getStateForNeighborUpdate: твёрдое сверху — земля через тик
        }
    }
}

/// Рукава передатчика в клетке p по соседям и режимам сторон — сырой записью состояния (тот же блок: сеть и
/// блок-сущности не трогаются). Изменились — сеть пересвяжет его в фазе сетей (связь с соседом могла появиться или
/// пропасть без смены блока: режим стороны, цвет).
pub(crate) fn fix_arms(w: &mut WorldData, p: [i32; 3]) {
    use crate::world::grid::{arm, tkind};
    let c = w.get_cell(p[0], p[1], p[2]);
    let a = w.transmitter_arms(p);
    let old = arms_of(state_of(c));
    if old != a {
        w.set_cell(p[0], p[1], p[2], cell_of(block_of(c), with_arms(state_of(c), a)));
        // Сменился рукав к передатчику того же вида — связь; к машине — только приёмники и источники сети.
        let relink = (0..6u8).any(|f| {
            let q = crate::cell::offset(p, f);
            arm(old, f) != arm(a, f) && tkind(w.get_cell(q[0], q[1], q[2])) == tkind(c)
        });
        if relink {
            w.grid.relink(p);
        } else {
            w.grid.touch(p);
        }
    }
}
