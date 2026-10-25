//! Сервер симуляции (design.md §2.2–2.3): авторитетный мир (Sim) и игроки — теневое тело, инвентарь, окно, ломание.
//! Действия игроков — те же функции, что у клиента (interact.rs), в режиме Auth, с проверками (дальность, точка
//! попадания, лимиты за тик, прогресс ломания по принятым тикам); движение — «теневой тик» той же физикой по
//! присланному вводу и сверка с итогом клиента (§1.3): совпало — принять; не совпало, но правдоподобно (отличие ≤ 0.1,
//! путь тела свободен) — принять итог клиента; иначе Teleport назад и ждать TeleportAck. События — с адресатом
//! (view::To) и в радиусе слышимости, правки — только тем, кто знает кусок (радиус View из Hello), сущности — в радиусе
//! слежения; номера сущностей — общий счётчик.
//!
//! Тик сервера (§2.3): входящие тики игроков в порядке входа (действия по порядку → теневой тик → доступность окна) →
//! мир по объединению зон игроков (Sim::tick_world) → подбор по игрокам → кадры (flush): каждому игроку один
//! S2C::Bundle с номером тика. Хранение — save/ (Saver над ServerSim). Сеть — net.rs (Server: соединения и байты).
//!
//! LocalServer — сервер с одним соединением-петлёй для браузера без сети (game/mod.rs: Game): тот же ServerSim, кадры
//! — байтами через ту же сериализацию, что идёт по сети.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::abi::{EV_CHEST_CLOSE, EV_CHEST_OPEN, EV_EQUIP, EV_PICKUP};
use crate::cell::{Cell, CS};
use crate::game::client::{inv_hash, keys_of, move_of};
use crate::game::crafting::crafting_table_in_reach;
use crate::game::bucket::use_bucket;
use crate::game::interact::{break_block, interact_block, item_pick, mine_delta, protected, ActCtx, Hit, Mode, REACH, REACH_CREATIVE};
use crate::game::inventory::{Inventory, OFFHAND};
use crate::game::physics::{collide, space_empty, Bufs};
use crate::game::player::{Keys, Player, STEP};
use crate::game::screen::{Menu, MenuKind, Store, Ui, UiEv};
use crate::game::health::{Health, Vitals};
use crate::game::sim::{set_time_of_day, Sim};
use crate::game::social::{clean_chat, nick_ok, smooth_ping, ChatBucket, CHAT_TOO_FAST};
use crate::game::view::{Events, To};
use crate::game::{crossroads_spawn, find_spawn, give_into, kit_into, menu_hash, place_at_spawn, screen_code, GameRng};
use crate::gen::{probe, GEN_VERSION};
use crate::proto::*;
use crate::registry::blocks::{bdef, OnUse};
use crate::registry::ids::AIR;
use crate::registry::items::{idef, ItemKind};
use crate::registry::shapes::Aabb;
use crate::save::token::{same_hash, token_hash};
use crate::world::blockentity::BlockEntity;
use crate::world::stack::ItemStack;
use crate::world::ticks::Zone;
use crate::world::{chunk_of, local_key, World, WorldData};

pub(crate) mod admin;
mod mobs;
mod pvp;
mod survival;
use survival::vitals_of;

/// Радиус слежения за сущностями от ступней (design.md §3.6): предметы рисуются ближе 64 от глаз — с запасом на рост;
/// падающие блоки и игроки видны дальше.
const TRACK_ITEMS: f64 = 72.0;
const TRACK_FALLING: f64 = 160.0;
const TRACK_PLAYERS: f64 = 128.0;
/// События мира (звук и частицы ломания, ставления, сундука) — в этом радиусе от клетки события.
const EVENT_R: f64 = 64.0;
/// Чужие трещины (BreakStage) — в этом радиусе (§4.2).
const STAGE_R: f64 = 32.0;
/// Радиус правок (Hello.view, кусков): по умолчанию и пределы; забыть — за радиусом с запасом.
pub(crate) const VIEW_DEFAULT: u8 = 8;
/// Предел радиуса правок: клиент просит «блоки до» из настроек (по умолчанию 32 куска, mp/design.md §3.5), сервер
/// ограничивает им. Объём при входе и смене куска — бюджет правок (N1c).
const VIEW_MAX: i32 = 32;
const FORGET_MARGIN: i32 = 2;
/// Бюджет правок кусков на игрока за тик сервера (§3.5): вход и смена куска не шлют сотни килобайт одним кадром — остаток
/// идёт в следующих тиках, от ближних кусков к дальним. Кусок больше бюджета уходит один и целиком (иначе встал бы).
pub(crate) const EDIT_BUDGET: usize = 32 << 10;
/// Не больше тиков игрока за тик сервера (§2.3); корзина тиков: 20 в секунду, всплеск до 40 (§1.3, п. 7).
const MAX_TICKS_PER_TICK: usize = 10;
const BUCKET_MAX: f32 = 40.0;
const INBOX_MAX: usize = 40;
/// Правдоподобие хода (§1.3, п. 5): отличие от теневого итога не больше (толчок моба или игрока на клиенте).
const MOVE_SLACK: f64 = 0.1;
/// Ломание (§4.2): прогресс по принятым тикам — не меньше (как «доломать по времени» в MC; MP-1 ломает сразу).
const BREAK_MIN: f32 = 0.7;
/// Лимиты действий за тик (§4.2–4.3).
const MAX_USES: u8 = 4;
const MAX_BREAKS: u8 = 4;
/// Пинг — раз в столько тиков.
const PING_EVERY: u64 = 20;
/// Сглаженный пинг игроков для Tab — всем раз в столько тиков (2 с, mp/design.md §3.8).
const LIST_PING_EVERY: u64 = 40;
/// Врата странствий (spawn.md §4): постоять в проёме (портал Нижнего мира в MC — 80 тиков, у нас 3 с); перезарядка;
/// куда — суша в кольце от площади, не ближе GATE_ALONE к другим игрокам.
const GATE_TICKS: u32 = 60;
const GATE_COOLDOWN: u64 = 1200;
const GATE_NEAR: f64 = 400.0;
const GATE_FAR: f64 = 2000.0;
const GATE_ALONE: f64 = 64.0;

/// Последнее отправленное клиенту о сущности.
#[derive(Clone, Copy)]
struct Tracked {
    pos: [f64; 3],
    /// Предмет — возраст вращения; игрок — номер его тика.
    spin: u32,
    count: i32,
}

/// Ломание на сервере: клетка, прогресс по принятым тикам игрока с BreakStart, стадия, которую видят другие. Отмена
/// (BreakAbort) запись не снимает (как miningPos в MC): клиент Minecraft может отменить и, не начиная заново, дожать
/// тот же блок — BreakDone проверяется по прогрессу с начала; другим трещины после отмены не видны.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Breaking {
    pub(crate) pos: [i32; 3],
    pub(crate) progress: f32,
    stage: i8,
    active: bool,
}

/// Игрок на сервере.
pub(crate) struct ServerPlayer {
    /// Номер сущности (общий счётчик).
    pub(crate) id: u32,
    pub(crate) nick: String,
    pub(crate) skin: u8,
    /// Теневое тело: состояние на конец принятого тика.
    pub(crate) player: Player,
    pub(crate) inv: Inventory,
    pub(crate) screen: Option<Menu>,
    /// Номер открытого окна (0 — инвентарь игрока).
    pub(crate) win: u8,
    next_win: u8,
    pub(crate) ui_ev: Vec<UiEv>,
    pub(crate) breaking: Option<Breaking>,
    pub(crate) online: bool,
    /// Хеш токена, которым ник закреплён (save/token.rs); None — ник свободен (гость или запись старой версии).
    pub(crate) token: Option<[u8; 32]>,
    /// Клиент «вне игры» (Away: скрытая вкладка, меню с замороженными кадрами): тиков он не шлёт, сервер тикает игрока
    /// с пустым вводом (§1.3). Тики, пришедшие после Away, — лишние и отбрасываются.
    pub(crate) away: bool,
    inbox: VecDeque<ClientTick>,
    /// Наибольший номер решённого предсказания этого тика.
    ack: Option<u32>,
    /// Окна (WinClose/WinOpen) — в кадр по порядку; свои сообщения (Teleport).
    win_out: Vec<Msg>,
    own: Vec<Msg>,
    /// Готовые кадры.
    pub(crate) outbox: Vec<S2C>,
    /// Что клиент знает: инвентарь, выбранная ячейка; разошлись — полная копия.
    sent_inv: Vec<ItemStack>,
    sent_sel: usize,
    inv_force: bool,
    win_force: bool,
    /// Номер последнего клика по окну, принятого от клиента (`Act::Click.rev`), и сколько из этого клиент уже знает
    /// (`WinAck`): по нему клиент отделяет подтверждённые клики от тех, что надо проиграть заново поверх копии окна.
    win_rev: u32,
    win_rev_sent: u32,
    /// Хранилище блока открытого окна, каким его знает клиент; числа печи.
    win_sent: Vec<ItemStack>,
    furnace_sent: Option<[i32; 4]>,
    /// Числа окна машины (MekBe::view), какими их знает клиент; свечение сетей.
    mek_sent: Vec<i32>,
    glow_sent: Vec<([i32; 3], u16)>,
    /// Стороны машин рядом, какими их знает клиент (Msg::MekSides, пока в руке конфигуратор).
    sides_sent: HashMap<[i32; 3], u32>,
    /// Предметы в пути, которые знает клиент (Msg::Transit) — номера стеков.
    transit_known: HashSet<u32>,
    tracked: HashMap<u32, Tracked>,
    /// Радиус правок (кусков) и кусок, вокруг которого клиент знает правки; куски, правки которых у клиента.
    pub(crate) view: i32,
    center: (i32, i32),
    known: HashSet<(i32, i32)>,
    /// Куски, чьи ChunkEdits ещё не ушли (бюджет EDIT_BUDGET): ближние впереди; правки таких кусков (Blocks) не шлются —
    /// кусок уйдёт целиком из мира на момент отправки. edit_used — уже отправлено с прошлого кадра (вход + кадр).
    edit_queue: VecDeque<(i32, i32)>,
    edit_used: usize,
    /// Номер последнего принятого тика клиента; взмах за этот тик.
    pub(crate) n: u32,
    swung: bool,
    /// Ждём TeleportAck с этим номером; номер следующего телепорта; нарушения движения.
    pub(crate) teleport: Option<u32>,
    next_tp: u32,
    pub(crate) violations: u32,
    /// Врата странствий (gen/spawn.rs): тиков подряд в проёме; раньше этого тика сервера врата не переносят.
    pub(crate) gate: u32,
    pub(crate) gate_ready: u64,
    /// Корзина тиков и счётчики действий этого тика.
    bucket: f32,
    uses: u8,
    breaks: u8,
    /// Пинг: (номер, время отправки, мс), последний замер.
    ping: Option<(u32, u64)>,
    pub(crate) rtt_ms: Option<u32>,
    /// Здоровье, воздух, горение (game/health.rs, game/server/survival.rs); что клиент знает о нём (Msg::Vitals).
    pub(crate) hp: Health,
    sent_vit: Option<Vitals>,
    /// Последнее обработанное приземление (Player.landed): урон падения считается один раз.
    fall_seen: Option<(u64, f32)>,
    /// От чего умер (пока мёртв).
    pub(crate) death_kind: Option<crate::game::health::DamageKind>,
    /// Ест: сервер решает, что и когда съедено (game/server/survival.rs).
    pub(crate) eating: Option<survival::Eating>,
    /// Сглаженный пинг для Tab (ListPing): `(старое·3 + новое) / 4`.
    ping_avg: Option<u32>,
    /// Корзина сообщений чата (частота).
    chat: ChatBucket,
    /// Тиков с последнего удара (lastAttackedTicks): перезарядка удара по мобу.
    pub(crate) atk_ticks: i32,
    /// Отбрасывание ударом моба: номер следующего и ждущее подтверждения (номер, скорость) — Msg::Velocity / VelocityAck.
    pub(crate) kb_next: u32,
    pub(crate) kb_pending: Option<(u32, [f64; 3])>,
    /// PvP (server/pvp.rs): тиков защиты зоны Перекрёстка (полная в зоне, 10 с на убыль после выхода) и метка боя после
    /// удара по игроку (защиты на это время нет).
    pub(crate) zone_grace: i32,
    pub(crate) pvp_tag: i32,
    /// Корзина команд чата (G17, server/admin.rs) и что клиент знает о режиме и правах (Msg::Mode).
    cmd: admin::CmdBucket,
    sent_mode: Option<(u8, u8)>,
}

impl ServerPlayer {
    pub(crate) fn new(id: u32, nick: &str) -> ServerPlayer {
        ServerPlayer {
            id,
            nick: nick.to_string(),
            skin: skin_of(nick),
            player: Player::new([8.5, 100.0, 8.5]),
            inv: Inventory::new(),
            screen: None,
            win: 0,
            next_win: 0,
            ui_ev: Vec::new(),
            breaking: None,
            online: false,
            token: None,
            away: false,
            inbox: VecDeque::new(),
            ack: None,
            win_out: Vec::new(),
            own: Vec::new(),
            outbox: Vec::new(),
            sent_inv: Vec::new(),
            sent_sel: 0,
            inv_force: false,
            win_force: false,
            win_rev: 0,
            win_rev_sent: 0,
            win_sent: Vec::new(),
            furnace_sent: None,
            mek_sent: Vec::new(),
            glow_sent: Vec::new(),
            sides_sent: HashMap::new(),
            transit_known: HashSet::new(),
            tracked: HashMap::new(),
            view: VIEW_DEFAULT as i32,
            center: (0, 0),
            known: HashSet::new(),
            edit_queue: VecDeque::new(),
            edit_used: 0,
            n: 0,
            swung: false,
            teleport: None,
            next_tp: 0,
            violations: 0,
            gate: 0,
            gate_ready: 0,
            bucket: BUCKET_MAX / 2.0,
            uses: 0,
            breaks: 0,
            ping: None,
            rtt_ms: None,
            hp: Health::new(),
            sent_vit: None,
            fall_seen: None,
            death_kind: None,
            eating: None,
            ping_avg: None,
            chat: ChatBucket::new(0),
            atk_ticks: 0,
            kb_next: 0,
            kb_pending: None,
            zone_grace: 0,
            pvp_tag: 0,
            cmd: admin::CmdBucket::new(0),
            sent_mode: None,
        }
    }
    fn feet_cell(&self) -> [i32; 3] {
        let p = &self.player;
        [p.pos[0] as i32, p.pos[1] as i32, p.pos[2] as i32]
    }
    fn inv_copy(&self) -> Vec<ItemStack> {
        (0..=OFFHAND).map(|i| self.inv.get(i)).collect()
    }
    fn chunk(&self) -> (i32, i32) {
        chunk_of(self.player.pos[0].floor() as i32, self.player.pos[2].floor() as i32)
    }
    /// Состояние на конец принятого тика — для других.
    fn moved(&self) -> PlayerMove {
        let p = &self.player;
        let mut flags = 0;
        for (on, f) in [(p.on_ground, PM_GROUND), (p.crouch, PM_CROUCH), (p.sprinting, PM_SPRINT), (p.flying, PM_FLY), (!self.hp.alive(), PM_DEAD)] {
            if on {
                flags |= f;
            }
        }
        PlayerMove { id: self.id, n: self.n, pos: p.pos.map(|v| v as f32), yaw: p.yaw, pitch: p.pitch, flags }
    }
}

/// Скин по нику (8 вариантов — MP-4; пока номер для модели).
fn skin_of(nick: &str) -> u8 {
    (nick.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 8) as u8
}

/// Счётчики сервера (проверки: в одной петле без задержки — нули).
#[derive(Default, Clone, Debug)]
pub(crate) struct ServerStats {
    /// Теневой тик разошёлся с итогом клиента, итог принят как правдоподобный.
    pub(crate) corrections: u32,
    /// Ход неправдоподобен — игрок возвращён (Teleport).
    pub(crate) teleports: u32,
    /// Действие отклонено проверкой (дальность, точка, лимит, прогресс ломания).
    pub(crate) rejected: u32,
    /// Хеш окна после клика разошёлся с клиентским.
    pub(crate) win_mismatch: u32,
    /// Хеш инвентаря после тика разошёлся с клиентским (ушла полная копия).
    pub(crate) inv_mismatch: u32,
}

/// Подбор этого тика: летящую копию видят все, кто следит за предметом (и подобравший) — §4.6.
struct PickupFx {
    item: u32,
    by: u32,
    msg: Msg,
}

pub(crate) struct ServerSim {
    pub(crate) sim: Sim,
    /// Вошедшие игроки — в порядке входа.
    pub(crate) players: Vec<ServerPlayer>,
    /// Игроки из хранилища и вышедшие, ещё не вошедшие снова.
    pub(crate) parked: Vec<ServerPlayer>,
    /// События тика с адресатом.
    pub(crate) events: Events,
    /// Звуки сервера (крышка сундука): высота тона.
    pub(crate) fx_rng: GameRng,
    /// Всем игрокам в этот кадр (вход и выход игроков); число смотрящих в сундук — тем, кто знает кусок.
    bcast: Vec<Msg>,
    pickups: Vec<PickupFx>,
    chest_viewers: Vec<([i32; 3], u8)>,
    /// Чужие трещины этого тика: (кто, клетка, стадия; −1 — убрать).
    stages: Vec<(u32, [i32; 3], i8)>,
    /// Отладка (полёт, набор, сдвиг) разрешена (локальный сервер; сеть — ключ --dev).
    pub(crate) debug: bool,
    /// Пинг игроков (сеть; локальной петле не нужен).
    pub(crate) ping: bool,
    /// Время для пинга, мс (ставит net.rs; у локального сервера — 0).
    pub(crate) now_ms: u64,
    pub(crate) name: String,
    pub(crate) max_players: u16,
    /// Ники по правилу `social::nick_ok` (3–16, латиница и кириллица) — сетевой сервер; локальный и проверки ядра — любые.
    pub(crate) strict_nicks: bool,
    /// Тиков сервера с начала процесса (не время суток: его двигает отладка) — частота чата и ListPing.
    ticks: u64,
    /// Бюджет правок на игрока за тик (EDIT_BUDGET); локальный сервер браузера — без предела (труба в памяти).
    pub(crate) edit_budget: usize,
    probe: Option<[u64; 4]>,
    /// Витрина Mekanism поставлена (raise_crossroads): её куски всегда в зоне, вход пополняется.
    showcase: bool,
    time_dirty: bool,
    bufs: Bufs,
    pub(crate) stats: ServerStats,
    /// Роли, журнал админ-команд, выкинуть, журнал правок для /rollback (G17, server/admin.rs).
    pub(crate) admin: admin::Admin,
}

impl ServerSim {
    /// Сервер над миром world (свой мир сервера: журнал записей включается здесь).
    pub(crate) fn new(world: World) -> ServerSim {
        let seed = world.seed() as u64;
        world.d.borrow_mut().enable_log();
        ServerSim {
            sim: Sim::new(&world, seed),
            players: Vec::new(),
            parked: Vec::new(),
            events: Events::default(),
            fx_rng: GameRng(seed ^ 0xEFF0_EC75),
            bcast: Vec::new(),
            pickups: Vec::new(),
            chest_viewers: Vec::new(),
            stages: Vec::new(),
            debug: false,
            ping: false,
            now_ms: 0,
            name: "локальный".into(),
            max_players: 32,
            strict_nicks: false,
            ticks: 0,
            edit_budget: EDIT_BUDGET,
            probe: None,
            showcase: false,
            time_dirty: false,
            bufs: Bufs::default(),
            stats: ServerStats::default(),
            admin: admin::Admin::default(),
        }
    }

    /// Локальный сервер для браузера и проверок: мир того же сида с копией правок и блок-сущностей src.
    pub(crate) fn from_world(src: &World) -> ServerSim {
        let w = World::new(src.seed());
        w.d.borrow_mut().copy_edits_from(&src.d.borrow());
        let mut s = ServerSim::new(w);
        s.debug = true;
        s.admin.open = true; // локальный сервер: команды всем (G17)
        s.edit_budget = usize::MAX;
        s.raise_crossroads();
        s
    }

    /// Перекрёсток при старте сервера (G13): зона защиты — заново поверх сохранённого мира, машины витрины на месте.
    pub(crate) fn raise_crossroads(&mut self) {
        let d = self.sim.world.d.clone();
        let mut ev = Events::default();
        crate::game::showcase::raise(&mut self.sim, &mut d.borrow_mut(), &mut ev);
        self.showcase = d.borrow().gen().site.active();
    }

    fn probe(&mut self) -> [u64; 4] {
        *self.probe.get_or_insert_with(probe)
    }

    pub(crate) fn idx(&self, id: u32) -> Option<usize> {
        self.players.iter().position(|p| p.id == id)
    }
    pub(crate) fn player(&self, id: u32) -> Option<&ServerPlayer> {
        self.players.iter().find(|p| p.id == id)
    }
    pub(crate) fn player_mut(&mut self, id: u32) -> Option<&mut ServerPlayer> {
        self.players.iter_mut().find(|p| p.id == id)
    }

    // ------------------------------------------------------------ вход и выход

    /// Вход (§3.2): проверки, игрок из хранилища или новый у точки появления; Welcome, список игроков, индекс и правки
    /// в радиусе View.
    pub(crate) fn join(&mut self, h: &Hello) -> Result<u32, (Reject, String)> {
        if let Some(text) = outdated(h.proto, &h.build) {
            return Err((Reject::Outdated, text));
        }
        if self.strict_nicks {
            if let Err(rule) = nick_ok(&h.nick) {
                return Err((Reject::BadNick, rule.into()));
            }
        }
        let n = h.nick.chars().count();
        // Ключ записи игрока должен получиться (ник → имя файла): иначе одна запись не прошла бы в хранилище и остановила
        // бы автосохранение всем.
        if n == 0 || n > 32 || h.nick.contains('/') || h.nick.chars().any(char::is_control) || crate::save::player_key(&h.nick).is_err() {
            return Err((Reject::BadNick, "ник: 1–32 символа без «/»".into()));
        }
        let key = h.nick.to_lowercase();
        if let Some(on) = self.players.iter().find(|p| p.nick.to_lowercase() == key) {
            // Ник в игре и закреплён за другим токеном — повтор не поможет (BadToken). Тот же токен сюда не доходит:
            // прежний сеанс вытесняется раньше (net.rs, same_session); гость и незакреплённый ник — «занят», ждать.
            let given = token_hash(&key, &h.token);
            if on.token.is_some_and(|stored| !given.is_some_and(|g| same_hash(&g, &stored))) {
                return Err((Reject::BadToken, "Этот ник занят другим игроком (другой браузер). Выберите другой ник.".into()));
            }
            return Err((Reject::NickTaken, "ник занят".into()));
        }
        if self.players.len() >= self.max_players as usize {
            return Err((Reject::Full, "мест нет".into()));
        }
        if h.probe != self.probe() {
            return Err((Reject::GenMismatch, "генератор клиента даёт другие куски (проба)".into()));
        }
        if let Some(why) = self.admin.roles.bans.get(&key) {
            let text = if why.is_empty() { "Вы забанены на этом сервере".to_string() } else { format!("Вы забанены на этом сервере: {why}") };
            return Err((Reject::Banned, text));
        }
        // Токен (§3.2): ник закреплён — войдёт только тот же токен; свободный — закрепится за первым с токеном (гость, все
        // нули, не закрепляет и закреплённый ник не откроет). Проверка до любых изменений: отказ ничего не двигает.
        let at = self.parked.iter().position(|p| p.nick.to_lowercase() == key);
        let given = token_hash(&key, &h.token);
        if let Some(stored) = at.and_then(|i| self.parked[i].token) {
            if !given.is_some_and(|g| same_hash(&g, &stored)) {
                return Err((Reject::BadToken, "Этот ник занят другим игроком (другой браузер). Выберите другой ник.".into()));
            }
        }
        let mut p = match at {
            Some(i) => self.parked.remove(i),
            None => {
                let id = self.sim.ids.next();
                let mut p = ServerPlayer::new(id, &h.nick);
                let (pos, yaw) = self.spawn_point();
                place_at_spawn(&mut p.player, pos);
                p.player.yaw = yaw;
                p
            }
        };
        if p.id == 0 {
            p.id = self.sim.ids.next();
        }
        // Здоровье живёт, пока сервер работает (на диск пока не пишется — save/ позже): вышел и вошёл — то же, что было;
        // записанный на диске входит с полным. Неуязвимость входа — 60 тиков (joinInvulnerabilityTicks).
        p.hp.spawn_protect = crate::game::health::SPAWN_PROTECT;
        p.eating = None;
        p.death_kind = None;
        p.sent_vit = None;
        if p.token.is_none() {
            p.token = given; // закрепить; гость остаётся без токена
        }
        p.online = true;
        p.away = false;
        p.view = if h.view == 0 { VIEW_DEFAULT as i32 } else { (h.view as i32).clamp(2, VIEW_MAX) };
        p.sent_inv = p.inv_copy();
        p.sent_sel = p.inv.selected;
        p.bucket = BUCKET_MAX / 2.0;
        p.chat = ChatBucket::new(self.ticks);
        p.cmd = admin::CmdBucket::new(self.ticks);
        // Режим и права — в первом кадре, до первого тика клиента (наблюдатель сразу сквозь стены).
        let mode = (p.player.mode.code(), self.level_for(&p));
        p.sent_mode = Some(mode);
        p.player.noclip = false; // полёт сквозь стены не переживает вход (клиент начинает без него); полёт выживания — да
        if p.player.mode == crate::game::rules::GameMode::Spectator {
            p.player.set_flying(true); // наблюдатель — сквозь стены всегда (клиенту — Msg::Mode первым кадром)
        }
        let me = SelfState {
            pos: p.player.pos,
            vel: p.player.vel,
            yaw: p.player.yaw,
            pitch: p.player.pitch,
            fall: p.player.fall,
            on_ground: p.player.on_ground,
            flying: p.player.flying,
            crouch: p.player.crouch,
            selected: p.inv.selected as u8,
            inv: (0..=OFFHAND).filter_map(|i| Some(p.inv.get(i)).filter(|s| !s.is_empty()).map(|s| (i as u8, s))).collect(),
        };
        let s = &self.sim;
        p.outbox.push(S2C::Welcome(Welcome {
            you: p.id,
            seed: s.world.seed(),
            gen_version: GEN_VERSION,
            time: s.time,
            day: s.day,
            cycle: s.day_cycle,
            me,
            server_name: self.name.clone(),
            online: self.players.len() as u16 + 1,
            max: self.max_players,
            debug: self.debug,
        }));
        // Список игроков (ники над головой, Tab — MP-4); о новом — всем в следующем кадре.
        let mut msgs: Vec<Msg> = self.players.iter().map(|o| Msg::ListAdd { id: o.id, nick: o.nick.clone(), skin: o.skin }).collect();
        let pings: Vec<(u32, u16)> = self.players.iter().filter_map(|o| Some((o.id, o.ping_avg?.min(u16::MAX as u32) as u16))).collect();
        if !pings.is_empty() {
            msgs.push(Msg::ListPing(pings)); // Tab сразу с пингом остальных, а не через 2 с
        }
        // Правки вокруг (§3.5): индекс, затем куски от ближних к дальним; дальше — по мере хода (flush).
        let (cx, cz) = p.chunk();
        let w = s.world.d.borrow();
        let r = p.view;
        let mut near: Vec<(i32, i32)> = w.edited_chunks().filter(|c| (c.0 - cx).abs() <= r && (c.1 - cz).abs() <= r).collect();
        near.sort_by_key(|c| ((c.0 - cx).pow(2) + (c.1 - cz).pow(2), *c));
        msgs.push(Msg::Mode { mode: mode.0, level: mode.1 });
        msgs.push(Msg::EditIndex(near.iter().map(|&c| (c.0, c.1, w.chunk_edits(c.0, c.1).len() as u32)).collect()));
        p.center = (cx, cz);
        p.known = near.iter().copied().collect();
        p.edit_queue = near.into();
        p.edit_used = 0;
        send_edits(&mut p, &w, self.edit_budget, &mut msgs);
        drop(w);
        p.outbox.push(S2C::Bundle(Bundle { tick: s.time, msgs }));
        self.bcast.push(Msg::ListAdd { id: p.id, nick: p.nick.clone(), skin: p.skin });
        self.bcast.push(Msg::Chat { kind: CHAT_JOIN, from: p.id, text: format!("{} присоединился к игре", p.nick) });
        let id = p.id;
        self.players.push(p);
        Ok(id)
    }

    /// Точка появления на Перекрёстке (вход нового игрока; возрождение после смерти — она же): ступни и yaw (лицом к
    /// Маяку). Не в телах игроков в сети; номер попытки — тик и число игроков (детерминированно).
    pub(crate) fn spawn_point(&mut self) -> ([f64; 3], f32) {
        let others: Vec<[f64; 3]> = self.players.iter().filter(|p| p.online).map(|p| p.player.pos).collect();
        let n = self.sim.time.wrapping_mul(31).wrapping_add(self.players.len() as u64);
        crossroads_spawn(&mut self.sim.world.d.borrow_mut(), n, &others)
    }

    /// Перенести игрока сервером (Врата странствий, возрождение): тело — на место в покое, клиенту — Teleport (до
    /// TeleportAck его ходы не принимаются); правки кусков у нового места уйдут в flush (смена куска).
    pub(crate) fn relocate(&mut self, i: usize, pos: [f64; 3], yaw: f32) {
        self.stop_breaking(i);
        let p = &mut self.players[i];
        place_at_spawn(&mut p.player, pos);
        p.player.yaw = yaw;
        p.next_tp = p.next_tp.wrapping_add(1).max(1);
        p.teleport = Some(p.next_tp);
        p.own.push(Msg::Teleport { id: p.next_tp, pos, yaw, pitch: p.player.pitch });
    }

    /// Пашня стала землёй (Block.pushEntitiesUpBeforeBlockChange): стоявших на ней игроков — на 1/16 вверх, на верх нового блока
    /// (иначе тело оказывается внутри блока и проваливается); клиенту — Teleport.
    fn lift_standing(&mut self) {
        for q in std::mem::take(&mut self.sim.lifted) {
            let slab = Aabb { lo: [q[0] as f64, q[1] as f64 + 15.0 / 16.0, q[2] as f64], hi: [q[0] as f64 + 1.0, q[1] as f64 + 1.0, q[2] as f64 + 1.0] };
            for i in 0..self.players.len() {
                if self.players[i].player.bbox().intersects(&slab) {
                    let (pos, yaw) = (self.players[i].player.pos, self.players[i].player.yaw);
                    self.relocate(i, [pos[0], q[1] as f64 + 1.0, pos[2]], yaw);
                }
            }
        }
    }

    /// Возрождение на Перекрёстке (spawn.md §6; G5 «выживание» зовёт после смерти): точка появления и перенос.
    pub(crate) fn respawn_at_crossroads(&mut self, i: usize) {
        let (pos, yaw) = self.spawn_point();
        self.relocate(i, pos, yaw);
    }

    /// Врата странствий: кто простоял в проёме GATE_TICKS — переносится в случайное место суши GATE_NEAR…GATE_FAR от
    /// площади, лицом к Маяку (луч виден над горизонтом); перезарядка GATE_COOLDOWN.
    fn tick_gates(&mut self) {
        let time = self.sim.time;
        let mut go = Vec::new();
        {
            let w = self.sim.world.d.borrow();
            let site = &w.gen().site;
            if !site.active() {
                return;
            }
            for (i, p) in self.players.iter_mut().enumerate() {
                let inside = p.online && !p.away && p.teleport.is_none() && site.in_gate(crate::game::interact::feet(&p.player));
                p.gate = if inside && time >= p.gate_ready { p.gate + 1 } else { 0 };
                if p.gate >= GATE_TICKS {
                    p.gate = 0;
                    p.gate_ready = time + GATE_COOLDOWN;
                    go.push(i);
                }
            }
        }
        for i in go {
            let (pos, yaw) = self.wander_point(i);
            self.relocate(i, pos, yaw);
        }
    }

    /// Место для Врат странствий: суша в кольце GATE_NEAR…GATE_FAR от площади, не ближе GATE_ALONE к другим игрокам;
    /// взгляд — на площадь. Детерминированно по тику и номеру игрока.
    fn wander_point(&mut self, i: usize) -> ([f64; 3], f32) {
        let id = self.players[i].id;
        let others: Vec<[f64; 3]> = self.players.iter().filter(|p| p.online && p.id != id).map(|p| p.player.pos).collect();
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let (sx, sz) = (w.gen().site.x as f64 + 0.5, w.gen().site.z as f64 + 0.5);
        let mut rng = GameRng(self.sim.time.wrapping_mul(0x9E37_79B9) ^ id as u64 ^ 0xA7E5);
        let mut best = None;
        for _ in 0..24 {
            let a = rng.next_f64() * std::f64::consts::TAU;
            let r = GATE_NEAR + rng.next_f64() * (GATE_FAR - GATE_NEAR);
            let (x, z) = ((sx + r * libm::cos(a)).floor() as i32, (sz + r * libm::sin(a)).floor() as i32);
            let p = find_spawn(&mut w, x, z);
            let alone = others.iter().all(|o| (o[0] - p[0]).hypot(o[2] - p[2]) >= GATE_ALONE);
            if p[1] > (crate::cell::SEA + 1) as f64 && alone {
                best = Some(p);
                break;
            }
            best.get_or_insert(p);
        }
        let p = best.unwrap();
        // Взгляд на площадь (yaw Minecraft: 0 — на +Z, 90 — на −X).
        let yaw = libm::atan2(-(sx - p[0]), sz - p[2]).to_degrees() as f32;
        (p, yaw)
    }

    /// Вошедший игрок с тем же ником и тем же токеном, что в Hello: то же устройство вошло снова (обрыв, который сервер
    /// ещё не заметил, перезагрузка вкладки). Прежний сеанс уступает — иначе переподключение упёрлось бы в «ник занят».
    pub(crate) fn same_session(&self, h: &Hello) -> Option<u32> {
        let key = h.nick.to_lowercase();
        let given = token_hash(&key, &h.token)?;
        self.players.iter().find(|p| p.nick.to_lowercase() == key && p.token.is_some_and(|t| same_hash(&t, &given))).map(|p| p.id)
    }

    /// Уступивший сеанс (same_session) ушёл, а номер сущности тот же вернётся с новым входом: другим игрокам — Despawn и
    /// забыть слежение, иначе они не получат Spawn заново и не увидят вошедшего.
    pub(crate) fn forget_for_others(&mut self, id: u32) {
        for o in &mut self.players {
            o.tracked.remove(&id);
        }
        self.bcast.push(Msg::Despawn(vec![id]));
    }

    /// Выход: окно закрывается (вещи — в инвентарь), игрок ждёт в parked (запись — save/); другим — ListRemove и
    /// Despawn (по слежению).
    pub(crate) fn leave(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        self.close_screen(i, true);
        if let Some(b) = self.players[i].breaking.take() {
            self.stages.push((id, b.pos, -1));
        }
        if !self.players[i].hp.alive() {
            self.reset_after_death(i); // вышел мёртвым — вернётся живым на точке появления (вещи уже выпали)
        }
        let mut p = self.players.remove(i);
        p.online = false;
        p.away = false;
        p.inbox.clear();
        p.outbox.clear();
        p.own.clear();
        p.tracked.clear();
        p.known.clear();
        p.transit_known.clear(); // новый сеанс начнёт без копий: стеки и свечение придут заново
        p.glow_sent.clear();
        p.sides_sent.clear();
        p.edit_queue.clear();
        p.edit_used = 0;
        p.teleport = None;
        self.bcast.push(Msg::ListRemove(id));
        self.bcast.push(Msg::Chat { kind: CHAT_LEAVE, from: id, text: format!("{} вышел из игры", p.nick) });
        self.parked.push(p);
    }

    /// Сообщение игрока id.
    pub(crate) fn handle(&mut self, id: u32, m: C2S) {
        let Some(i) = self.idx(id) else { return };
        match m {
            C2S::Tick(t) => {
                let p = &mut self.players[i];
                if !p.away && p.inbox.len() < INBOX_MAX {
                    p.inbox.push_back(t); // переполнение — лишние тики («ускоренный таймер») отбрасываются
                }
            }
            C2S::Away { on } => self.set_away(i, on),
            // Действия между тиками (локальный сервер: проверки и отладка ставят мир между тиками).
            C2S::Acts(list) if self.debug => {
                for a in list {
                    self.apply_act(i, a);
                }
            }
            C2S::Pong { id: k } => {
                let p = &mut self.players[i];
                if let Some((sent_id, at)) = p.ping {
                    if sent_id == k {
                        let rtt = self.now_ms.saturating_sub(at).min(u32::MAX as u64) as u32;
                        p.rtt_ms = Some(rtt);
                        p.ping_avg = Some(smooth_ping(p.ping_avg, rtt));
                        p.ping = None;
                    }
                }
            }
            C2S::Chat { text } => self.chat_in(i, &text),
            C2S::View { radius } => {
                // Смена на ходу: сервер пересчитает известные куски (know_chunks) в этом же тике — вошедшие в
                // радиус придут целиком, ушедшие за радиус с запасом — Forget.
                let p = &mut self.players[i];
                p.view = (radius as i32).clamp(2, VIEW_MAX);
                p.center = (i32::MAX, i32::MAX);
            }
            C2S::Respawn => self.respawn(i),
            _ => {} // PvP — MP-7
        }
    }

    /// Строка в чат всем (G2): вход и выход идут отсюда же. kind — `CHAT_*` (proto.rs): для CHAT_PLAYER from — отправитель,
    /// text — его слова; для остальных text — готовая строка. Доставка — в кадре этого тика, отправитель получает её тоже.
    /// Для выживания: смерть — `announce(CHAT_DEATH, id, "Ник разбился")`.
    pub(crate) fn announce(&mut self, kind: u8, from: u32, text: String) {
        self.bcast.push(Msg::Chat { kind, from, text });
    }

    /// Строка в чат одному игроку (ответ на команду, отказ): в его кадр этого тика.
    pub(crate) fn tell(&mut self, id: u32, kind: u8, text: String) {
        if let Some(p) = self.player_mut(id) {
            p.own.push(Msg::Chat { kind, from: 0, text });
        }
    }

    /// Сообщение игрока (G2, mp/ui.md §4): текст чистится (управляющие знаки, пробелы, 256 символов), частота — корзина
    /// `ChatBucket`; «/…» — команда, ответ только спросившему; остальное — всем.
    fn chat_in(&mut self, i: usize, raw: &str) {
        let text = clean_chat(raw);
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            return self.command_in(i, &text); // команды (G17): своя корзина частоты, права — server/admin.rs
        }
        let (id, now) = (self.players[i].id, self.ticks);
        if self.admin.roles.mutes.contains(&self.players[i].nick.to_lowercase()) {
            return self.tell(id, CHAT_DENY, "Вам запрещено писать в чат".into());
        }
        if !self.players[i].chat.take(now) {
            return self.tell(id, CHAT_DENY, CHAT_TOO_FAST.into());
        }
        self.announce(CHAT_PLAYER, id, text)
    }

    /// Away { on } (§1.3). Ушёл: ломание снимается (трещины у других), тики с этого момента не принимаются, тело тикает
    /// без ввода. Вернулся: отброшенное не догоняем — игрок получает Resume с серверным состоянием и подтверждает его
    /// (TeleportAck, до него ходы не принимаются). Resume — на любой Away { on: false }, даже если Away { on: true }
    /// не было: клиент ждёт ответа и без него не тикает.
    fn set_away(&mut self, i: usize, on: bool) {
        if on {
            if !self.players[i].away {
                self.stop_breaking(i);
                self.players[i].away = true;
            }
            return;
        }
        let p = &mut self.players[i];
        p.away = false;
        p.inbox.clear();
        p.next_tp = p.next_tp.wrapping_add(1).max(1);
        p.teleport = Some(p.next_tp);
        // Клики, пришедшие перед Away, брошены вместе с inbox: открытое окно и инвентарь клиенту — заново.
        p.win_force = true;
        p.inv_force = true;
        p.own.push(Msg::Resume { id: p.next_tp, n: p.n, mv: move_of(&p.player, 0) });
    }

    /// Тик ушедшего игрока (§1.3): физика без ввода по серверному миру, номер тика для других растёт, окно держится,
    /// пока рядом блок (tick_screen).
    fn tick_away(&mut self, i: usize) {
        self.hp_pre(i);
        {
            let d = self.sim.world.d.clone();
            let mut w = d.borrow_mut();
            let p = &mut self.players[i];
            p.player.tick(&mut *w, Keys::default(), false);
            p.n = p.n.wrapping_add(1);
        }
        self.hp_post(i);
        self.tick_screen(i);
    }

    // ------------------------------------------------------------ тик

    /// Тик сервера (§2.3).
    pub(crate) fn tick(&mut self) {
        self.ticks += 1;
        self.tick_pvp();
        for i in 0..self.players.len() {
            let p = &mut self.players[i];
            p.bucket = (p.bucket + 1.0).min(BUCKET_MAX);
            let mut ticked = false;
            for _ in 0..MAX_TICKS_PER_TICK {
                if self.players[i].bucket < 1.0 {
                    break;
                }
                let Some(t) = self.players[i].inbox.pop_front() else { break };
                self.players[i].bucket -= 1.0;
                self.process_tick(i, t);
                ticked = true;
            }
            // Ушедший игрок: тики от него не ждём, но тело живёт — падает, тормозит (его последние тики, пришедшие до
            // Away, обработаны выше).
            if self.players[i].away && !ticked {
                self.tick_away(i);
            }
        }
        self.tick_gates();
        let mut zone = Zone::default();
        for p in &self.players {
            zone.add_around(p.player.pos);
        }
        // Витрина Mekanism работает и без игроков рядом (G13): её куски всегда в зоне; вход пополняется раз в секунду.
        if self.showcase {
            let mut w = self.sim.world.d.borrow_mut();
            for (cx, cz) in crate::game::showcase::chunks(&w) {
                zone.add_square(cx, cz, 0);
            }
            if self.ticks % crate::game::showcase::REFILL_EVERY == 0 {
                crate::game::showcase::refill(&mut w);
            }
        }
        self.sim.tick_world(zone, &mut self.events);
        self.lift_standing();
        self.tick_mobs();
        self.tick_pickup();
        self.admin_tick();
        self.flush();
    }

    /// Тик игрока: взгляд этого тика, действия по порядку, прогресс ломания, теневой тик, сверка инвентаря,
    /// доступность окна.
    fn process_tick(&mut self, i: usize, t: ClientTick) {
        let p = &mut self.players[i];
        p.uses = 0;
        p.breaks = 0;
        p.atk_ticks = p.atk_ticks.saturating_add(1);
        if t.mv.yaw.is_finite() && t.mv.pitch.is_finite() {
            p.player.yaw = t.mv.yaw;
            p.player.pitch = t.mv.pitch.clamp(-90.0, 90.0);
        }
        for a in t.acts {
            self.apply_act(i, a);
        }
        self.hp_pre(i);
        self.mine_tick(i);
        self.shadow(i, &t.mv);
        self.hp_post(i);
        let p = &mut self.players[i];
        // Номер тика для других игроков растёт всегда: после ухода (Away) и перезахода клиент считает с меньшего.
        let next = p.n.wrapping_add(1);
        p.n = if t.n.wrapping_sub(next) as i32 > 0 { t.n } else { next };
        if inv_hash(&p.inv) == t.inv {
            p.sent_inv = p.inv_copy(); // у клиента то же (его предсказание сошлось)
        } else {
            p.inv_force = true;
            self.stats.inv_mismatch += 1;
        }
        self.tick_screen(i);
    }

    /// Прогресс ломания за принятый тик (§4.2): тот же break_delta по состоянию начала тика (до движения), что у
    /// клиента; стадия — другим.
    fn mine_tick(&mut self, i: usize) {
        let Some(mut b) = self.players[i].breaking else { return };
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let id = self.players[i].id;
        let block = w.get_block(b.pos[0], b.pos[1], b.pos[2]);
        if block == AIR {
            self.players[i].breaking = None;
            if b.stage >= 0 {
                self.stages.push((id, b.pos, -1));
            }
            return;
        }
        let p = &self.players[i];
        b.progress += mine_delta(&mut w, &p.player, &p.inv, block);
        let stage = if b.active { ((b.progress * 10.0) as i32).clamp(0, 9) as i8 } else { -1 };
        if stage != b.stage {
            b.stage = stage;
            self.stages.push((id, b.pos, stage));
        }
        self.players[i].breaking = Some(b);
    }

    /// Теневой тик (§1.3): тот же Player::tick по присланным клавишам на мире сервера; итог сверяется с клиентским.
    /// Совпало — принят. Не совпало: правдоподобно (числа конечны и в мире, отличие ≤ MOVE_SLACK, путь коробки тела
    /// свободен; полёт — если разрешён (game/rules.rs), сквозь стены — только с отладкой) — принять итог клиента (опора проверяется, падение — наибольшее); иначе —
    /// назад в состояние прошлого тика со скоростью 0, Teleport и ожидание TeleportAck (до него ходы не принимаются).
    fn shadow(&mut self, i: usize, mv: &Move) {
        if self.players[i].teleport.is_some() {
            return; // ход посчитан ещё до телепорта
        }
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let debug = self.debug;
        let p = &mut self.players[i];
        let before = p.player.pos;
        let was_ground = p.player.on_ground;
        let finite = mv.pos.iter().chain(mv.vel.iter()).all(|v| v.is_finite()) && mv.fall.is_finite() && mv.fwd_in.is_finite();
        let in_world = finite && mv.pos[0].abs() < 3.0e7 && mv.pos[2].abs() < 3.0e7 && mv.pos[1] > -128.0 && mv.pos[1] < 512.0;
        let eating = p.eating.is_some(); // ест: ввод ×0.2 (ClientPlayerEntity.tickMovement)
        p.player.tick(&mut *w, keys_of(mv.keys as u32), eating);
        if in_world && same_move(&p.player, mv) {
            return;
        }
        let flying = mv.flags & MV_FLY != 0;
        let noclip = mv.flags & MV_NOCLIP != 0;
        let mode = p.player.mode;
        let fly_ok = (!noclip || (p.player.noclip && crate::game::rules::may_noclip(mode, debug))) && (!flying || p.player.flying || crate::game::rules::may_fly(mode));
        let near = (0..3).map(|k| (mv.pos[k] - p.player.pos[k]).powi(2)).sum::<f64>() <= MOVE_SLACK * MOVE_SLACK;
        let path = noclip || {
            let h = if mv.flags & MV_CROUCH != 0 { 1.5 } else { 1.8 };
            let b = Aabb { lo: [before[0] - 0.3, before[1], before[2] - 0.3], hi: [before[0] + 0.3, before[1] + h, before[2] + 0.3] };
            let delta = [mv.pos[0] - before[0], mv.pos[1] - before[1], mv.pos[2] - before[2]];
            let got = collide(&mut *w, &b, delta, was_ground, STEP, &mut self.bufs);
            (0..3).all(|k| (got[k] - delta[k]).abs() < 1e-4)
        };
        if in_world && fly_ok && near && path {
            let fall = p.player.fall.max(mv.fall);
            adopt_move(&mut p.player, mv);
            p.player.fall = fall;
            if p.player.on_ground && !flying {
                // Заявленная опора: под коробкой есть форма (NoFall — ASP-12).
                let q = mv.pos;
                let slab = Aabb { lo: [q[0] - 0.3, q[1] - 0.01, q[2] - 0.3], hi: [q[0] + 0.3, q[1], q[2] + 0.3] };
                if space_empty(&mut *w, &slab, &mut self.bufs.a) {
                    p.player.on_ground = false;
                }
            }
            self.stats.corrections += 1;
            return;
        }
        p.player.teleport(before);
        p.next_tp = p.next_tp.wrapping_add(1).max(1);
        p.teleport = Some(p.next_tp);
        p.violations += 1;
        p.own.push(Msg::Teleport { id: p.next_tp, pos: before, yaw: p.player.yaw, pitch: p.player.pitch });
        self.stats.teleports += 1;
    }

    /// Дальность до клетки (§4.2–4.3): от глаз до ближайшей точки коробки блока — меньше REACH + 1.
    fn reach_ok(&self, i: usize, pos: [i32; 3]) -> bool {
        let p = &self.players[i].player;
        let eye = p.eye(1.0);
        let reach = if self.creative_of(i) { REACH_CREATIVE } else { REACH } + 1.0;
        let d2: f64 = (0..3)
            .map(|k| {
                let c = eye[k].clamp(pos[k] as f64, pos[k] as f64 + 1.0);
                (c - eye[k]).powi(2)
            })
            .sum();
        d2 < reach * reach
    }

    /// Клетка в зоне Перекрёстка (gen/spawn.rs): ломать и ставить нельзя.
    fn protected(&self, pos: [i32; 3]) -> bool {
        protected(&self.sim.world.d.borrow(), pos)
    }

    /// Отказ в действии: предсказание клиента откатится по Ack; инвентарь — полной копией.
    fn reject(&mut self, i: usize, seq: u32) {
        self.players[i].inv_force = true;
        self.stats.rejected += 1;
        self.ack(i, seq);
    }

    /// Ломание закончено или начато другое: запись снять, другим — трещины убрать.
    fn stop_breaking(&mut self, i: usize) {
        if let Some(b) = self.players[i].breaking.take() {
            if b.stage >= 0 {
                self.stages.push((self.players[i].id, b.pos, -1));
            }
        }
    }

    /// Действие игрока i (proto Act) — тем же кодом, что предсказал клиент, после проверок.
    pub(crate) fn apply_act(&mut self, i: usize, a: Act) {
        // Мёртвый игрок мир не трогает и окон не открывает (ввод обнулён, ASP-12 §10.1): отказ с откатом предсказания.
        if !self.players[i].hp.alive() {
            match a {
                Act::BreakStart { seq, .. } | Act::BreakDone { seq, .. } | Act::Use { seq, .. } | Act::UseAir { seq, .. } | Act::Bucket { seq, .. } => return self.reject(i, seq),
                Act::Click { .. } | Act::QuickMoveAll { .. } | Act::Drop { .. } | Act::Pick { .. } | Act::OpenInventory | Act::Attack { .. } | Act::Swing { .. } => {
                    self.players[i].inv_force = true;
                    return;
                }
                _ => {}
            }
        }
        // Наблюдатель (G17) мир не трогает, окон не открывает, не бьёт и не бросает: отказ с откатом предсказания.
        if crate::game::rules::spectator(&self.players[i].player) {
            match a {
                Act::BreakStart { seq, .. } | Act::BreakDone { seq, .. } | Act::Use { seq, .. } | Act::UseAir { seq, .. } | Act::Bucket { seq, .. } => return self.reject(i, seq),
                Act::Click { .. }
                | Act::QuickMoveAll { .. }
                | Act::Drop { .. }
                | Act::Pick { .. }
                | Act::OpenInventory
                | Act::Attack { .. }
                | Act::Swing { .. }
                | Act::UseEntity { .. }
                | Act::ItemMode { .. }
                | Act::WinButton { .. } => {
                    self.players[i].inv_force = true;
                    return;
                }
                _ => {}
            }
        }
        match a {
            Act::Click { win, rev, slot, button, action, hash } => {
                if self.players[i].screen.is_none() || win != self.players[i].win {
                    return; // окно уже другое: клик к закрытому окну
                }
                self.players[i].win_rev = rev;
                self.with_screen(i, |m, u| m.click(u, slot as i32, button as i32, action as i32));
                self.check_win(i, hash);
            }
            Act::QuickMoveAll { win, rev, slot, item, damage, hash } => {
                if self.players[i].screen.is_none() || win != self.players[i].win || slot < 0 {
                    return;
                }
                self.players[i].win_rev = rev;
                let like = ItemStack { item, count: 1, damage };
                self.with_screen(i, |m, u| m.quick_move_all(u, slot as usize, like));
                self.check_win(i, hash);
            }
            Act::CloseWindow { win } => {
                if self.players[i].screen.is_some() && win == self.players[i].win {
                    self.close_screen(i, true);
                }
            }
            Act::WinButton { win, widget } => {
                let Some(MenuKind::Mek(pos, _)) = self.players[i].screen.as_ref().map(|m| m.kind) else { return };
                if win == self.players[i].win && !self.protected(pos) {
                    if let Some(m) = self.sim.world.d.borrow_mut().be_mut().mek_mut(pos) {
                        m.press(widget as usize);
                    }
                }
            }
            Act::Hotbar(s) => {
                if (s as usize) < 9 {
                    if self.players[i].inv.selected != s as usize {
                        self.players[i].atk_ticks = 0; // другой предмет — перезарядка заново
                    }
                    self.players[i].inv.selected = s as usize;
                    self.players[i].sent_sel = s as usize;
                }
            }
            Act::OpenInventory => self.open_screen(i, Menu::player()),
            Act::Drop { all } => self.drop_selected(i, all),
            Act::ItemMode { back } => {
                crate::game::mek_tool::cycle_hand(self.players[i].inv.hand_mut(), back);
            }
            // F: полёт выживания (G16). Запрещён (game/rules.rs) — клиенту его состояние обратно.
            Act::Flight(on) => {
                let p = &mut self.players[i];
                if p.player.mode == crate::game::rules::GameMode::Spectator {
                    p.own.push(Msg::Flight(true)); // наблюдатель летает всегда
                } else if !on || crate::game::rules::may_fly(p.player.mode) {
                    p.player.set_flight(on);
                } else {
                    p.own.push(Msg::Flight(p.player.flying));
                }
            }
            Act::Swing { .. } => self.players[i].swung = true,
            Act::BreakStart { pos, seq, .. } => {
                self.players[i].breaks += 1;
                self.stop_breaking(i);
                if self.players[i].breaks > MAX_BREAKS || !self.reach_ok(i, pos) || self.protected(pos) {
                    return self.reject(i, seq);
                }
                let creative = self.creative_of(i);
                let p = &self.players[i];
                let instant = creative || {
                    let d = self.sim.world.d.clone();
                    let mut w = d.borrow_mut();
                    let b = w.get_block(pos[0], pos[1], pos[2]);
                    b != AIR && mine_delta(&mut w, &p.player, &p.inv, b) >= 1.0
                };
                if instant {
                    self.auth(i, |c| break_block(c, pos));
                } else {
                    self.players[i].breaking = Some(Breaking { pos, progress: 0.0, stage: -1, active: true });
                }
                self.ack(i, seq);
            }
            Act::BreakAbort { seq, .. } => {
                // Отмена: трещины у других убрать, запись оставить (см. Breaking).
                let id = self.players[i].id;
                if let Some(b) = self.players[i].breaking.as_mut() {
                    b.active = false;
                    if b.stage >= 0 {
                        b.stage = -1;
                        self.stages.push((id, b.pos, -1));
                    }
                }
                self.ack(i, seq);
            }
            Act::BreakDone { pos, seq } => {
                // Прогресс по принятым тикам (§4.2): сумма delta с BreakStart ≥ BREAK_MIN; клетка та же; дальность.
                self.players[i].breaks += 1;
                let creative = self.creative_of(i);
                let p = &self.players[i];
                let ok = p.breaks <= MAX_BREAKS
                    && (creative || p.breaking.is_some_and(|b| b.pos == pos && b.progress >= BREAK_MIN))
                    && self.reach_ok(i, pos)
                    && !self.protected(pos);
                self.stop_breaking(i);
                if !ok {
                    return self.reject(i, seq);
                }
                self.auth(i, |c| break_block(c, pos));
                self.ack(i, seq);
            }
            Act::Use { hand, pos, face, at, inside, seq } => {
                // Дальность, точка попадания в ±1.0000001 от центра клетки (ASP-05 §2.5), не больше MAX_USES за тик.
                self.players[i].uses += 1;
                let at_ok = at.iter().all(|&v| v.is_finite() && (v as f64 - 0.5).abs() < 1.0000001);
                if self.players[i].uses > MAX_USES || !at_ok || face > 5 || hand > 1 || !self.reach_ok(i, pos) {
                    return self.reject(i, seq);
                }
                let point = std::array::from_fn(|k| pos[k] as f64 + at[k] as f64);
                let h = Hit { pos, face, point, inside };
                let (_, open) = self.auth(i, |c| interact_block(c, hand as usize, h));
                if let Some(m) = open {
                    self.open_screen(i, m);
                }
                self.ack(i, seq);
            }
            Act::UseAir { hand, seq } => self.use_air(i, hand, seq),
            Act::Bucket { hand, pos, face, seq } => {
                // Ведро (ASP-15 §5): дальность и лимит использований, дальше — то же решение, что у клиента.
                self.players[i].uses += 1;
                if self.players[i].uses > MAX_USES || face > 5 || hand > 1 || !self.reach_ok(i, pos) {
                    return self.reject(i, seq);
                }
                let point = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
                let h = Hit { pos, face, point, inside: false };
                self.auth(i, |c| use_bucket(c, hand as usize, h));
                self.ack(i, seq);
            }
            Act::Pick { pos } => {
                if !self.reach_ok(i, pos) {
                    self.players[i].inv_force = true;
                    return;
                }
                let d = self.sim.world.d.clone();
                let creative = crate::game::rules::creative(&self.players[i].player);
                let p = &mut self.players[i];
                item_pick(&mut d.borrow_mut(), &mut p.inv, pos, creative);
                p.sent_sel = p.inv.selected;
            }
            Act::TeleportAck { id } => {
                let p = &mut self.players[i];
                if p.teleport == Some(id) {
                    p.teleport = None;
                }
            }
            Act::UseRelease => self.eat_cancel(i, None),
            Act::Attack { target } => self.attack_entity(i, target), // моб или игрок (server/pvp.rs)
            Act::VelocityAck { id } => self.velocity_ack(i, id),
            Act::UseEntity { target, hand } => self.use_entity(i, target, hand),
            Act::Debug(cmd) => {
                if self.debug {
                    self.debug_cmd(i, cmd);
                }
            }
        }
    }

    fn ack(&mut self, i: usize, seq: u32) {
        let a = &mut self.players[i].ack;
        *a = Some(a.map_or(seq, |s| if seq.wrapping_sub(s) as i32 > 0 { seq } else { s }));
    }

    /// Действие над миром от игрока i в режиме Auth (сервер решает): «не в себя» — и против тел других игроков.
    /// Ответ — итог действия и окно, которое оно просит открыть.
    pub(crate) fn auth<R>(&mut self, i: usize, f: impl FnOnce(&mut ActCtx) -> R) -> (R, Option<Menu>) {
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let actor = self.players[i].id;
        let bodies: Vec<Aabb> = self.players.iter().filter(|o| o.id != actor).map(|o| o.player.bbox()).collect();
        let p = &mut self.players[i];
        let mut c = ActCtx::new(&mut w, &p.player, &mut p.inv, &mut self.events, Mode::Auth { sim: &mut self.sim, actor });
        c.bodies = &bodies;
        let r = f(&mut c);
        (r, c.open.take())
    }

    /// Выброс из руки (ServerPlayerEntity.dropSelectedItem): вперёд по взгляду, с «бросившим», задержка 40; позиция и
    /// взгляд — на начало тика (фаза ввода, до движения).
    fn drop_selected(&mut self, i: usize, all: bool) {
        if protected(&self.sim.world.d.borrow(), crate::game::interact::feet(&self.players[i].player)) {
            return; // на Перекрёстке вещь остаётся у игрока (клиент предсказал так же)
        }
        let p = &mut self.players[i];
        let s = p.inv.take_selected(all);
        if s.is_empty() {
            return;
        }
        let pl = &p.player;
        let sim = &mut self.sim;
        sim.items.throw(pl.pos, pl.eye_height(), pl.yaw, pl.pitch, s, false, true, &mut sim.rng, &mut sim.ids);
    }

    fn debug_cmd(&mut self, i: usize, cmd: DebugCmd) {
        if let DebugCmd::Hurt { amount } = cmd {
            if amount.is_finite() {
                self.hurt(i, crate::game::health::DamageKind::Generic, amount);
            }
            return;
        }
        if let DebugCmd::SpawnMob { kind, pos, n } = cmd {
            return self.debug_spawn_mob(kind, pos, n);
        }
        let p = &mut self.players[i];
        match cmd {
            DebugCmd::Fly(on) => p.player.set_flying(on),
            // Полёт выживания идёт своим Act::Flight.

            DebugCmd::Give { item, count } => {
                if idef(item).kind != ItemKind::None {
                    give_into(&mut p.inv, item, count);
                }
            }
            DebugCmd::Kit => kit_into(&mut p.inv),
            DebugCmd::ClearInventory => {
                let sel = p.inv.selected;
                p.inv = Inventory::new();
                p.inv.selected = sel;
            }
            DebugCmd::Teleport { pos } => {
                if pos.iter().all(|v| v.is_finite()) {
                    p.player.teleport(pos);
                }
            }
            DebugCmd::Spawn { x, z } => {
                if x.is_finite() && z.is_finite() {
                    let pos = find_spawn(&mut self.sim.world.d.borrow_mut(), x.floor() as i32, z.floor() as i32);
                    place_at_spawn(&mut p.player, pos);
                }
            }
            DebugCmd::Time(f) => {
                self.sim.day = set_time_of_day(f);
                self.time_dirty = true;
            }
            DebugCmd::DayCycle(on) => {
                self.sim.day_cycle = on;
                self.time_dirty = true;
            }
            DebugCmd::SetHp { hp } => {
                if p.hp.alive() && hp.is_finite() && hp > 0.0 {
                    p.hp.hp = hp.min(p.hp.max);
                    p.hp.time_until_regen = 0;
                }
            }
            DebugCmd::Hurt { .. } => {}
            DebugCmd::TickSpeed(n) => self.sim.random_tick_speed = n.min(4096),
            DebugCmd::SpawnMob { .. } => {}
            DebugCmd::Build(kind) => {
                let feet = [p.player.pos[0].floor() as i32, p.player.pos[1].floor() as i32, p.player.pos[2].floor() as i32];
                let d = self.sim.world.d.clone();
                let mut w = d.borrow_mut();
                crate::game::mek_demo::build(&mut self.sim, &mut w, &mut self.events, &mut p.inv, feet, kind);
            }
        }
    }

    // ------------------------------------------------------------ окна

    /// 2½. Доступность открытого окна после движения игрока (ServerPlayerEntity.tick → canUse): верстак, сундук, печь на
    /// месте и ближе (4.5 + 4)² от глаз до коробки блока; иначе окно закрывается, вещи возвращаются.
    fn tick_screen(&mut self, i: usize) {
        // Окно блока держится, пока в клетке блок с тем же окном (таблица: on_use) и его блок-сущность, если она нужна.
        let Some((pos, want)) = self.players[i].screen.as_ref().and_then(|m| match m.kind {
            MenuKind::Crafting(p) => Some((p, OnUse::Crafting)),
            MenuKind::Chest(p) => Some((p, OnUse::Chest)),
            MenuKind::Furnace(p) => Some((p, OnUse::Furnace)),
            MenuKind::Mek(p, _) => Some((p, OnUse::Machine)),
            MenuKind::Player => None,
        }) else {
            return;
        };
        let ok = {
            let mut w = self.sim.world.d.borrow_mut();
            let def = bdef(w.get_block(pos[0], pos[1], pos[2]));
            def.on_use == want && (def.be.is_none() || w.be().get(pos).is_some())
        };
        if !crafting_table_in_reach(self.players[i].player.eye(1.0), pos, ok) {
            self.close_screen(i, false);
        }
    }

    /// Открыть окно (openHandledScreen): прежнее сначала закрывается (WinClose); окно блока — WinOpen с содержимым;
    /// сундук — смотрящих +1 (первый — звук открытия; число — тем, кто знает кусок, крышка у клиентов).
    pub(crate) fn open_screen(&mut self, i: usize, m: Menu) {
        if self.players[i].screen.is_some() {
            self.close_screen(i, false);
        }
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let kind = screen_code(&m.kind);
        let pos = m.block_pos();
        if let MenuKind::Chest(p) = m.kind {
            if let Some(c) = w.be_mut().chest_mut(p) {
                if c.on_open() {
                    let pitch = 0.9 + self.fx_rng.next_f32() * 0.1;
                    self.events.push(EV_CHEST_OPEN, p, (pitch * 1000.0) as i32);
                }
                self.chest_viewers.push((p, c.viewers));
            }
        }
        let pl = &mut self.players[i];
        if matches!(m.kind, MenuKind::Player) {
            pl.win = 0;
        } else {
            pl.next_win = if pl.next_win == 255 { 1 } else { pl.next_win + 1 };
            pl.win = pl.next_win;
            let slots: Vec<ItemStack> = pos.and_then(|p| w.be().get(p)).map_or(Vec::new(), |b| b.stacks().to_vec());
            pl.win_out.push(Msg::WinOpen { win: pl.win, kind: kind as u8, pos, rev: pl.win_rev, slots: slots.clone() });
            pl.win_sent = slots;
            pl.furnace_sent = None;
            pl.mek_sent.clear();
        }
        pl.screen = Some(m);
    }

    /// Закрыть окно: курсор и сетка — в инвентарь, без места — бросок вперёд (ASP-08 §4); сундук — смотрящих −1 (звук
    /// закрытия и крышка вниз). by_client — закрыл сам игрок (ему не сообщать).
    pub(crate) fn close_screen(&mut self, i: usize, by_client: bool) {
        let Some(kind) = self.players[i].screen.as_ref().map(|m| m.kind) else { return };
        self.with_screen(i, |m, u| m.close(u));
        let pl = &mut self.players[i];
        pl.screen = None;
        if !by_client {
            pl.win_out.push(Msg::WinClose { win: pl.win });
        }
        if let MenuKind::Chest(p) = kind {
            let mut w = self.sim.world.d.borrow_mut();
            if let Some(c) = w.be_mut().chest_mut(p) {
                if c.on_close() {
                    let pitch = 0.9 + self.fx_rng.next_f32() * 0.1;
                    self.events.push(EV_CHEST_CLOSE, p, (pitch * 1000.0) as i32);
                }
                self.chest_viewers.push((p, c.viewers));
            }
        }
    }

    /// Действие над открытым окном игрока i с хранилищами сервера; броски и звуки окна — сразу.
    pub(crate) fn with_screen<R>(&mut self, i: usize, f: impl FnOnce(&mut Menu, &mut Ui) -> R) -> Option<R> {
        let mut m = self.players[i].screen.take()?;
        let r = {
            let d = self.sim.world.d.clone();
            let mut w = d.borrow_mut();
            let locked = m.block_pos().is_some_and(|p| protected(&w, p));
            let block = m.block_pos().and_then(|p| w.be_mut().container(p));
            let book = &self.sim.book;
            let p = &mut self.players[i];
            let creative = crate::game::rules::creative(&p.player);
            let mut u = Ui { inv: &mut p.inv, block, book, ev: &mut p.ui_ev, alive: true, creative, locked };
            f(&mut m, &mut u)
        };
        self.players[i].screen = Some(m);
        self.flush_ui(i);
        Some(r)
    }

    /// Сверка окна после клика: хеш разошёлся — полная копия окна и инвентаря в кадр.
    fn check_win(&mut self, i: usize, hash: u32) {
        let ours = self.with_screen(i, |m, u| menu_hash(m, u)).unwrap_or(0);
        if ours != hash {
            let p = &mut self.players[i];
            p.win_force = true;
            p.inv_force = true;
            self.stats.win_mismatch += 1;
        } else if let Some(pos) = self.players[i].screen.as_ref().and_then(|m| m.block_pos()) {
            // Клиент уже видит то же, что сервер: эхо его собственной правки хранилища (WinSlots) ему не нужно — оно
            // пришло бы позже его следующих предсказаний и откатило бы их.
            let cur = self.sim.world.d.borrow().be().get(pos).map(|b| b.stacks().to_vec());
            if let Some(cur) = cur {
                self.players[i].win_sent = cur;
            }
        }
    }

    /// События окна: бросок вперёд по взгляду (задержка 40); надевание брони — звук другим.
    fn flush_ui(&mut self, i: usize) {
        let evs = std::mem::take(&mut self.players[i].ui_ev);
        for e in evs {
            match e {
                UiEv::Drop { stack, owned } => {
                    let pl = &self.players[i].player;
                    let sim = &mut self.sim;
                    sim.items.throw(pl.pos, pl.eye_height(), pl.yaw, pl.pitch, stack, false, owned, &mut sim.rng, &mut sim.ids);
                }
                UiEv::Equip(item) => {
                    let p = &self.players[i];
                    self.events.push_to(To::Except(p.id), EV_EQUIP, p.feet_cell(), item as i32);
                }
            }
        }
    }

    // ------------------------------------------------------------ подбор и кадры

    /// 4. Подбор (после сущностей), игроки в порядке входа: звук «pop» и подскок иконки — подобравшему (событие несёт
    /// ячейку, куда легло), летящая копия — Msg::Pickup подобравшему и всем, кто следит за предметом (flush).
    fn tick_pickup(&mut self) {
        for i in 0..self.players.len() {
            let mut picked = Vec::new();
            let p = &mut self.players[i];
            if !p.hp.alive() || crate::game::rules::spectator(&p.player) {
                continue; // мёртвый и наблюдатель ничего не подбирают (свои выпавшие вещи — тоже)
            }
            let bb = p.player.bbox();
            self.sim.items.pickup(&bb, &mut p.inv, &mut picked);
            for k in picked {
                let c = [k.pos[0].floor() as i32, k.pos[1].floor() as i32, k.pos[2].floor() as i32];
                self.events.push_to(To::Only(p.id), EV_PICKUP, c, k.item as i32 | (k.slot as i32) << 16);
                let msg = Msg::Pickup { id: k.id, by: p.id, stack: k.stack, from: k.pos, spin: k.spin };
                self.pickups.push(PickupFx { item: k.id, by: p.id, msg });
            }
        }
    }

    /// Кадры (§2.3, фаза 5): каждому игроку — правки известных ему кусков (новые куски радиуса — целиком, ушедшие за
    /// радиус — Forget), решённые предсказания, сущности и игроки в радиусе слежения, трещины и взмахи других, окна,
    /// инвентарь, события в радиусе слышимости, время, пинг. Конец тика; локальный сервер зовёт и между тиками
    /// (проверки ставят мир напрямую).
    pub(crate) fn flush(&mut self) {
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        // Правки — по кускам, клетка один раз (последняя запись).
        let mut by_chunk: BTreeMap<(i32, i32), Vec<(u16, u32)>> = BTreeMap::new();
        let mut seen: HashMap<[i32; 3], (i32, i32, usize)> = HashMap::new();
        for (p, c) in w.take_log() {
            let ch = chunk_of(p[0], p[2]);
            match seen.get(&p) {
                Some(&(x, z, k)) => by_chunk.get_mut(&(x, z)).unwrap()[k].1 = c,
                None => {
                    let list = by_chunk.entry(ch).or_default();
                    seen.insert(p, (ch.0, ch.1, list.len()));
                    list.push((local_key(p[0].rem_euclid(CS), p[1], p[2].rem_euclid(CS)), c));
                }
            }
        }
        let mut bcast = std::mem::take(&mut self.bcast);
        if self.ping && self.ticks % LIST_PING_EVERY == 0 {
            let list: Vec<(u32, u16)> = self.players.iter().filter_map(|p| Some((p.id, p.ping_avg?.min(u16::MAX as u32) as u16))).collect();
            if !list.is_empty() {
                bcast.push(Msg::ListPing(list));
            }
        }
        let pickups = std::mem::take(&mut self.pickups);
        let anims = std::mem::take(&mut self.sim.mobs.anims);
        let viewers = std::mem::take(&mut self.chest_viewers);
        let stages = std::mem::take(&mut self.stages);
        let events = self.events.drain();
        // Игроки — каким их видят другие (до правки каждого получателя).
        // Наблюдатель (G17) другим не виден: его нет в слежении (Despawn, если был).
        let others: Vec<(PlayerMove, String, u8, bool)> =
            self.players.iter().filter(|p| !crate::game::rules::spectator(&p.player)).map(|p| (p.moved(), p.nick.clone(), p.skin, p.swung)).collect();
        let levels: Vec<u8> = (0..self.players.len()).map(|i| self.level_of(i)).collect();
        let s = &self.sim;
        let tick = s.time;
        let time = (self.time_dirty || tick % 20 == 0).then_some(Msg::Time { time: s.time, day: s.day, cycle: s.day_cycle });
        self.time_dirty = false;
        let ping = self.ping && tick % PING_EVERY == 0;
        let budget = self.edit_budget;
        let glow = w.grid.glow_list();
        // Предметы в пути: изменившиеся за тик (появились, новый путь) и закончившиеся — всем, кто знает их кусок.
        let t_changed: HashSet<u32> = std::mem::take(&mut w.logistics.changed).into_iter().collect();
        let t_ended = std::mem::take(&mut w.logistics.ended);
        for (pi, p) in self.players.iter_mut().enumerate() {
            let mut msgs = Vec::new();
            // Режим и права (G17): при входе и изменении (/op, правка roles.txt; /gamemode шлёт сам вместе с Teleport).
            let mode = (p.player.mode.code(), levels[pi]);
            if p.sent_mode != Some(mode) {
                p.sent_mode = Some(mode);
                msgs.push(Msg::Mode { mode: mode.0, level: mode.1 });
            }
            know_chunks(p, &w, &mut msgs);
            send_edits(p, &w, budget, &mut msgs);
            p.edit_used = 0; // кадр ушёл: следующий вход или смена куска считают бюджет заново
            let r = p.view;
            for (&ch, list) in &by_chunk {
                // Кусок ждёт своей очереди (ChunkEdits ещё не ушёл) — клиент получит его уже с этими правками.
                if (ch.0 - p.center.0).abs() <= r && (ch.1 - p.center.1).abs() <= r && !p.edit_queue.contains(&ch) {
                    p.known.insert(ch);
                    msgs.push(Msg::Blocks { cx: ch.0, cz: ch.1, list: list.clone() });
                }
            }
            if let Some(seq) = p.ack.take() {
                msgs.push(Msg::Ack { seq });
            }
            msgs.extend(bcast.iter().cloned());
            for &(pos, n) in &viewers {
                if p.known.contains(&chunk_of(pos[0], pos[2])) || near_cell(p, pos, EVENT_R) {
                    msgs.push(Msg::ChestViewers { pos, n });
                }
            }
            track_entities(p, s, &others, &pickups, &anims, &mut msgs);
            for &(who, pos, stage) in &stages {
                if who != p.id && near_cell(p, pos, STAGE_R) {
                    msgs.push(Msg::BreakStage { who, pos, stage });
                }
            }
            msgs.append(&mut p.own);
            let vit = vitals_of(p);
            if p.sent_vit != Some(vit) {
                p.sent_vit = Some(vit);
                msgs.push(Msg::Vitals(vit));
            }
            sync_window(p, &w, &mut msgs);
            sync_sides(p, &w, &mut msgs);
            sync_transit(p, &w, &t_changed, &t_ended, &mut msgs);
            if p.glow_sent != glow {
                p.glow_sent = glow.clone();
                msgs.push(Msg::NetGlow(glow.clone()));
            }
            let inv = p.inv_copy();
            let sel = (p.inv.selected != p.sent_sel).then_some(p.inv.selected as u8);
            if p.inv_force || inv != p.sent_inv || sel.is_some() {
                p.sent_inv = inv.clone();
                p.sent_sel = p.inv.selected;
                p.inv_force = false;
                msgs.push(Msg::Inventory { slots: inv, selected: sel });
            }
            // Подтверждение кликов по окну: при изменении номера и вместе с любой копией состояния (по нему клиент
            // знает, какие свои клики сервер в ней уже учёл).
            if p.screen.is_some() {
                let state = msgs.iter().any(|m| matches!(m, Msg::Inventory { .. } | Msg::WinSlots { .. }));
                if state || p.win_rev != p.win_rev_sent {
                    p.win_rev_sent = p.win_rev;
                    msgs.push(Msg::WinAck { win: p.win, rev: p.win_rev });
                }
            }
            for (to, e) in &events {
                let heard = match to {
                    To::Only(_) => true,
                    _ => near_cell(p, [e[1], e[2], e[3]], EVENT_R),
                };
                if heard && to.reaches(p.id) {
                    msgs.push(Msg::Fx { code: e[0] as u16, pos: [e[1], e[2], e[3]], arg: e[4] });
                }
            }
            if let Some(t) = &time {
                msgs.push(t.clone());
            }
            if ping && p.ping.is_none() {
                let id = tick as u32;
                p.ping = Some((id, self.now_ms));
                msgs.push(Msg::Ping { id });
            }
            p.outbox.push(S2C::Bundle(Bundle { tick, msgs }));
        }
        for p in &mut self.players {
            p.swung = false;
        }
    }

    /// Отладка и проверки: записать ячейку по правилам игры (обновления соседей, тики, блок-сущности, опад). false —
    /// вне высоты или то же.
    pub(crate) fn world_edit(&mut self, pos: [i32; 3], cell: Cell) -> bool {
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        self.sim.set_world(&mut w, pos, cell, &mut self.events)
    }

    /// Отладка: [запланированных тиков, падающих блоков, блок-сущностей].
    pub(crate) fn world_stats(&self) -> Vec<u32> {
        vec![self.sim.ticks.len() as u32, self.sim.falling.len() as u32, self.sim.world.d.borrow().be().len() as u32]
    }
    /// Предметов в мире.
    pub(crate) fn item_count(&self) -> u32 {
        self.sim.items.list.iter().filter(|e| !e.removed).count() as u32
    }
}

/// Клетка pos ближе r блоков к ступням игрока.
fn near_cell(p: &ServerPlayer, pos: [i32; 3], r: f64) -> bool {
    let q = p.player.pos;
    (0..3).map(|k| (pos[k] as f64 + 0.5 - q[k]).powi(2)).sum::<f64>() <= r * r
}

/// Радиус правок (§3.5): игрок перешёл в другой кусок — куски с правками, вошедшие в радиус, ставятся в очередь (EditIndex
/// сразу — клиент знает, чего ждать; сами ChunkEdits уходят в send_edits под бюджет), куски, ушедшие за радиус с
/// запасом, — Forget (клиент забывает их правки). Очередь пересортировывается: ближние к новому месту — первыми.
fn know_chunks(p: &mut ServerPlayer, w: &WorldData, msgs: &mut Vec<Msg>) {
    let c = p.chunk();
    if c == p.center {
        return;
    }
    p.center = c;
    let r = p.view;
    let mut gone: Vec<(i32, i32)> = p.known.iter().copied().filter(|k| (k.0 - c.0).abs() > r + FORGET_MARGIN || (k.1 - c.1).abs() > r + FORGET_MARGIN).collect();
    gone.sort_unstable();
    for k in gone {
        p.known.remove(&k);
        p.edit_queue.retain(|q| *q != k);
        msgs.push(Msg::Forget { cx: k.0, cz: k.1 });
    }
    let new: Vec<(i32, i32)> = w.edited_chunks().filter(|k| (k.0 - c.0).abs() <= r && (k.1 - c.1).abs() <= r && !p.known.contains(k)).collect();
    if !new.is_empty() {
        msgs.push(Msg::EditIndex(new.iter().map(|&k| (k.0, k.1, w.chunk_edits(k.0, k.1).len() as u32)).collect()));
        p.known.extend(new.iter().copied());
        p.edit_queue.extend(new);
    }
    let mut q: Vec<(i32, i32)> = p.edit_queue.drain(..).collect();
    q.sort_by_key(|k| ((k.0 - c.0).pow(2) + (k.1 - c.1).pow(2), *k));
    p.edit_queue = q.into();
}

/// Правки кусков из очереди игрока — до бюджета байт на кадр (design.md §3.5: 32 КБ на тик): остаток уходит в следующих
/// тиках. Кусок целиком, по порядку очереди; первый кусок кадра идёт всегда (кусок больше бюджета не должен встать). Размер
/// — по кодированию сообщения; edit_used хранит уже отправленное с прошлого кадра (вход и кадр тика делят бюджет).
fn send_edits(p: &mut ServerPlayer, w: &WorldData, budget: usize, msgs: &mut Vec<Msg>) {
    while let Some(&(cx, cz)) = p.edit_queue.front() {
        let m = Msg::ChunkEdits { cx, cz, edits: delta_keys(&w.chunk_edits(cx, cz)) };
        let size = if budget == usize::MAX { 0 } else { encode(&m).len() };
        if p.edit_used > 0 && p.edit_used + size > budget {
            break;
        }
        p.edit_used += size.max(1);
        p.edit_queue.pop_front();
        msgs.push(m);
    }
}

/// Сущности для игрока: новые в радиусе — Spawn, сдвинувшиеся — Moves (игроки — PlayerMoves), взмах — Anim,
/// изменилось число — ItemCount; подобранные — Pickup; пропавшие и ушедшие из радиуса — Despawn.
fn track_entities(p: &mut ServerPlayer, s: &Sim, others: &[(PlayerMove, String, u8, bool)], pickups: &[PickupFx], mob_anims: &[(u32, u8)], msgs: &mut Vec<Msg>) {
    let me = p.player.pos;
    let near = |q: [f64; 3], r: f64| (0..3).map(|k| (q[k] - me[k]).powi(2)).sum::<f64>() <= r * r;
    let mut alive: HashSet<u32> = HashSet::new();
    let (mut moves, mut counts, mut pmoves, mut anims) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for e in s.items.list.iter().filter(|e| !e.removed && near(e.pos, TRACK_ITEMS)) {
        alive.insert(e.id);
        match p.tracked.get_mut(&e.id) {
            None => {
                let ent = Ent::Item { stack: e.stack, pos: e.pos, prev: e.prev, vel: e.vel, spin: e.spin_age, offset: e.offset };
                msgs.push(Msg::Spawn { id: e.id, ent });
                p.tracked.insert(e.id, Tracked { pos: e.pos, spin: e.spin_age, count: e.stack.count });
            }
            Some(t) => {
                if t.pos != e.pos || t.spin != e.spin_age {
                    moves.push(EntMove { id: e.id, pos: e.pos, spin: e.spin_age });
                    (t.pos, t.spin) = (e.pos, e.spin_age);
                }
                if t.count != e.stack.count {
                    counts.push(Msg::ItemCount { id: e.id, count: e.stack.count.clamp(0, 255) as u8 });
                    t.count = e.stack.count;
                }
            }
        }
    }
    for f in s.falling.iter().filter(|f| !f.removed && near(f.pos, TRACK_FALLING)) {
        alive.insert(f.id);
        match p.tracked.get_mut(&f.id) {
            None => {
                msgs.push(Msg::Spawn { id: f.id, ent: Ent::Falling { cell: f.cell, pos: f.pos, prev: f.prev, vel: f.vel } });
                p.tracked.insert(f.id, Tracked { pos: f.pos, spin: 0, count: 1 });
            }
            Some(t) => {
                if t.pos != f.pos {
                    moves.push(EntMove { id: f.id, pos: f.pos, spin: 0 });
                    t.pos = f.pos;
                }
            }
        }
    }
    for (mv, nick, skin, swung) in others {
        if mv.id == p.id || !near(mv.pos.map(|v| v as f64), TRACK_PLAYERS) {
            continue;
        }
        alive.insert(mv.id);
        let pos = mv.pos.map(|v| v as f64);
        match p.tracked.get_mut(&mv.id) {
            None => {
                msgs.push(Msg::Spawn { id: mv.id, ent: Ent::Player { nick: nick.clone(), skin: *skin, mv: *mv } });
                p.tracked.insert(mv.id, Tracked { pos, spin: mv.n, count: 1 });
            }
            Some(t) => {
                if t.spin != mv.n {
                    pmoves.push(*mv);
                    (t.pos, t.spin) = (pos, mv.n);
                }
            }
        }
        if *swung {
            anims.push(Msg::Anim { id: mv.id, kind: 0 });
        }
    }
    if !moves.is_empty() {
        msgs.push(Msg::Moves(moves));
    }
    if !pmoves.is_empty() {
        msgs.push(Msg::PlayerMoves(pmoves));
    }
    msgs.extend(anims);
    msgs.extend(counts);
    mobs::track_mobs(p, s, mob_anims, &mut alive, msgs);
    // Подбор: летящую копию получают подобравший и все, кто видел предмет (он ещё в их слежении — Despawn ниже).
    for pk in pickups {
        if pk.by == p.id || p.tracked.contains_key(&pk.item) {
            msgs.push(pk.msg.clone());
        }
    }
    let mut gone: Vec<u32> = p.tracked.keys().copied().filter(|id| !alive.contains(id)).collect();
    if !gone.is_empty() {
        gone.sort_unstable();
        for id in &gone {
            p.tracked.remove(id);
        }
        msgs.push(Msg::Despawn(gone));
    }
}

/// Окно игрока: открытия и закрытия по порядку, изменившиеся ячейки хранилища блока, числа печи; после расхождения
/// хеша — всё окно (курсор, сетка, результат, хранилище).
/// Стороны машин в радиусе SIDES_R, пока в руке конфигуратор (подсветка граней, MEK-09 §7.2): только изменившиеся.
/// Предметы в пути (M5): стек в известном клиенту куске — состояние, если клиент его не знает или у стека новое
/// событие; ушёл из известных кусков, доставлен или выпал — конец.
fn sync_transit(p: &mut ServerPlayer, w: &WorldData, changed: &HashSet<u32>, ended: &[u32], msgs: &mut Vec<Msg>) {
    let mut add = Vec::new();
    let mut gone: Vec<u32> = ended.iter().copied().filter(|id| p.transit_known.remove(id)).collect();
    for t in &w.logistics.stacks {
        if !t.ready {
            continue;
        }
        let at = t.at();
        let seen = p.known.contains(&chunk_of(at[0], at[2]));
        let known = p.transit_known.contains(&t.id);
        if seen && (!known || changed.contains(&t.id)) {
            p.transit_known.insert(t.id);
            add.push(crate::world::logistics::TransitMsg::of(t));
        } else if !seen && known {
            p.transit_known.remove(&t.id);
            gone.push(t.id);
        }
    }
    if !gone.is_empty() {
        msgs.push(Msg::TransitEnd(gone));
    }
    if !add.is_empty() {
        msgs.push(Msg::Transit(add));
    }
}

fn sync_sides(p: &mut ServerPlayer, w: &WorldData, msgs: &mut Vec<Msg>) {
    const SIDES_R: f64 = 12.0;
    if p.inv.hand().id() != crate::registry::items::I_CONFIGURATOR {
        return;
    }
    let mut list = Vec::new();
    for (q, _, be) in w.be().iter() {
        let BlockEntity::Mek(m) = be else { continue };
        if crate::world::mek::sides::item_modes(m.kind).is_empty() || !near_cell(p, q, SIDES_R) {
            continue;
        }
        let v = crate::world::mek::sides::pack_sides(m);
        if p.sides_sent.get(&q) != Some(&v) {
            p.sides_sent.insert(q, v);
            list.push((q, v));
        }
    }
    if !list.is_empty() {
        msgs.push(Msg::MekSides(list));
    }
}

fn sync_window(p: &mut ServerPlayer, w: &WorldData, msgs: &mut Vec<Msg>) {
    msgs.append(&mut p.win_out);
    let force = std::mem::take(&mut p.win_force);
    let Some(m) = &p.screen else { return };
    let mut list: Vec<(i16, ItemStack)> = Vec::new();
    let be = m.block_pos().and_then(|pos| w.be().get(pos));
    let cur: Vec<ItemStack> = be.map_or(Vec::new(), |b| b.stacks().to_vec());
    if force {
        list.push((-1, m.cursor));
    }
    for (k, def) in m.slots.iter().enumerate() {
        let s = match def.store {
            Store::Block(j) => {
                let s = cur.get(j as usize).copied().unwrap_or_default();
                if !force && p.win_sent.get(j as usize) == Some(&s) {
                    continue;
                }
                s
            }
            Store::Grid(j) if force => m.grid.cells[j as usize],
            Store::Result if force => m.result,
            _ => continue,
        };
        list.push((k as i16, s));
    }
    p.win_sent = cur;
    if !list.is_empty() {
        msgs.push(Msg::WinSlots { win: p.win, rev: p.win_rev, list });
    }
    if let Some(BlockEntity::Furnace(f)) = be {
        let v = [f.burn, f.fuel_total, f.cook, f.cook_total];
        if force || p.furnace_sent != Some(v) {
            p.furnace_sent = Some(v);
            msgs.push(Msg::WinData { win: p.win, values: v.to_vec() });
        }
    }
    if let Some(BlockEntity::Mek(m)) = be {
        let v = m.view();
        if force || p.mek_sent != v {
            p.mek_sent = v.clone();
            msgs.push(Msg::WinData { win: p.win, values: v });
        }
    }
}

/// Итог теневого тика совпал с клиентским (биты позиций и скоростей, флаги).
fn same_move(p: &Player, mv: &Move) -> bool {
    let bits = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| a[k].to_bits() == b[k].to_bits());
    let flags = [
        (p.on_ground, MV_GROUND),
        (p.sprinting, MV_SPRINT),
        (p.crouch, MV_CROUCH),
        (p.flying, MV_FLY),
        (p.sneak_in, MV_SNEAK),
        (p.h_coll, MV_HCOLL),
        (p.soft, MV_SOFT),
        (p.noclip, MV_NOCLIP),
    ];
    bits(p.pos, mv.pos)
        && bits(p.vel, mv.vel)
        && p.fall.to_bits() == mv.fall.to_bits()
        && flags.iter().all(|&(on, f)| on == (mv.flags & f != 0))
        && p.jump_cd.clamp(0, 255) as u8 == mv.jump_cd
        && p.dtap.clamp(0, 255) as u8 == mv.dtap
        && p.fwd_in.to_bits() == mv.fwd_in.to_bits()
        && p.support == mv.support
}

/// Принять итог клиента (правдоподобный — см. ServerSim::shadow).
pub(crate) fn adopt_move(p: &mut Player, mv: &Move) {
    p.pos = mv.pos;
    p.vel = mv.vel;
    p.fall = mv.fall;
    p.on_ground = mv.flags & MV_GROUND != 0;
    p.sprinting = mv.flags & MV_SPRINT != 0;
    p.crouch = mv.flags & MV_CROUCH != 0;
    p.flying = mv.flags & MV_FLY != 0;
    p.sneak_in = mv.flags & MV_SNEAK != 0;
    p.h_coll = mv.flags & MV_HCOLL != 0;
    p.soft = mv.flags & MV_SOFT != 0;
    p.noclip = mv.flags & MV_NOCLIP != 0;
    p.jump_cd = mv.jump_cd as i32;
    p.dtap = mv.dtap as i32;
    p.fwd_in = mv.fwd_in;
    p.support = mv.support;
}

/// Сервер с одним соединением-петлёй (браузер без сети, проверки): Hello → вход, дальше — сообщения игрока; кадры —
/// байтами.
pub(crate) struct LocalServer {
    pub(crate) sim: ServerSim,
    /// Номер вошедшего игрока.
    pub(crate) pid: Option<u32>,
    out: Vec<Vec<u8>>,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
}

impl LocalServer {
    pub(crate) fn new(sim: ServerSim) -> LocalServer {
        LocalServer { sim, pid: None, out: Vec::new(), bytes_in: 0, bytes_out: 0 }
    }
    pub(crate) fn recv(&mut self, b: &[u8]) -> Result<(), ProtoError> {
        self.bytes_in += b.len() as u64;
        let m: C2S = decode(b)?;
        match (self.pid, m) {
            (None, C2S::Hello(h)) => match self.sim.join(&h) {
                Ok(id) => self.pid = Some(id),
                Err((code, text)) => self.out.push(encode(&S2C::Reject { code, text })),
            },
            (Some(id), m) => self.sim.handle(id, m),
            (None, _) => {} // до входа — ничего
        }
        Ok(())
    }
    pub(crate) fn tick(&mut self) {
        self.sim.tick();
        // Петля в браузере: выкинуть некого (одно соединение), журнала и файла ролей нет.
        self.sim.admin.kicks.clear();
        self.sim.admin.log.clear();
        self.sim.admin.roles_dirty = false;
        self.sim.admin.save_req = false;
    }
    pub(crate) fn flush(&mut self) {
        self.sim.flush();
    }
    pub(crate) fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        let mut v = std::mem::take(&mut self.out);
        if let Some(p) = self.pid.and_then(|id| self.sim.player_mut(id)) {
            v.extend(p.outbox.drain(..).map(|m| encode(&m)));
        }
        self.bytes_out += v.iter().map(|b| b.len() as u64).sum::<u64>();
        v
    }
}
