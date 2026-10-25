//! Клиент симуляции (MP-0, design.md §2.2, P1-5): свой игрок считается как раньше — ввод фазы тика, машина ломания,
//! ставление и использование (предсказание: interact.rs в режиме Predict над зеркалом мира), движение той же физикой,
//! рука и HUD, данные кадра для рендера. Мир, сущности, инвентарь, окна и время — зеркало того, что пришлёт сервер
//! (proto S2C); своё предсказание инвентаря и окон сервер подтверждает полной копией (MP-5 — ревизии).
//!
//! Действия копятся очередью до границы тика (ревью P2 «Сеть»): клики окна, колесо и отладка — между тиками (в кадре,
//! предсказание сразу), фаза ввода — в тике; всё по порядку уходит одним C2S::Tick вместе с итогом движения (Move).
//! Сервер повторяет их в том же порядке. Выбранная ячейка хотбара шлётся (Act::Hotbar) перед каждым действием, которому
//! она важна, и в конце тика.
//!
//! Порядок фаз тика клиента — прежний (ASP-05 §11.1): рука до ввода → ввод и использование → рука → игрок → рука после
//! игрока → время; затем (Game::tick) ответ сервера на этот тик и «клиентская» фаза tick_visual (крышки, выборка
//! вокруг игрока). Отладка (V, K, J) — команды Debug: клиент выполняет сразу, сервер — в своём порядке (только если
//! сервер её разрешил: Welcome.debug).
//!
//! Другие игроки (MP-1, design.md §3.6): ходы с номером тика отправителя копятся, рисуются с отставанием ~2 тика по
//! курсору, который идёт по тику своего клиента и подстраивается (быстрее, если отстали больше 3 тиков, медленнее —
//! ближе 1.5), без экстраполяции. До первого тика клиент ждёт правки своего куска и соседей (EditIndex → ChunkEdits).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::abi::{Model, ScreenKind, SlotStore, SlotView, MSG_CROSSROADS, MSG_CROSSROADS_ENTER, MSG_CROSSROADS_LEAVE, MSG_HEIGHT, MSG_SKY_DAY, MSG_SKY_NIGHT, MSG_WANDER, NPRESS};
use crate::gen::spawn::{SKY_DAY, SKY_NIGHT};
use crate::abi::{EV_CRUMB, EV_EQUIP, EV_FURNACE_CRACKLE, EV_FURNACE_FX, EV_HIT, EV_LANDED, EV_REEQUIP, EV_SCREEN, EV_SWING};
use crate::abi::{Ev, Sfx};
use crate::abi::{K_ATTACK, K_BACK, K_FWD, K_JUMP, K_LEFT, K_LOCKED, K_RIGHT, K_SNEAK, K_SPRINT, K_USE};
use crate::abi::{P_ATTACK, P_DROP, P_DROP_ALL, P_FLIGHT, P_FLY, P_HOTBAR, P_INVENTORY, P_MODE, P_MODE_BACK, P_PICK, P_USE};
use crate::cell::{block_of, state_of, Cell, CS, DIRS, H};
use crate::game::bucket::use_bucket;
use crate::game::equip::equip_from_hand;
use crate::game::crafting::RecipeBook;
use crate::game::entity::{FallingBlock, ItemEntity, Items, PickupAnim};
use crate::game::interact::{
    break_block, feet, interact_block, item_pick, mine_delta, protected, raycast, raycast_ex, tool_key, ActCtx, Hit, Mining, Mode, Pending, Use, REACH,
    REACH_CREATIVE,
};
use crate::game::health::{death_fov_div, death_tilt_deg, hurt_tilt_deg, Vitals, MAX_HP};
use crate::game::inventory::{Inventory, OFFHAND};
use crate::game::player::{Keys, Player};
use crate::game::server::{adopt_move, VIEW_DEFAULT};
use crate::game::screen::{Menu, MenuKind, SlotKind, Store, Ui, UiEv};
use crate::game::sim::{set_time_of_day, time_of_day, DAY_TICKS};
use crate::game::social::clean_chat;
use crate::game::mek_client::mek_slot_view;
use crate::game::view::View;
use crate::game::{find_spawn, give_into, kit_into, menu_hash, screen_code, GameRng, TPS};
use crate::gen::probe;
use crate::proto::*;
use crate::registry::blocks::{bdef, facing_of, model_of, outline_at, snow_layers, Display, ST_LIT};
use crate::registry::ids::AIR;
use crate::registry::items::{equip_slot, food_nutrition, idef, ItemId, ItemKind};
use crate::game::sound::{Body, Steps};
use crate::world::stack::ItemStack;
use crate::world::{World, WorldData, CHUNK_CELLS};

/// Поедание в руке (ASP-12 §9): тиков осталось (32 → 0), рука, ячейка и предмет, с которыми начали, номер UseAir.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Eating {
    pub(crate) left: i32,
    pub(crate) hand: usize,
    pub(crate) slot: usize,
    pub(crate) item: ItemId,
    pub(crate) seq: u32,
}

/// Время еды, тиков: (int)(1.6 с · 20).
pub(crate) const EAT_TICKS: i32 = 32;

/// Игрок и управление: тело, инвентарь (предсказанный), ломание, ввод, прицел, открытое окно.
pub(crate) struct Ctl {
    pub(crate) player: Player,
    pub(crate) inv: Inventory,
    pub(crate) mine: Mining,
    /// Удержания (маска K_*), снимок для тика.
    pub(crate) keys: u32,
    /// Нажатия (P_*) с прошлого тика.
    pub(crate) presses: [u16; NPRESS],
    pub(crate) target: Option<Hit>,
    /// itemUseCooldown: 4 после попытки ПКМ.
    pub(crate) use_cd: i32,
    /// attackCooldown: 10 после удара в воздух, 10000 после захвата мыши.
    pub(crate) attack_cd: i32,
    /// Открытое окно (screen.rs, ASP-08).
    pub(crate) screen: Option<Menu>,
    pub(crate) ui_ev: Vec<UiEv>,
    /// Смещать обводку цветов, как меш (когда шейдер блоков умеет смещение растений) — луч и рамка.
    pub(crate) plant_offset: bool,
}

impl Ctl {
    pub(crate) fn new() -> Ctl {
        Ctl {
            player: Player::new([8.5, 100.0, 8.5]),
            inv: Inventory::new(),
            mine: Mining::new(),
            keys: 0,
            presses: [0; NPRESS],
            target: None,
            use_cd: 0,
            attack_cd: 0,
            screen: None,
            ui_ev: Vec::new(),
            plant_offset: false,
        }
    }
    pub(crate) fn held(&self, k: u32) -> bool {
        self.keys & k != 0
    }
    /// Съесть одно нажатие (KeyBinding.wasPressed).
    pub(crate) fn take(&mut self, code: u32) -> bool {
        let c = &mut self.presses[code as usize];
        if *c > 0 {
            *c -= 1;
            true
        } else {
            false
        }
    }
}

/// Счётчики сети клиента (проверки MP-0: в одной петле без задержки — нули).
#[derive(Default, Clone, Debug)]
pub(crate) struct ClientStats {
    /// Предсказанная клетка после Ack оказалась другой (блок «вернулся»).
    pub(crate) rollbacks: u32,
    /// Сервер переставил игрока (Teleport).
    pub(crate) teleports: u32,
    /// Отказ во входе или выброс: текст для игрока.
    pub(crate) rejected: Option<String>,
    /// Код отказа во входе (Reject): по нему страница решает, что делать (Outdated — перезагрузить).
    pub(crate) reject: Option<Reject>,
    pub(crate) frames_in: u32,
    /// Сервер вернул игрока после ухода (Resume) — состояние сервера принято.
    pub(crate) resumes: u32,
}

/// Отставание отрисовки других игроков от их последнего хода, тиков (§3.6), и пределы подстройки.
pub(crate) const REMOTE_DELAY: f64 = 2.0;
const REMOTE_LOW: f64 = 1.5;
const REMOTE_HIGH: f64 = 3.0;
/// Дальше этого (блоков за ход) — не тянуть, а переставить (телепорт).
const REMOTE_SNAP: f64 = 8.0;

/// Строк чата в памяти клиента (mp/ui.md §4).
const CHAT_KEEP: usize = 100;
/// Сколько неподтверждённых кликов по окну помнить (клик — 20 Гц × задержка, с запасом на очереди).
const WIN_PENDING_MAX: usize = 256;

/// Предсказанный клик по окну, ждущий подтверждения (`WinAck`).
#[derive(Clone, Copy, Debug)]
enum WinClick {
    Click { slot: i32, button: i32, action: i32 },
    QuickAll { slot: i32, item: ItemId, damage: u16 },
}

/// Строка чата: вид (`proto::CHAT_*`) и готовый текст.
#[derive(Clone, Debug)]
pub(crate) struct ChatLine {
    pub(crate) kind: u8,
    pub(crate) text: String,
}

/// Другой игрок у клиента: ходы по номеру тика отправителя, курсор отрисовки, взмах, шаг ног.
pub(crate) struct Remote {
    pub(crate) nick: String,
    pub(crate) skin: u8,
    samples: VecDeque<PlayerMove>,
    /// Курсор в тиках отправителя: на прошлом тике клиента и на этом (между ними — доля кадра alpha).
    pub(crate) t: f64,
    t_prev: f64,
    swing: i32,
    swing_prev: i32,
    limb: f32,
    limb_prev: f32,
    amt: f32,
    amt_prev: f32,
    /// Ритм шагов (звук, game/sound.rs) и ход за этот тик: от, до, PM_* — забирает tick_visual.
    steps: Steps,
    mv: Option<([f64; 3], [f64; 3], u8)>,
    /// Поворот тела, градусы (LivingEntity.turnHead): тянется за ходьбой, голова — за взглядом не дальше 75°.
    body: f32,
    body_prev: f32,
    /// Возраст в тиках клиента — качание рук в покое.
    age: u32,
}

use crate::game::third::wrap_deg;

impl Remote {
    fn new(nick: String, skin: u8, mv: PlayerMove) -> Remote {
        let t = mv.n as f64;
        let mut samples = VecDeque::new();
        samples.push_back(mv);
        let yaw = mv.yaw;
        Remote { nick, skin, samples, t, t_prev: t, swing: -1, swing_prev: -1, limb: 0.0, limb_prev: 0.0, amt: 0.0, amt_prev: 0.0, body: yaw, body_prev: yaw, age: 0, steps: Steps::new(), mv: None }
    }
    fn latest(&self) -> f64 {
        self.samples.back().map_or(0.0, |m| m.n as f64)
    }
    /// Новый ход (старые и повторы — мимо); прыжок дальше REMOTE_SNAP — переставить сразу.
    fn push(&mut self, mv: PlayerMove) {
        let Some(last) = self.samples.back() else { return };
        if mv.n <= last.n {
            return;
        }
        let d2: f32 = (0..3).map(|k| (mv.pos[k] - last.pos[k]).powi(2)).sum();
        if d2 as f64 > REMOTE_SNAP * REMOTE_SNAP {
            self.samples.clear();
            self.t = mv.n as f64;
            self.t_prev = self.t;
        }
        self.samples.push_back(mv);
    }
    /// Тело за тик (LivingEntity.tickMovement + turnHead, 1.21.1): идёт — тело к направлению хода (спиной вперёд, если
    /// взгляд почти назад), машет рукой — к взгляду; тянется на 0.3 от разницы за тик; голова от тела не дальше 75°, а
    /// дальше 50° тело доворачивается к голове на 0.2 от угла.
    fn turn_body(&mut self, a: &[f64; 3], b: &[f64; 3]) {
        let yaw = self.at(self.t).1;
        let (dx, dz) = ((b[0] - a[0]) as f32, (b[2] - a[2]) as f32);
        self.body_prev = self.body;
        self.body = crate::game::third::turn_body(self.body, yaw, dx, dz, self.swing >= 0);
    }
    /// Поворот тела на доле тика alpha: по короткой дуге между прошлым и этим тиком.
    fn body_at(&self, alpha: f32) -> f32 {
        self.body_prev + wrap_deg(self.body - self.body_prev) * alpha
    }
    /// Состояние в момент t (тики отправителя): линейно между ходами, yaw — по короткой дуге; вне — крайний ход.
    pub(crate) fn at(&self, t: f64) -> ([f64; 3], f32, f32, u8) {
        let s = &self.samples;
        let one = |m: &PlayerMove| (m.pos.map(|v| v as f64), m.yaw, m.pitch, m.flags);
        let (Some(first), Some(last)) = (s.front(), s.back()) else { return ([0.0; 3], 0.0, 0.0, 0) };
        if t <= first.n as f64 {
            return one(first);
        }
        if t >= last.n as f64 {
            return one(last);
        }
        let k = s.iter().position(|m| m.n as f64 > t).unwrap_or(s.len() - 1);
        let (a, b) = (&s[k - 1], &s[k]);
        let f = (t - a.n as f64) / (b.n - a.n) as f64;
        let pos = std::array::from_fn(|i| a.pos[i] as f64 + (b.pos[i] as f64 - a.pos[i] as f64) * f);
        let mut dy = (b.yaw - a.yaw) % 360.0;
        if dy > 180.0 {
            dy -= 360.0;
        } else if dy < -180.0 {
            dy += 360.0;
        }
        (pos, a.yaw + dy * f as f32, a.pitch + (b.pitch - a.pitch) * f as f32, if f < 0.5 { a.flags } else { b.flags })
    }
    /// Тик клиента: курсор +1 (подстройка отставания), ноги по пройденному, взмах, старые ходы — прочь.
    fn advance(&mut self) {
        let latest = self.latest();
        let lag = latest - self.t;
        let step = if lag > REMOTE_HIGH {
            1.1
        } else if lag < REMOTE_LOW {
            0.9
        } else {
            1.0
        };
        self.t_prev = self.t;
        self.t = (self.t + step).min(latest);
        if latest - self.t > 4.0 * REMOTE_HIGH {
            self.t = latest - REMOTE_DELAY; // давно не догоняли — сразу
        }
        let (a, b) = (self.at(self.t_prev).0, self.at(self.t).0);
        self.mv = Some((a, b, self.at(self.t).3));
        let speed = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt() as f32;
        self.age = self.age.wrapping_add(1);
        self.turn_body(&a, &b);
        self.amt_prev = self.amt;
        self.limb_prev = self.limb;
        self.amt += ((speed * 4.0).min(1.0) - self.amt) * 0.4; // LimbAnimator
        self.limb += self.amt;
        self.swing_prev = self.swing;
        if self.swing >= 0 {
            self.swing += 1;
            if self.swing >= crate::game::player::SWING_TICKS {
                self.swing = -1;
            }
        }
        while self.samples.len() > 2 && (self.samples[1].n as f64) < self.t_prev - 1.0 {
            self.samples.pop_front();
        }
    }
}

pub(crate) struct ClientSim {
    /// Зеркало мира (общая ручка с отрисовкой): сид, правки с сервера, свои предсказания.
    pub(crate) world: World,
    pub(crate) ctl: Ctl,
    pub(crate) view: View,
    /// Рецепты — предсказание сетки крафта тем же Menu::click.
    pub(crate) book: RecipeBook,
    /// Зеркало предметов сервера (и летящие копии подбора) — отрисовка.
    pub(crate) items: Items,
    /// Зеркало падающих блоков — отрисовка и «не в себя».
    pub(crate) falling: Vec<FallingBlock>,
    /// Время мира и суток: идёт само, поправка — Msg::Time.
    pub(crate) time: u64,
    pub(crate) day: u64,
    pub(crate) day_cycle: bool,
    /// Номер своей сущности; вошли ли (Welcome).
    pub(crate) me: u32,
    pub(crate) joined: bool,
    /// Номер своего тика.
    pub(crate) n: u32,
    /// Действия до границы тика, по порядку.
    pub(crate) acts: Vec<Act>,
    /// Номер последнего предсказания (Ack решает все до него).
    pub(crate) seq: u32,
    pub(crate) pending: Pending,
    /// Выбранная ячейка, о которой знает сервер.
    pub(crate) sent_sel: usize,
    /// Номер открытого окна у сервера (0 — инвентарь игрока).
    pub(crate) win: u8,
    /// Счётчик кликов по окнам (идёт всегда вперёд, `Act::Click.rev`) и клики, которых сервер ещё не подтвердил
    /// (`WinAck`): после полной копии окна они проигрываются заново поверх неё (design.md §4.4).
    win_clicks: u32,
    win_pending: Vec<(u32, WinClick)>,
    /// В кадре пришла полная копия окна или инвентаря — в конце кадра неподтверждённые клики проигрываются заново.
    win_rebase: bool,
    /// Тик сервера последнего кадра: новый тик — шаг prev сущностей и летящих копий.
    pub(crate) last_tick: u64,
    /// Сервер разрешает отладку (Welcome.debug): иначе V, K, J и прочие команды отладки молчат.
    pub(crate) debug_ok: bool,
    /// Куски из EditIndex, правки которых ещё не пришли (свой кусок и соседи — до первого тика).
    pending_edits: HashSet<(i32, i32)>,
    /// Другие игроки в радиусе слежения; ники вошедших (Tab — MP-4); чужие трещины.
    pub(crate) remotes: BTreeMap<u32, Remote>,
    pub(crate) names: HashMap<u32, String>,
    /// Tab (G2): номер скина и сглаженный пинг (мс) игроков из ListAdd / ListPing; имя сервера и число мест из Welcome.
    skins: HashMap<u32, u8>,
    pings: HashMap<u32, u16>,
    pub(crate) server_name: String,
    pub(crate) max_players: u16,
    /// Чат (G2): последние CHAT_KEEP строк и сколько строк пришло всего (JS читает новые по счётчику).
    chat: VecDeque<ChatLine>,
    chat_total: u32,
    cracks: HashMap<u32, ([i32; 3], i8)>,
    pub(crate) stats: ClientStats,
    /// Радиус правок (кусков), который клиент хочет («блоки до» в настройках), и тот, что уже ушёл серверу.
    pub(crate) view_want: u8,
    view_sent: u8,
    /// Вернулись из «ухода» (Away { on: false }), а сервер ещё не прислал Resume: тики стоят — ход со старого
    /// состояния сервер всё равно отбросил бы.
    pub(crate) away_wait: bool,
    /// Своё здоровье, как его прислал сервер (Msg::Vitals); счётчики кадра, что ведёт клиент по его изменениям
    /// (ClientPlayerEntity.updateHealth): крен камеры от удара (10 → 0) и мигание сердец (20 при уроне, 10 при лечении);
    /// тиков смерти для камеры и кнопки возрождения.
    pub(crate) vit: Vitals,
    /// Ест (предсказание: тот же автомат, что у сервера; потребляет только сервер).
    pub(crate) eating: Option<Eating>,
    pub(crate) cam_hurt: i32,
    pub(crate) hud_regen: i32,
    pub(crate) death_ticks: i32,
    /// Исходящие сообщения.
    pub(crate) out: Vec<C2S>,
    /// Свечение кабелей (0…1000) по последнему Msg::NetGlow (game/mek_client.rs).
    pub(crate) cable_glow: HashMap<[i32; 3], u16>,
    /// Предметы в пути по транспортёрам (M5, Msg::Transit): копия для рисования, сдвиг — в tick_visual.
    pub(crate) transit: crate::world::logistics::View,
    /// Мобы (game/mob/view.rs): зеркало и курсор отрисовки; моб под прицелом (ближе блока, в 3 блоках).
    pub(crate) mobs: crate::game::mob::view::MobMirror,
    pub(crate) aim_mob: Option<u32>,
    /// Другой игрок под прицелом (game/pvp.rs): цель удара.
    pub(crate) aim_player: Option<u32>,
    /// Права для подсказок команд (Msg::Mode, G17): 0 игрок, 1 админ, 2 владелец. Проверяет сервер.
    pub(crate) level: u8,
    /// Своя модель для вида от третьего лица (G16, game/third.rs).
    pub(crate) self_body: crate::game::third::SelfBody,
}

impl ClientSim {
    /// Клиент над миром world (ручка общая с отрисовкой); сид мира — тот, что пришлёт сервер (Welcome сверяет).
    pub(crate) fn new(world: &World) -> ClientSim {
        let seed = world.d.borrow().gen().seed as u64;
        ClientSim {
            world: world.share(),
            ctl: Ctl::new(),
            view: View::new(seed),
            book: RecipeBook::standard(),
            items: Items::new(),
            falling: Vec::new(),
            time: 0,
            day: 0,
            day_cycle: true,
            me: 0,
            joined: false,
            n: 0,
            acts: Vec::new(),
            seq: 0,
            pending: Pending::default(),
            sent_sel: 0,
            win: 0,
            win_clicks: 0,
            win_pending: Vec::new(),
            win_rebase: false,
            last_tick: 0,
            debug_ok: true,
            pending_edits: HashSet::new(),
            remotes: BTreeMap::new(),
            names: HashMap::new(),
            skins: HashMap::new(),
            pings: HashMap::new(),
            server_name: String::new(),
            max_players: 0,
            chat: VecDeque::new(),
            chat_total: 0,
            cracks: HashMap::new(),
            stats: ClientStats::default(),
            view_want: VIEW_DEFAULT,
            view_sent: VIEW_DEFAULT,
            away_wait: false,
            vit: Vitals::FULL,
            eating: None,
            cam_hurt: 0,
            hud_regen: 0,
            death_ticks: 0,
            out: Vec::new(),
            cable_glow: HashMap::new(),
            transit: Default::default(),
            mobs: Default::default(),
            aim_mob: None,
            aim_player: None,
            level: 0,
            self_body: Default::default(),
        }
    }

    /// Вход: Hello с пробой генератора (~4 куска) и токеном (нули — гость, ник не закрепляется).
    pub(crate) fn hello(&mut self, nick: &str, token: [u8; 16]) {
        self.view_sent = self.view_want;
        self.out.push(C2S::Hello(Hello { proto: PROTO, build: BUILD.into(), nick: nick.into(), token, view: self.view_want, probe: probe() }));
    }

    /// Радиус правок мира в кусках («блоки до» в настройках; сервер ограничивает своим VIEW_MAX). До Welcome — в ещё не
    /// отправленный Hello, а если тот уже ушёл, — сразу после входа; в игре — C2S::View. Тот же радиус — не шлётся.
    pub(crate) fn set_view(&mut self, radius: u8) {
        self.view_want = radius;
        for m in &mut self.out {
            if let C2S::Hello(h) = m {
                h.view = radius;
                self.view_sent = radius;
                return;
            }
        }
        self.send_view();
    }
    fn send_view(&mut self) {
        if self.joined && self.view_sent != self.view_want {
            self.view_sent = self.view_want;
            self.out.push(C2S::View { radius: self.view_want });
        }
    }

    /// Уход со вкладки или в меню с замороженными кадрами (on) и возвращение (!on): пока игрок «вне игры», сервер тикает
    /// его с пустым вводом; вернулся — ждём Resume и только потом тикаем. До входа — молчит.
    pub(crate) fn set_away(&mut self, on: bool) {
        if !self.joined {
            return;
        }
        self.out.push(C2S::Away { on });
        if !on {
            self.away_wait = true;
        }
    }

    // ------------------------------------------------------------ ввод (из кадра JS)

    pub(crate) fn set_keys(&mut self, mask: u32) {
        self.ctl.keys = mask;
    }
    /// Нажатие (P_*): копится до тика.
    pub(crate) fn press(&mut self, code: u32) {
        if let Some(c) = self.ctl.presses.get_mut(code as usize) {
            *c = c.saturating_add(1);
        }
    }
    /// Пауза, потеря фокуса, выход из захвата: отпустить всё и сбросить очереди (KeyBinding.unpressAll).
    pub(crate) fn release_all(&mut self) {
        self.ctl.keys = 0;
        self.ctl.presses = [0; NPRESS];
    }
    /// Мышь захвачена (Mouse.lockCursor): клик, вернувший мышь в игру, не бьёт, пока ЛКМ не отпущена.
    pub(crate) fn grab(&mut self) {
        self.ctl.attack_cd = 10000;
    }
    /// Колесо — выбор сразу в кадре (сервер узнает ячейку перед следующим действием): шаг 1 за событие, вверх — влево.
    pub(crate) fn scroll(&mut self, amount: f64) {
        if amount != 0.0 {
            self.ctl.inv.scroll(amount);
        }
    }
    /// Поворот мышью раз в кадр, после тиков.
    pub(crate) fn turn(&mut self, dx: f64, dy: f64, sens: f64) {
        self.ctl.player.turn(dx, dy, sens);
    }
    /// Взгляд в градусах Minecraft (свой: уходит серверу в каждом тике).
    pub(crate) fn set_look(&mut self, yaw: f32, pitch: f32) {
        self.ctl.player.yaw = yaw;
        self.ctl.player.pitch = pitch.clamp(-90.0, 90.0);
        self.view.hand.snap_look(self.ctl.player.yaw, self.ctl.player.pitch);
    }
    pub(crate) fn set_plant_offset(&mut self, on: bool) {
        self.ctl.plant_offset = on;
    }

    /// Действие — в очередь; перед ним — выбранная ячейка, если сервер знает другую.
    pub(crate) fn act(&mut self, a: Act) {
        self.sync_sel();
        self.acts.push(a);
    }
    pub(crate) fn sync_sel(&mut self) {
        if self.sent_sel != self.ctl.inv.selected {
            self.sent_sel = self.ctl.inv.selected;
            self.acts.push(Act::Hotbar(self.sent_sel as u8));
        }
    }

    // ------------------------------------------------------------ отладка (команды Debug: сразу у себя, серверу — по порядку)

    pub(crate) fn set_flying(&mut self, on: bool) {
        if !self.debug_ok {
            return;
        }
        self.ctl.player.set_flying(on);
        self.act(Act::Debug(DebugCmd::Fly(on)));
    }
    /// Переставить ступни (без интерполяции).
    pub(crate) fn teleport(&mut self, x: f64, y: f64, z: f64) {
        if !self.debug_ok {
            return;
        }
        self.ctl.player.teleport([x, y, z]);
        self.view.hand.snap_look(self.ctl.player.yaw, self.ctl.player.pitch);
        self.act(Act::Debug(DebugCmd::Teleport { pos: [x, y, z] }));
    }
    /// Поставить на землю у точки (x, z) — первое свободное место рядом (мир клиента — тот же, что у сервера).
    pub(crate) fn spawn(&mut self, x: f64, z: f64) {
        if !self.debug_ok {
            return;
        }
        let p = find_spawn(&mut self.world.d.borrow_mut(), x.floor() as i32, z.floor() as i32);
        crate::game::place_at_spawn(&mut self.ctl.player, p);
        self.view.hand.snap_look(self.ctl.player.yaw, self.ctl.player.pitch);
        self.act(Act::Debug(DebugCmd::Spawn { x, z }));
    }
    /// Отладка (J): сдвинуть игрока.
    pub(crate) fn debug_shift(&mut self, dx: f64, dy: f64, dz: f64) {
        let p = self.ctl.player.pos;
        self.teleport(p[0] + dx, p[1] + dy, p[2] + dz);
    }
    /// Положить предмет по правилам подбора. Ответ — сколько не влезло.
    pub(crate) fn give(&mut self, item: ItemId, count: i32) -> i32 {
        if idef(item).kind == ItemKind::None || !self.debug_ok {
            return count;
        }
        self.sync_sel();
        let left = give_into(&mut self.ctl.inv, item, count);
        self.act(Act::Debug(DebugCmd::Give { item, count }));
        left
    }
    /// Набор для приёмки (K).
    pub(crate) fn kit(&mut self) {
        if !self.debug_ok {
            return;
        }
        self.sync_sel();
        kit_into(&mut self.ctl.inv);
        self.act(Act::Debug(DebugCmd::Kit));
    }
    /// Площадка Mekanism (DebugCmd::Build): строит сервер, набор придёт копией инвентаря.
    pub(crate) fn debug_build(&mut self, kind: u8) {
        if self.debug_ok {
            self.sync_sel();
            self.act(Act::Debug(DebugCmd::Build(kind)));
        }
    }
    pub(crate) fn clear_inventory(&mut self) {
        if !self.debug_ok {
            return;
        }
        let sel = self.ctl.inv.selected;
        self.ctl.inv = Inventory::new();
        self.ctl.inv.selected = sel;
        self.act(Act::Debug(DebugCmd::ClearInventory));
    }
    /// Кнопка «Возродиться» экрана смерти: активна с 20-го тика смерти (сервер проверяет сам).
    pub(crate) fn respawn(&mut self) {
        if self.vit.dead && self.death_ticks >= 20 {
            self.out.push(C2S::Respawn);
        }
    }
    /// Отладка: урон N (обычный путь сервера) и поставить здоровье.
    pub(crate) fn debug_hurt(&mut self, amount: f32) {
        if self.debug_ok {
            self.act(Act::Debug(DebugCmd::Hurt { amount }));
        }
    }
    /// Отладка: скорость случайных тиков (gamerule randomTickSpeed).
    pub(crate) fn debug_tick_speed(&mut self, n: u32) {
        if self.debug_ok {
            self.act(Act::Debug(DebugCmd::TickSpeed(n)));
        }
    }
    pub(crate) fn debug_set_hp(&mut self, hp: f32) {
        if self.debug_ok {
            self.act(Act::Debug(DebugCmd::SetHp { hp }));
        }
    }
    /// Отладка: n мобов вида kind у ступней (x, y, z) — сцены и проверки (game/mob).
    pub(crate) fn debug_spawn_mob(&mut self, kind: u8, pos: [f64; 3], n: u8) {
        if self.debug_ok {
            self.act(Act::Debug(DebugCmd::SpawnMob { kind, pos, n }));
        }
    }
    /// Здоровье для HUD и экрана смерти: [здоровье, воздух, очки брони, горит 0/1, мёртв 0/1, тиков смерти (кнопка живёт с
    /// 20), мигание сердец (тиков осталось: 20 после урона, 10 после лечения), крен от удара (тиков), максимум здоровья,
    /// причина смерти (DamageKind) пока мёртв].
    pub(crate) fn vitals(&self) -> Vec<f64> {
        let v = &self.vit;
        vec![
            v.hp as f64,
            v.air as f64,
            v.armor as f64,
            v.burning as i32 as f64,
            v.dead as i32 as f64,
            self.death_ticks as f64,
            self.hud_regen as f64,
            self.cam_hurt as f64,
            crate::game::health::MAX_HP as f64,
            v.cause as f64,
        ]
    }
    pub(crate) fn set_time_of_day(&mut self, f: f64) {
        if !self.debug_ok {
            return;
        }
        self.day = set_time_of_day(f);
        self.act(Act::Debug(DebugCmd::Time(f)));
    }
    pub(crate) fn set_day_cycle(&mut self, on: bool) {
        if !self.debug_ok {
            return;
        }
        self.day_cycle = on;
        self.act(Act::Debug(DebugCmd::DayCycle(on)));
    }

    // ------------------------------------------------------------ тик

    /// Вошли и правки своего куска и соседей пришли — можно тикать (ASP-01 §2.10: тик игрока ждёт кусок).
    pub(crate) fn ready(&self) -> bool {
        if !self.joined {
            return false;
        }
        let p = self.ctl.player.pos;
        let (cx, cz) = crate::world::chunk_of(p[0].floor() as i32, p[2].floor() as i32);
        !self.pending_edits.iter().any(|c| (c.0 - cx).abs() <= 1 && (c.1 - cz).abs() <= 1)
    }

    /// Предсказанный тик (фазы до мира): рука → ввод → рука → игрок → рука → время; итог — C2S::Tick.
    pub(crate) fn tick(&mut self) {
        if !self.ready() || self.away_wait {
            return; // ждём Welcome и правки вокруг; после ухода — Resume
        }
        if self.vit.dead {
            // Мёртвый не управляет (isImmobile, ASP-12 §10.1): ввод обнулён, окно закрыто; тело падает по физике.
            self.ctl.keys = 0;
            self.ctl.presses = [0; NPRESS];
            if self.ctl.screen.is_some() {
                self.close_local();
            }
        }
        let pre = self.hand_pre();
        self.tick_use();
        self.hand_tick(pre);
        self.tick_player();
        self.hand_after_player();
        self.time += 1;
        if self.day_cycle {
            self.day = (self.day + 1) % DAY_TICKS;
        }
        self.sync_sel();
        self.n = self.n.wrapping_add(1);
        let mv = self.move_state();
        let inv = inv_hash(&self.ctl.inv);
        self.out.push(C2S::Tick(ClientTick { n: self.n, acts: std::mem::take(&mut self.acts), mv, inv }));
    }

    /// Итог движения тика (proto Move).
    pub(crate) fn move_state(&self) -> Move {
        move_of(&self.ctl.player, self.ctl.keys)
    }

    /// InGameHud.tick: имя предмета в руке (40 тиков при смене предмета) и строка действия.
    pub(crate) fn hud_tick(&mut self) {
        for c in [&mut self.cam_hurt, &mut self.hud_regen] {
            if *c > 0 {
                *c -= 1;
            }
        }
        if self.vit.dead && self.death_ticks < 20 {
            self.death_ticks += 1;
        } else if !self.vit.dead {
            self.death_ticks = 0;
        }
        let s = self.ctl.inv.hand();
        if s.is_empty() {
            self.view.name_fade = 0;
        } else if self.view.name_item == 0 || s.id() != self.view.name_item {
            self.view.name_fade = 40;
        } else if self.view.name_fade > 0 {
            self.view.name_fade -= 1;
        }
        self.view.name_item = s.id();
        if self.view.overlay > 0 {
            self.view.overlay -= 1;
        }
    }

    /// 1. Ввод и использование (MinecraftClient.tick до tickEntities): таймер ПКМ −1; луч 4.5 от глаз (tickDelta = 1);
    /// 1–9 — по одному за тик; все клики ЛКМ, все клики ПКМ; удержание ПКМ при таймере 0; ломание (ASP-04).
    /// Ставление идёт до движения игрока (ASP-05 §7.3); затем камера — по состоянию на конец прошлого тика.
    pub(crate) fn tick_use(&mut self) {
        let d = self.world.d.clone();
        let mut wd = d.borrow_mut();
        let w = &mut *wd;
        self.hud_tick();
        // Граница Перекрёстка (gen/spawn.rs): вошёл или появился на площади — «Перекрёсток…», вышел — «Вы покидаете…».
        let inside = protected(w, feet(&self.ctl.player));
        if self.view.zone_in != Some(inside) && (inside || self.view.zone_in.is_some()) {
            self.view.overlay = 60;
            self.view.overlay_msg = if inside { MSG_CROSSROADS_ENTER } else { MSG_CROSSROADS_LEAVE };
        }
        self.view.zone_in = Some(inside);
        // Небо по кнопке (G15): только в зоне; вышел — плавно к времени мира.
        if !inside {
            self.view.sky_pick = None;
        }
        self.view.sky_step(time_of_day(self.day, self.day_cycle, 0.0));
        let gate = w.gen().site.in_gate(feet(&self.ctl.player));
        if gate && !self.view.gate_in {
            self.view.overlay = 60;
            self.view.overlay_msg = MSG_WANDER;
        }
        self.view.gate_in = gate;
        if self.ctl.use_cd > 0 {
            self.ctl.use_cd -= 1;
        }
        self.ctl.target = self.pick(w, 1.0);
        self.aim_entities(w);
        if self.ctl.screen.is_some() {
            // Открыто окно: handleInputEvents не вызывается (ASP-05 §7.1) — нажатия мира забирает окно.
            self.ctl.presses = [0; NPRESS];
            self.ctl.player.camera_tick();
            return;
        }
        for i in 0..9 {
            if self.ctl.take(P_HOTBAR + i) {
                self.ctl.inv.selected = i as usize;
            }
        }
        // E (inventoryKey): окно инвентаря открывает клиент сам; setScreen отпускает все клавиши — остальное в этом
        // тике не идёт.
        if crate::game::rules::spectator(&self.ctl.player) {
            for p in [P_INVENTORY, P_DROP, P_DROP_ALL, P_MODE, P_MODE_BACK] {
                while self.ctl.take(p) {}
            }
        }
        if self.ctl.take(P_INVENTORY) {
            self.open_local(Menu::player(), 0);
            self.act(Act::OpenInventory);
            self.handle_block_breaking(w, false);
            self.ctl.player.camera_tick();
            return;
        }
        // Q / Ctrl+Q (dropKey): каждое накопленное нажатие, до ЛКМ и ПКМ; что-то выпало — взмах.
        while self.ctl.take(P_DROP) {
            self.drop_selected(w, false);
        }
        while self.ctl.take(P_DROP_ALL) {
            self.drop_selected(w, true);
        }
        // N / Shift+N: режим конфигуратора в руке (Mekanism) — у себя сразу, серверу — Act::ItemMode.
        for (key, back) in [(P_MODE, false), (P_MODE_BACK, true)] {
            while self.ctl.take(key) {
                let h = self.ctl.inv.hand_mut();
                if crate::game::mek_tool::cycle_hand(h, back) {
                    self.view.overlay_msg = crate::game::mek_tool::mode_msg(h.damage);
                    self.acts.push(Act::ItemMode { back });
                }
            }
        }
        // F: полёт выживания (G16) — у себя сразу, серверу Act::Flight до хода этого тика; разрешение — game/rules.rs.
        while self.ctl.take(P_FLIGHT) {
            let p = &mut self.ctl.player;
            if p.noclip || !(p.flying || crate::game::rules::may_fly(p.mode)) {
                continue;
            }
            let on = !p.flying;
            p.set_flight(on);
            self.acts.push(Act::Flight(on));
        }
        while self.ctl.take(P_FLY) {
            if !self.debug_ok || crate::game::rules::spectator(&self.ctl.player) {
                continue;
            }
            let on = !self.ctl.player.flying;
            self.set_flying(on);
            self.ctl.target = self.pick(w, 1.0); // дальность креатива
        }
        let mut broke = false;
        if crate::game::rules::spectator(&self.ctl.player) {
            // Наблюдатель (G17): не ломает, не ставит, не бьёт, не выбирает блок.
            for p in [P_ATTACK, P_USE, P_PICK] {
                while self.ctl.take(p) {}
            }
            self.handle_block_breaking(w, false);
            self.ctl.player.camera_tick();
            return;
        }
        if self.eating.is_some() {
            // Ест (handleInputEvents: isUsingItem): отпустил ПКМ — прервать; нажатия ЛКМ, ПКМ и СКМ выбрасываются — ударить и
            // ломать нельзя.
            if !self.ctl.held(K_USE) {
                self.stop_eating(true);
            }
            for p in [P_ATTACK, P_USE, P_PICK] {
                while self.ctl.take(p) {}
            }
        } else {
            while self.ctl.take(P_ATTACK) {
                broke |= self.do_attack(w);
            }
            while self.ctl.take(P_USE) {
                self.do_item_use(w);
            }
            while self.ctl.take(P_PICK) {
                self.do_item_pick(w); // СКМ — выбор блока (doItemPick, выживание)
            }
            if self.ctl.held(K_USE) && self.ctl.use_cd == 0 {
                self.do_item_use(w);
            }
        }
        let held = self.ctl.held(K_LOCKED) && !broke && self.ctl.held(K_ATTACK) && self.eating.is_none();
        self.handle_block_breaking(w, held);
        if self.ctl.attack_cd > 0 {
            self.ctl.attack_cd -= 1;
        }
        self.ctl.player.camera_tick();
    }

    /// 2. Игрок: выталкивание из блоков, физика Java (ASP-02), взмах, поза; подскок иконок.
    pub(crate) fn tick_player(&mut self) {
        let k = keys_of(self.ctl.keys);
        let landed = self.ctl.player.landed;
        self.eat_tick(); // tickItemStackUsage: до движения; доеденное снимает замедление в этот же тик
        let using = self.eating.is_some();
        self.ctl.player.tick(&mut *self.world.d.borrow_mut(), k, using);
        self.self_body.tick(&self.ctl.player);
        if self.ctl.player.landed != landed {
            if let Some((_, fd)) = self.ctl.player.landed {
                let p = self.feet_cell();
                self.view.events.push(EV_LANDED, p, (fd * 1000.0) as i32);
            }
        }
        self.step_sounds();
        self.ctl.inv.tick();
    }

    /// Шаги своего игрока (ASP-13 §2.1): ритм по пройденному, блок — опора под ногами (game/sound.rs).
    fn step_sounds(&mut self) {
        let p = &mut self.ctl.player;
        let climbing = !p.flying && p.climbing(&mut *self.world.d.borrow_mut());
        let body = crate::game::sound::Body {
            pos: p.pos,
            moved: [p.pos[0] - p.prev[0], p.pos[1] - p.prev[1], p.pos[2] - p.prev[2]],
            on_ground: p.on_ground,
            climbing,
            silent: p.flying || (p.on_ground && p.sneak_in),
            crouching: p.crouch,
            support: p.support,
        };
        self.view.steps.tick(&body, &mut *self.world.d.borrow_mut(), &mut self.view.events);
    }

    fn feet_cell(&self) -> [i32; 3] {
        let p = &self.ctl.player;
        [p.pos[0] as i32, p.pos[1] as i32, p.pos[2] as i32]
    }

    /// «Клиент» (ASP-10 §5, фаза 6): шаг крышек сундуков; случайная выборка блоков вокруг ступней
    /// (ClientWorld.doRandomBlockDisplayTicks): 667 раз по точке в радиусе 16 и 32 (смещение nextInt(r) − nextInt(r) по
    /// каждой оси) — горящая печь даёт треск (шанс 0.1) и дым с огоньком у низа лица. Блоки с показом бывают только в
    /// правках — выборка смотрит в правки и не грузит куски. «Горит» — по LIT в ячейке (FurnaceBlock.randomDisplayTick):
    /// печь сервера клиенту не видна.
    pub(crate) fn tick_visual(&mut self) {
        if !self.ready() {
            return;
        }
        for r in self.remotes.values_mut() {
            r.advance();
        }
        self.mobs.advance(self.last_tick, &mut self.view.events);
        self.transit.advance();
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        // Шаги других игроков — по их ходам (ASP-13 §2.1: те же правила, что у своих).
        for r in self.remotes.values_mut() {
            if let Some((a, b, fl)) = r.mv.take() {
                let body = Body {
                    pos: b,
                    moved: [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    on_ground: fl & PM_GROUND != 0,
                    climbing: false,
                    silent: fl & PM_FLY != 0 || (fl & PM_GROUND != 0 && fl & PM_CROUCH != 0),
                    crouching: fl & PM_CROUCH != 0,
                    support: None,
                };
                r.steps.tick(&body, &mut w, &mut self.view.events);
            }
        }
        w.be_mut().step_lids();
        self.mek_emitters(&w); // петли машин Mekanism (Ev::Emitter, game/mek_client.rs)
        let feet = [self.ctl.player.pos[0].floor() as i32, self.ctl.player.pos[1].floor() as i32, self.ctl.player.pos[2].floor() as i32];
        // Показ есть пока только у горящей печи: нет горящих — выборка не нужна (ГСЧ эффектов на эталоны не влияет).
        let any = w.be().furnaces().any(|p| w.edited_cell(p[0], p[1], p[2]).is_some_and(|c| state_of(c) & ST_LIT != 0));
        for _ in 0..667 {
            for r in [16, 32] {
                let mut p = feet;
                for k in 0..3 {
                    p[k] += self.view.fx_rng.below(r) - self.view.fx_rng.below(r);
                }
                if !any || p[1] < 0 || p[1] >= H {
                    continue;
                }
                let Some(c) = w.edited_cell(p[0], p[1], p[2]) else { continue };
                if bdef(block_of(c)).display == Display::Furnace && state_of(c) & ST_LIT != 0 {
                    self.furnace_display(p, facing_of(c));
                }
            }
        }
    }

    /// FurnaceBlock.randomDisplayTick (горящая): треск с шансом 0.1 у низа блока; точка — 0.52 от центра в сторону лица,
    /// вдоль лица ±0.3, высота 0…6/16 от низа; там SMOKE и FLAME.
    pub(crate) fn furnace_display(&mut self, p: [i32; 3], facing: u8) {
        let (cx, by, cz) = (p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5);
        if self.view.fx_rng.next_f64() < 0.1 {
            self.view.events.push(EV_FURNACE_CRACKLE, p, 0);
        }
        let side = self.view.fx_rng.next_f64() * 0.6 - 0.3;
        let up = self.view.fx_rng.next_f64() * 6.0 / 16.0;
        let d = DIRS[facing as usize];
        let (dx, dz) = if d[0] != 0 { (d[0] as f64 * 0.52, side) } else { (side, d[2] as f64 * 0.52) };
        let at = [((cx + dx) * 1000.0) as i32, ((by + up) * 1000.0) as i32, ((cz + dz) * 1000.0) as i32];
        self.view.events.push(EV_FURNACE_FX, at, 0);
    }

    // ------------------------------------------------------------ ломание (ClientPlayerInteractionManager, ASP-04)

    /// Творческий режим (G17) или отладочный полёт сквозь стены; полёт выживания — обычное выживание (game/rules.rs).
    pub(crate) fn creative(&self) -> bool {
        crate::game::rules::creative(&self.ctl.player)
    }

    /// Цель взгляда с долей a между тиками (a = 1 — в тике, по текущей позиции).
    pub(crate) fn pick(&self, w: &mut WorldData, a: f64) -> Option<Hit> {
        let reach = if self.creative() { REACH_CREATIVE } else { REACH };
        raycast(w, self.ctl.player.eye(a), self.ctl.player.look(), reach, self.ctl.plant_offset)
    }

    /// Предсказать поломку (режим Predict) и сообщить серверу: начало (мгновенный блок, креатив) или конец ломания.
    fn predict_break(&mut self, w: &mut WorldData, pos: [i32; 3], face: u8, done: bool) {
        self.sync_sel();
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        {
            let mode = Mode::Predict { seq, pending: &mut self.pending, falling: &self.falling };
            let mut c = ActCtx::new(w, &self.ctl.player, &mut self.ctl.inv, &mut self.view.events, mode);
            break_block(&mut c, pos);
        }
        self.acts.push(if done { Act::BreakDone { pos, seq } } else { Act::BreakStart { pos, face, seq } });
    }

    /// MinecraftClient.doAttack — на каждое нажатие ЛКМ. Ответ — блок сломан сразу (bl3).
    pub(crate) fn do_attack(&mut self, w: &mut WorldData) -> bool {
        if self.ctl.attack_cd > 0 {
            return false;
        }
        if self.attack_aimed_mob() || self.attack_aimed_player() {
            return false;
        }
        let mut broke = false;
        match self.ctl.target {
            Some(h) if w.get_block(h.pos[0], h.pos[1], h.pos[2]) != AIR => {
                let _ = self.attack_block(w, h.pos, h.face);
                broke = w.get_block(h.pos[0], h.pos[1], h.pos[2]) == AIR;
            }
            // Промах — или блок уже сломан другим кликом этого тика (switch проваливается в MISS).
            _ => {
                if !self.creative() {
                    self.ctl.attack_cd = 10;
                }
            }
        }
        self.swing();
        broke
    }

    /// Раз в тик (MinecraftClient.handleBlockBreaking).
    pub(crate) fn handle_block_breaking(&mut self, w: &mut WorldData, held: bool) {
        if !held {
            self.ctl.attack_cd = 0;
        }
        if self.ctl.attack_cd > 0 {
            return;
        }
        match self.ctl.target {
            Some(h) if held => {
                if w.get_block(h.pos[0], h.pos[1], h.pos[2]) != AIR && self.update_progress(w, h.pos, h.face) {
                    self.view.events.push(EV_CRUMB, h.pos, h.face as i32);
                    self.swing();
                }
            }
            _ => self.cancel_breaking(),
        }
    }

    pub(crate) fn same_target(&self, pos: [i32; 3]) -> bool {
        self.ctl.mine.pos == pos && self.ctl.mine.tool == tool_key(&self.ctl.inv)
    }

    /// Начало (attackBlock): клик или новая цель при удержании. Кулдаун 5 здесь не проверяется. false — ломать здесь
    /// нельзя (isBlockBreakingRestricted: защита Перекрёстка) — ни трещин, ни крошек, ни взмаха от удержания.
    pub(crate) fn attack_block(&mut self, w: &mut WorldData, pos: [i32; 3], face: u8) -> bool {
        if protected(w, pos) {
            self.zone_msg();
            return false;
        }
        if self.creative() {
            self.predict_break(w, pos, face, false);
            self.ctl.mine.cooldown = 5;
            return true;
        }
        if self.ctl.mine.breaking && self.same_target(pos) {
            return true;
        }
        let b = w.get_block(pos[0], pos[1], pos[2]);
        if b != AIR && mine_delta(w, &self.ctl.player, &self.ctl.inv, b) >= 1.0 {
            self.predict_break(w, pos, face, false); // мгновенно, кулдаун не ставится
        } else {
            if self.ctl.mine.breaking {
                let old = self.ctl.mine.pos; // новая цель: прежнее ломание — отмена (ABORT_DESTROY_BLOCK)
                self.act(Act::BreakAbort { pos: old, seq: self.seq });
            }
            self.ctl.mine.breaking = true;
            self.ctl.mine.pos = pos;
            self.ctl.mine.tool = tool_key(&self.ctl.inv);
            self.ctl.mine.progress = 0.0;
            self.ctl.mine.sound = 0.0;
            self.ctl.mine.stage = self.ctl.mine.stage_now();
            self.act(Act::BreakStart { pos, face, seq: self.seq });
        }
        true
    }

    /// Сообщение над хотбаром: «Перекрёсток: здесь ничего не изменить».
    fn zone_msg(&mut self) {
        self.view.overlay = 60;
        self.view.overlay_msg = MSG_CROSSROADS;
    }

    /// updateBlockBreakingProgress: true — показать крошку и махнуть.
    pub(crate) fn update_progress(&mut self, w: &mut WorldData, pos: [i32; 3], face: u8) -> bool {
        if self.ctl.mine.cooldown > 0 {
            self.ctl.mine.cooldown -= 1;
            return true;
        }
        if self.creative() {
            if protected(w, pos) {
                self.zone_msg();
                return false;
            }
            self.ctl.mine.cooldown = 5;
            self.predict_break(w, pos, face, false);
            return true;
        }
        if !self.same_target(pos) {
            return self.attack_block(w, pos, face); // в этот тик прогресса нет
        }
        let b = w.get_block(pos[0], pos[1], pos[2]);
        if b == AIR {
            self.ctl.mine.breaking = false;
            return false;
        }
        self.ctl.mine.progress += mine_delta(w, &self.ctl.player, &self.ctl.inv, b); // delta каждый тик заново: прыжок, вода
        if self.ctl.mine.sound % 4.0 == 0.0 {
            self.view.events.push(EV_HIT, pos, b as i32);
        }
        self.ctl.mine.sound += 1.0;
        if self.ctl.mine.progress >= 1.0 {
            self.ctl.mine.breaking = false;
            self.predict_break(w, pos, face, true);
            self.ctl.mine.progress = 0.0;
            self.ctl.mine.sound = 0.0;
            self.ctl.mine.cooldown = 5;
        }
        self.ctl.mine.stage = self.ctl.mine.stage_now();
        true
    }

    /// cancelBlockBreaking: трещины пропадают в тот же тик; кулдаун и позиция не трогаются.
    pub(crate) fn cancel_breaking(&mut self) {
        if self.ctl.mine.breaking {
            self.ctl.mine.breaking = false;
            self.ctl.mine.progress = 0.0;
            self.ctl.mine.stage = -1;
            let pos = self.ctl.mine.pos;
            self.act(Act::BreakAbort { pos, seq: self.seq });
        }
    }

    // ------------------------------------------------------------ ПКМ, СКМ, Q

    /// MinecraftClient.doItemUse: таймер 4 тика при любой попытке; во время ломания — ничего. Каждая рука, до которой
    /// дошло, — Act::Use (сервер повторяет то же решение).
    pub(crate) fn do_item_use(&mut self, w: &mut WorldData) {
        if self.ctl.mine.breaking {
            return;
        }
        self.ctl.use_cd = 4;
        if self.use_aimed_mob() {
            return;
        }
        for hand in [0usize, 1] {
            let stack = if hand == 0 { self.ctl.inv.hand() } else { self.ctl.inv.offhand };
            if let Some(h) = self.ctl.target {
                self.sync_sel();
                self.seq = self.seq.wrapping_add(1);
                let seq = self.seq;
                let (r, height, zone, sky) = {
                    let mode = Mode::Predict { seq, pending: &mut self.pending, falling: &self.falling };
                    let mut c = ActCtx::new(w, &self.ctl.player, &mut self.ctl.inv, &mut self.view.events, mode);
                    let r = interact_block(&mut c, hand, h);
                    (r, c.height_msg, c.zone_msg, c.sky)
                };
                if let Some(day) = sky {
                    // Кнопка неба (G15): серверу — тот же Use (он только машет рукой), небо меняется у себя.
                    self.view.sky_pick = Some(if day { SKY_DAY } else { SKY_NIGHT });
                    self.view.overlay = 60;
                    self.view.overlay_msg = if day { MSG_SKY_DAY } else { MSG_SKY_NIGHT };
                    self.view.events.push(Ev::Sound as i32, h.pos, Sfx::UiClick as i32);
                }
                let at = std::array::from_fn(|k| (h.point[k] - h.pos[k] as f64) as f32);
                self.acts.push(Act::Use { hand: hand as u8, pos: h.pos, face: h.face, at, inside: h.inside, seq });
                if height {
                    self.view.overlay = 60;
                    self.view.overlay_msg = MSG_HEIGHT;
                }
                if zone {
                    self.zone_msg();
                }
                let now = if hand == 0 { self.ctl.inv.hand() } else { self.ctl.inv.offhand };
                match r {
                    Use::Swing => {
                        self.swing();
                        if !now.is_empty() && now.count != stack.count {
                            self.view.events.push(EV_REEQUIP, h.pos, hand as i32);
                        }
                        return;
                    }
                    Use::Fail => return,
                    Use::Pass => {}
                }
            }
            // Предмет «в воздух»: ведро (ASP-15 §5) — у блоков ничего (PASS), еда начинает поедание (ASP-12 §9).
            match self.use_bucket(w, hand) {
                Use::Swing => {
                    self.swing();
                    self.view.events.push(EV_REEQUIP, self.feet_cell(), hand as i32);
                    return;
                }
                Use::Fail => return,
                Use::Pass => {}
            }
            if self.use_armor(w, hand) {
                return;
            }
            if hand == 0 && self.start_eating() {
                return;
            }
        }
    }

    /// ПКМ бронёй в руке hand: надеть (equip.rs) и сообщить серверу тем же Act::UseAir. false — в руке не броня.
    fn use_armor(&mut self, w: &mut WorldData, hand: usize) -> bool {
        let stack = if hand == 0 { self.ctl.inv.hand() } else { self.ctl.inv.offhand };
        if equip_slot(stack.id()).is_none() || self.vit.dead {
            return false;
        }
        self.sync_sel();
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let mode = Mode::Predict { seq, pending: &mut self.pending, falling: &self.falling };
        let mut c = ActCtx::new(w, &self.ctl.player, &mut self.ctl.inv, &mut self.view.events, mode);
        equip_from_hand(&mut c, hand);
        self.acts.push(Act::UseAir { hand: hand as u8, seq });
        self.swing();
        self.view.events.push(EV_REEQUIP, self.feet_cell(), hand as i32);
        true
    }

    /// ПКМ ведром в руке hand (bucket.rs): цель — для пустого свой луч по источникам, для полного — обычная цель взгляда;
    /// предсказание в режиме Predict и Act::Bucket серверу. Pass — в руке не ведро или цели нет.
    fn use_bucket(&mut self, w: &mut WorldData, hand: usize) -> Use {
        let stack = if hand == 0 { self.ctl.inv.hand() } else { self.ctl.inv.offhand };
        let ItemKind::Bucket { fluid } = idef(stack.id()).kind else { return Use::Pass };
        let hit = if fluid == 0 {
            let reach = if self.creative() { REACH_CREATIVE } else { REACH };
            raycast_ex(w, self.ctl.player.eye(1.0), self.ctl.player.look(), reach, self.ctl.plant_offset, true)
        } else {
            self.ctl.target
        };
        let Some(h) = hit else { return Use::Pass };
        self.sync_sel();
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let (r, zone) = {
            let mode = Mode::Predict { seq, pending: &mut self.pending, falling: &self.falling };
            let mut c = ActCtx::new(w, &self.ctl.player, &mut self.ctl.inv, &mut self.view.events, mode);
            let r = use_bucket(&mut c, hand, h);
            (r, c.zone_msg)
        };
        self.acts.push(Act::Bucket { hand: hand as u8, pos: h.pos, face: h.face, seq });
        if zone {
            self.zone_msg();
        }
        r
    }

    /// ПКМ с едой в главной руке: можно, только если сердца не полны [наше]; решает и сервер (UseAir → UseStop при отказе).
    pub(crate) fn start_eating(&mut self) -> bool {
        let s = self.ctl.inv.hand();
        if food_nutrition(s.id()).is_none() {
            return false;
        }
        if self.vit.dead || !(self.vit.hp < MAX_HP) || self.eating.is_some() {
            return false;
        }
        self.sync_sel();
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        self.eating = Some(Eating { left: EAT_TICKS, hand: 0, slot: self.ctl.inv.selected, item: s.id(), seq });
        self.view.hand.start_use(0, EAT_TICKS);
        self.acts.push(Act::UseAir { hand: 0, seq });
        true
    }

    /// Прервать поедание (отпустил ПКМ, сменил предмет); notify — сказать серверу.
    pub(crate) fn stop_eating(&mut self, notify: bool) {
        if self.eating.take().is_some() {
            self.view.hand.set_use(None);
            if notify {
                self.acts.push(Act::UseRelease);
            }
        }
    }

    /// Тик поедания (LivingEntity.tickItemStackUsage): предмет сменился — прервать; крошки на 8-м, 12-м… тике (left % 4 == 0
    /// при прошедших > 7); −1; на 0 — съедено (инвентарь и лечение приходят от сервера), звуки еды и отрыжки.
    pub(crate) fn eat_tick(&mut self) {
        let Some(mut e) = self.eating else { return };
        let s = if e.hand == 0 { self.ctl.inv.hand() } else { self.ctl.inv.offhand };
        if s.is_empty() || s.id() != e.item || (e.hand == 0 && self.ctl.inv.selected != e.slot) || self.vit.dead {
            self.stop_eating(true);
            return;
        }
        let at = self.feet_cell();
        if EAT_TICKS - e.left > 7 && e.left % 4 == 0 {
            self.view.events.push(Ev::Sound as i32, at, Sfx::Eat as i32);
        }
        e.left -= 1;
        if e.left <= 0 {
            self.eating = None;
            self.view.hand.set_use(None);
            // Предсказание: стек −1 (то же делает сервер в этом тике — хеш инвентаря сходится; лечение пришлёт сервер).
            let slot = self.ctl.inv.selected;
            self.ctl.inv.set(slot, s.with_count(s.n() - 1));
            self.view.events.push(Ev::Sound as i32, at, Sfx::EatDone as i32);
            self.view.events.push(Ev::Sound as i32, at, Sfx::Burp as i32);
            return;
        }
        self.view.hand.set_use(Some((e.hand, e.left, EAT_TICKS)));
        self.eating = Some(e);
    }

    /// Средняя кнопка (doItemPick): выбор или перестановка сразу, серверу — Act::Pick (он сделает то же).
    pub(crate) fn do_item_pick(&mut self, w: &mut WorldData) {
        let Some(h) = self.ctl.target else { return };
        self.sync_sel();
        let creative = self.creative();
        item_pick(w, &mut self.ctl.inv, h.pos, creative);
        self.acts.push(Act::Pick { pos: h.pos });
        self.sent_sel = self.ctl.inv.selected;
    }

    /// Выброс из руки (Q / Ctrl+Q): стек — сразу у себя, предмет в мире создаст сервер.
    pub(crate) fn drop_selected(&mut self, w: &WorldData, all: bool) {
        self.sync_sel();
        // На Перекрёстке вещь остаётся у игрока (spawn.md §5); сервер решает так же по своей позиции.
        if !self.ctl.inv.hand().is_empty() && protected(w, feet(&self.ctl.player)) {
            self.zone_msg();
            return;
        }
        let s = self.ctl.inv.take_selected(all);
        if s.is_empty() {
            return;
        }
        self.view.hand.quiet(0); // правка стека руки на месте — без «нырка» (hand.rs)
        self.acts.push(Act::Drop { all });
        self.swing();
    }

    pub(crate) fn swing(&mut self) {
        self.ctl.player.swing_hand();
        let p = self.feet_cell();
        self.view.events.push(EV_SWING, p, 0);
        self.acts.push(Act::Swing { hand: 0 });
    }

    // ------------------------------------------------------------ окна (предсказание тем же Menu; screen.js — автомат ввода)

    /// Открыть окно у себя (E — сам клиент; блоки — по WinOpen): прежнее закрывается без сообщения серверу (он знает);
    /// все клавиши отпускаются (setScreen → KeyBinding.unpressAll).
    pub(crate) fn open_local(&mut self, m: Menu, win: u8) {
        if self.ctl.screen.is_some() {
            self.close_local();
        }
        let kind = screen_code(&m.kind);
        self.ctl.screen = Some(m);
        self.win = win;
        self.win_pending.clear();
        self.release_all();
        let p = self.feet_cell();
        self.view.events.push(EV_SCREEN, p, kind);
    }
    /// Закрыть окно у себя: курсор и сетка — в инвентарь (не влезло — бросит сервер).
    pub(crate) fn close_local(&mut self) {
        if self.ctl.screen.is_none() {
            return;
        }
        self.with_screen(|m, u| m.close(u));
        self.ctl.screen = None;
        self.win_pending.clear();
        let p = self.feet_cell();
        self.view.events.push(EV_SCREEN, p, 0);
    }
    /// Открыть окно инвентаря сразу (проверки; в игре E идёт через press в тике).
    pub(crate) fn open_inventory(&mut self) {
        self.open_local(Menu::player(), 0);
        self.act(Act::OpenInventory);
    }
    /// Закрыть окно (E, Esc): у себя сразу, серверу — по очереди.
    pub(crate) fn close_screen(&mut self) {
        if self.ctl.screen.is_none() {
            return;
        }
        self.close_local();
        self.act(Act::CloseWindow { win: self.win });
    }

    /// Действие над открытым окном с хранилищами клиента (хранилище блока — зеркало блок-сущности).
    pub(crate) fn with_screen<R>(&mut self, f: impl FnOnce(&mut Menu, &mut Ui) -> R) -> Option<R> {
        let mut m = self.ctl.screen.take()?;
        let r = {
            let d = self.world.d.clone();
            let mut w = d.borrow_mut();
            let locked = m.block_pos().is_some_and(|p| protected(&w, p));
            let block = m.block_pos().and_then(|p| w.be_mut().container(p));
            let creative = crate::game::rules::creative(&self.ctl.player);
            let mut u = Ui { inv: &mut self.ctl.inv, block, book: &self.book, ev: &mut self.ctl.ui_ev, alive: true, creative, locked };
            f(&mut m, &mut u)
        };
        self.ctl.screen = Some(m);
        self.flush_ui();
        Some(r)
    }
    /// События окна у клиента: надевание брони — звук; бросок создаст сервер.
    fn flush_ui(&mut self) {
        let p = self.feet_cell();
        for e in std::mem::take(&mut self.ctl.ui_ev) {
            if let UiEv::Equip(item) = e {
                self.view.events.push(EV_EQUIP, p, item as i32);
            }
        }
    }
    /// Хеш открытого окна (курсор и все слоты) — серверу для сверки предсказания.
    fn win_hash(&mut self) -> u32 {
        self.with_screen(|m, u| menu_hash(m, u)).unwrap_or(0)
    }

    /// Запомнить предсказанный клик до подтверждения сервера; номер идёт в `Act::Click.rev`.
    fn win_push(&mut self, c: WinClick) -> u32 {
        self.win_clicks = self.win_clicks.wrapping_add(1);
        if self.win_pending.len() >= WIN_PENDING_MAX {
            self.win_pending.remove(0);
        }
        self.win_pending.push((self.win_clicks, c));
        self.win_clicks
    }
    /// Пришла полная копия окна или инвентаря (серверное состояние на конец тика, до `WinAck.rev` включительно):
    /// клики после него сервер ещё не видел — проигрываются заново поверх копии, чтобы предсказание не откатывалось.
    fn win_replay(&mut self) {
        if self.ctl.screen.is_none() || self.win_pending.is_empty() {
            return;
        }
        self.with_screen(|m, _| m.qc_reset());
        for (_, c) in self.win_pending.clone() {
            match c {
                WinClick::Click { slot, button, action } => {
                    self.with_screen(|m, u| m.click(u, slot, button, action));
                }
                WinClick::QuickAll { slot, item, damage } => {
                    let like = ItemStack { item, count: 1, damage };
                    self.with_screen(|m, u| m.quick_move_all(u, slot as usize, like));
                }
            }
        }
    }

    pub(crate) fn screen_click(&mut self, slot: i32, button: i32, action: i32) {
        if self.with_screen(|m, u| m.click(u, slot, button, action)).is_none() {
            return;
        }
        let hash = self.win_hash();
        let rev = self.win_push(WinClick::Click { slot, button, action });
        self.act(Act::Click { win: self.win, rev, slot: slot as i16, button: button as u8, action: action as u8, hash });
    }
    pub(crate) fn screen_quick_move_all(&mut self, slot: i32, item: ItemId, damage: u16) {
        if slot < 0 {
            return;
        }
        let like = ItemStack { item, count: 1, damage };
        if self.with_screen(|m, u| m.quick_move_all(u, slot as usize, like)).is_none() {
            return;
        }
        let hash = self.win_hash();
        let rev = self.win_push(WinClick::QuickAll { slot, item, damage });
        self.act(Act::QuickMoveAll { win: self.win, rev, slot: slot as i16, item, damage, hash });
    }
    pub(crate) fn screen_drag_accepts(&mut self, slot: i32, mode: u8, n: u32) -> bool {
        slot >= 0 && self.with_screen(|m, u| m.drag_accepts(u, slot as usize, mode, n as usize)).unwrap_or(false)
    }
    pub(crate) fn screen_drag_preview(&mut self, mode: u8, slots: &[i32]) -> Vec<i32> {
        let ids: Vec<usize> = slots.iter().filter(|&&s| s >= 0).map(|&s| s as usize).collect();
        self.with_screen(|m, u| m.drag_preview(u, mode, &ids)).unwrap_or_default()
    }

    // ------------------------------------------------------------ ответ сервера

    pub(crate) fn apply(&mut self, m: S2C) {
        match m {
            S2C::Welcome(wl) => self.welcome(wl),
            S2C::Reject { code, text } => {
                self.stats.reject = Some(code);
                self.stats.rejected = Some(text);
            }
            S2C::Kick { text } => {
                self.stats.rejected = Some(text);
                self.joined = false;
            }
            S2C::Bundle(b) => self.bundle(b),
        }
    }

    fn welcome(&mut self, wl: Welcome) {
        if wl.seed != self.world.seed() {
            self.stats.rejected = Some(format!("сид сервера {} ≠ мира клиента {}", wl.seed, self.world.seed()));
            return;
        }
        self.me = wl.you;
        self.joined = true;
        self.vit = Vitals::FULL;
        self.eating = None;
        self.cam_hurt = 0;
        self.hud_regen = 0;
        self.death_ticks = 0;
        self.server_name = wl.server_name;
        self.max_players = wl.max;
        self.send_view(); // радиус сменили между Hello и Welcome
        self.debug_ok = wl.debug;
        self.acts.clear(); // до входа — не в счёт (позицию, время и инвентарь даёт сервер)
        self.time = wl.time;
        self.day = wl.day;
        self.day_cycle = wl.cycle;
        self.last_tick = wl.time;
        let me = wl.me;
        let p = &mut self.ctl.player;
        p.teleport(me.pos);
        p.vel = me.vel;
        p.yaw = me.yaw;
        p.pitch = me.pitch.clamp(-90.0, 90.0);
        p.fall = me.fall;
        p.on_ground = me.on_ground;
        p.flying = me.flying;
        p.noclip = false;
        p.crouch = me.crouch;
        p.cam_y = p.eye_height();
        p.last_cam_y = p.cam_y;
        self.view.hand.snap_look(p.yaw, p.pitch);
        self.self_body.snap(p.yaw);
        let mut inv = Inventory::new();
        for (i, s) in me.inv {
            if (i as usize) <= OFFHAND {
                inv.set(i as usize, s);
            }
        }
        inv.selected = (me.selected as usize).min(8);
        self.sent_sel = inv.selected;
        self.ctl.inv = inv;
    }

    fn bundle(&mut self, b: Bundle) {
        self.stats.frames_in += 1;
        if b.tick != self.last_tick {
            // Новый тик сервера: позиция начала тика — у всех сущностей; летящие копии — шаг (ASP-07 §5.3).
            self.last_tick = b.tick;
            for e in &mut self.items.list {
                e.prev = e.pos;
            }
            for f in &mut self.falling {
                f.prev = f.pos;
            }
            for a in &mut self.items.anims {
                a.t += 1;
            }
            self.items.anims.retain(|a| a.t < 3);
        }
        for m in b.msgs {
            self.msg(m);
        }
        if std::mem::take(&mut self.win_rebase) {
            self.win_replay();
        }
        let mut w = self.world.d.borrow_mut();
        w.take_removed(); // содержимое зеркала не рассыпается — предметы пришлёт сервер
        w.take_unsaved();
    }

    fn msg(&mut self, m: Msg) {
        match m {
            Msg::EditIndex(list) => self.pending_edits.extend(list.iter().map(|e| (e.0, e.1))),
            Msg::ChunkEdits { cx, cz, edits } => {
                self.pending_edits.remove(&(cx, cz));
                let Some(list) = undelta_keys(&edits, CHUNK_CELLS) else { return };
                let d = self.world.d.clone();
                let mut w = d.borrow_mut();
                w.forget_chunk(cx, cz);
                for (k, c) in list {
                    let (lx, y, lz) = crate::world::local_pos(k);
                    w.set_cell(cx * CS + lx, y, cz * CS + lz, c);
                }
            }
            Msg::Forget { cx, cz } => self.world.d.borrow_mut().forget_chunk(cx, cz),
            Msg::Blocks { cx, cz, list } => {
                let d = self.world.d.clone();
                let mut w = d.borrow_mut();
                for (k, c) in list {
                    if k >= CHUNK_CELLS {
                        continue;
                    }
                    let (lx, y, lz) = crate::world::local_pos(k);
                    let p = [cx * CS + lx, y, cz * CS + lz];
                    match self.pending.cells.get_mut(&p) {
                        Some(e) => e.server = Some(c),
                        None => {
                            w.set_cell(p[0], p[1], p[2], c);
                        }
                    }
                }
            }
            Msg::Ack { seq } => self.ack(seq),
            Msg::BreakStage { who, pos, stage } => {
                if stage < 0 {
                    self.cracks.remove(&who);
                } else {
                    self.cracks.insert(who, (pos, stage));
                }
            }
            Msg::Spawn { id, ent } => match ent {
                Ent::Item { stack, pos, prev, vel, spin, offset } => {
                    self.items.list.retain(|e| e.id != id);
                    let mut e = self.items.make(id, pos, stack, &mut GameRng(0));
                    e.prev = prev;
                    e.vel = vel;
                    e.spin_age = spin;
                    e.offset = offset;
                    self.items.list.push(e);
                }
                Ent::Falling { cell, pos, prev, vel } => {
                    self.falling.retain(|f| f.id != id);
                    let mut f = FallingBlock::new(id, cell, [0, 0, 0]);
                    f.pos = pos;
                    f.prev = prev;
                    f.vel = vel;
                    self.falling.push(f);
                }
                Ent::Mob { kind, mv } => self.mobs.spawn(id, kind, mv, self.last_tick),
                Ent::Player { nick, skin, mv } => {
                    if id != self.me {
                        self.names.insert(id, nick.clone());
                        self.remotes.insert(id, Remote::new(nick, skin, mv));
                    }
                }
            },
            Msg::Despawn(ids) => {
                self.items.list.retain(|e| !ids.contains(&e.id));
                self.falling.retain(|f| !ids.contains(&f.id));
                for id in &ids {
                    self.remotes.remove(id);
                }
                self.mobs.remove(&ids);
            }
            Msg::PlayerMoves(list) => {
                for mv in list {
                    if let Some(r) = self.remotes.get_mut(&mv.id) {
                        r.push(mv);
                    }
                }
            }
            Msg::Anim { id, kind: 1 } => self.mobs.hurt(id),
            Msg::Anim { id, kind } => {
                if let (0, Some(r)) = (kind, self.remotes.get_mut(&id)) {
                    if r.swing < 0 || r.swing >= crate::game::player::SWING_TICKS / 2 {
                        r.swing = 0; // LivingEntity.swingHand
                    }
                }
            }
            Msg::Moves(list) => {
                for mv in list {
                    if let Some(e) = self.items.list.iter_mut().find(|e| e.id == mv.id) {
                        e.pos = mv.pos;
                        e.spin_age = mv.spin;
                    } else if let Some(f) = self.falling.iter_mut().find(|f| f.id == mv.id) {
                        f.pos = mv.pos;
                    }
                }
            }
            Msg::ItemCount { id, count } => {
                if let Some(e) = self.items.list.iter_mut().find(|e| e.id == id) {
                    e.stack.count = count as i32;
                }
            }
            Msg::Pickup { id, by, stack, from, spin } => {
                let offset = self.items.list.iter().find(|e| e.id == id).map_or(0.0, |e: &ItemEntity| e.offset);
                // Свой подбор летит к себе; чужой — к подобравшему (его видно, пока он в слежении).
                let to = (by != self.me).then_some(by);
                self.items.anims.push(PickupAnim { stack, from, spin_age: spin, offset, t: 0, to });
            }
            // Полёт задал сервер (возрождение, отказ): идёт до Teleport, ходы до TeleportAck не в счёт.
            Msg::Flight(on) => self.ctl.player.set_flight(on),
            // Режим и права (G17): тело — тем же rules::apply_mode, что у сервера; Teleport идёт следом.
            Msg::Mode { mode, level } => {
                crate::game::rules::apply_mode(&mut self.ctl.player, crate::game::rules::GameMode::from_code(mode));
                self.level = level;
                if crate::game::rules::spectator(&self.ctl.player) && self.ctl.screen.is_some() {
                    self.close_local();
                }
            }
            Msg::Teleport { id, pos, yaw, pitch } => {
                self.ctl.player.teleport(pos);
                self.self_body.snap(yaw);
                self.ctl.player.yaw = yaw;
                self.ctl.player.pitch = pitch;
                self.view.hand.snap_look(yaw, pitch);
                self.stats.teleports += 1;
                self.acts.push(Act::TeleportAck { id });
            }
            // Отбрасывание ударом моба (ASP-12 §5: замена скорости): поставить и подтвердить в следующем тике — сервер
            // ставит ту же скорость своему телу по VelocityAck до теневого хода.
            Msg::Velocity { id, v } => {
                if v.iter().all(|x| x.is_finite()) {
                    self.ctl.player.vel = v;
                    self.acts.push(Act::VelocityAck { id });
                }
            }
            Msg::MobMoves(list) => self.mobs.moves(list, self.last_tick),
            Msg::Resume { id, n, mv } => {
                // Сервер тикал нас, пока мы были «вне игры» (Away): встать в его состояние (взгляд — свой), номер тика
                // продолжить с его, подтвердить — дальше ходы принимаются.
                let p = &mut self.ctl.player;
                adopt_move(p, &mv);
                p.prev = p.pos;
                p.last_cam_y = p.cam_y;
                self.n = n;
                self.away_wait = false;
                self.stats.resumes += 1;
                self.win_pending.clear(); // клики до ухода сервер мог не получить (inbox сброшен) — проигрывать нечего
                self.acts.push(Act::TeleportAck { id });
            }
            Msg::Inventory { slots, selected } => {
                self.win_rebase |= self.ctl.screen.is_some();
                for (i, s) in slots.into_iter().enumerate().take(OFFHAND + 1) {
                    let old = self.ctl.inv.get(i);
                    if s.n() > old.n() && s.same_kind(&old) || (old.is_empty() && !s.is_empty()) {
                        self.ctl.inv.bob[i] = 5; // подскок иконки (ClientPlayNetworkHandler: стек вырос)
                    }
                    self.ctl.inv.set(i, s);
                }
                if let Some(sel) = selected {
                    self.ctl.inv.selected = (sel as usize).min(8);
                    self.sent_sel = self.ctl.inv.selected;
                }
            }
            Msg::WinOpen { win, kind, pos, rev: _, slots } => {
                let menu = match (kind as i32, pos) {
                    (k, Some(p)) if k == ScreenKind::Crafting as i32 => Menu::crafting(p),
                    (k, Some(p)) if k == ScreenKind::Chest as i32 => Menu::chest(p),
                    (k, Some(p)) if k == ScreenKind::Furnace as i32 => Menu::furnace(p),
                    (k, Some(p)) if k == ScreenKind::Machine as i32 => self.mek_menu(p).unwrap_or_else(Menu::player),
                    _ => Menu::player(),
                };
                if let Some(p) = pos {
                    let mut w = self.world.d.borrow_mut();
                    if let Some(c) = w.be_mut().container(p) {
                        for (i, s) in slots.into_iter().enumerate() {
                            c.set(i, s);
                        }
                    }
                }
                self.open_local(menu, win);
            }
            Msg::WinSlots { win, rev: _, list } => {
                if win != self.win {
                    return;
                }
                self.win_rebase |= list.iter().any(|&(slot, _)| slot < 0); // с курсором — полная копия
                self.with_screen(|m, u| {
                    for (slot, s) in list {
                        if slot < 0 {
                            m.cursor = s;
                            continue;
                        }
                        let Some(def) = m.slots.get(slot as usize) else { continue };
                        match def.store {
                            Store::Player(_) => {}
                            Store::Grid(k) => m.grid.cells[k as usize] = s,
                            Store::Result => m.result = s,
                            Store::Block(k) => {
                                if let Some(b) = u.block.as_mut() {
                                    b.set(k as usize, s);
                                }
                            }
                        }
                    }
                });
            }
            Msg::WinAck { win, rev } => {
                if win == self.win {
                    self.win_pending.retain(|&(n, _)| n.wrapping_sub(rev) as i32 > 0);
                }
            }
            Msg::WinData { win, values } if matches!(self.ctl.screen.as_ref().map(|m| m.kind), Some(MenuKind::Mek(..))) => {
                let Some(MenuKind::Mek(p, _)) = self.ctl.screen.as_ref().map(|m| m.kind) else { return };
                if win == self.win {
                    if let Some(m) = self.world.d.borrow_mut().be_mut().mek_mut(p) {
                        m.apply_view(&values);
                    }
                }
            }
            Msg::NetGlow(list) => self.on_net_glow(&list),
            Msg::MekSides(list) => self.on_mek_sides(&list),
            Msg::Transit(list) => self.transit.apply(list),
            Msg::TransitEnd(ids) => self.transit.end(&ids),
            Msg::WinData { win, values } => {
                let Some(MenuKind::Furnace(p)) = self.ctl.screen.as_ref().map(|m| m.kind) else { return };
                if win != self.win || values.len() < 4 {
                    return;
                }
                if let Some(f) = self.world.d.borrow_mut().be_mut().furnace_mut(p) {
                    (f.burn, f.fuel_total, f.cook, f.cook_total) = (values[0], values[1], values[2], values[3]);
                }
            }
            Msg::WinClose { win } => {
                if win == self.win && self.ctl.screen.is_some() {
                    self.close_local();
                }
            }
            Msg::ChestViewers { pos, n } => {
                if let Some(c) = self.world.d.borrow_mut().be_mut().chest_mut(pos) {
                    c.viewers = n;
                    c.lid.open = n > 0;
                }
            }
            Msg::Fx { code, pos, arg } => self.view.events.push(code as i32, pos, arg),
            Msg::Time { time, day, cycle } => {
                self.time = time;
                self.day = day;
                self.day_cycle = cycle;
            }
            Msg::Ping { id } => self.out.push(C2S::Pong { id }),
            Msg::ListAdd { id, nick, skin } => {
                self.names.insert(id, nick);
                self.skins.insert(id, skin);
            }
            Msg::ListRemove(id) => {
                self.names.remove(&id);
                self.skins.remove(&id);
                self.pings.remove(&id);
                self.cracks.remove(&id);
            }
            Msg::Vitals(v) => {
                // ClientPlayerEntity.updateHealth: снижение — крен 10 тиков и мигание 20; рост — мигание 10.
                let old = self.vit;
                if v.hp < old.hp {
                    self.cam_hurt = 10;
                    self.hud_regen = 20;
                } else if v.hp > old.hp {
                    self.hud_regen = 10;
                }
                self.vit = v;
            }
            Msg::UseStop { seq } => {
                if self.eating.is_some_and(|e| e.seq == seq) {
                    self.stop_eating(false);
                }
            }
            Msg::ListPing(list) => self.pings.extend(list),
            Msg::Chat { kind, from, text } => {
                // Слова игрока — «<Ник> текст» (ник на момент прихода: после выхода его уже не найти); остальное готово.
                let text = if kind == CHAT_PLAYER { format!("<{}> {}", self.names.get(&from).map_or("?", String::as_str), text) } else { text };
                self.chat.push_back(ChatLine { kind, text });
                if self.chat.len() > CHAT_KEEP {
                    self.chat.pop_front();
                }
                self.chat_total = self.chat_total.wrapping_add(1);
            }
        }
    }

    /// Ack{seq}: предсказания до seq решены — в клетку присланное сервером или прежнее (сервер её не трогал).
    fn ack(&mut self, seq: u32) {
        let done: Vec<([i32; 3], Cell)> = self
            .pending
            .cells
            .iter()
            .filter(|(_, e)| e.seq.wrapping_sub(seq) as i32 <= 0)
            .map(|(p, e)| (*p, e.server.unwrap_or(e.old)))
            .collect();
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        for (p, c) in done {
            self.pending.cells.remove(&p);
            if w.get_cell(p[0], p[1], p[2]) != c {
                self.stats.rollbacks += 1;
                w.set_cell(p[0], p[1], p[2], c);
            }
        }
    }

    // ------------------------------------------------------------ данные кадра (рендер, HUD, отладка)

    /// Тиков с начала игры (время мира).
    pub(crate) fn ticks(&self) -> f64 {
        self.time as f64
    }

    /// Камера кадра: [x, y, z глаз, yaw°, pitch°, множитель FOV, покачивание: сдвиг x, y, крен Z°, кивок X°,
    /// полёт 0/1, взмах руки 0…1 (интерполированный)].
    pub(crate) fn view(&self, alpha: f64) -> Vec<f64> {
        let mut v = self.ctl.player.view(alpha).to_vec();
        v.push(self.ctl.player.flying as i32 as f64);
        let (l, c) = (self.ctl.player.last_swing, self.ctl.player.swing);
        let mut d = c - l;
        if d < 0.0 {
            d += 1.0;
        }
        v.push((l + d * alpha as f32) as f64);
        // Крен камеры от удара и смерти (GameRenderer.tiltViewWhenHurt): угол Rz° от удара, направление удара g°, крен смерти Rz°;
        // при смерти FOV делится (getFov). Матрица — web/game/camera.js: Rz(смерть)·Ry(−g)·Rz(удар)·Ry(g).
        let a = alpha as f32;
        v.push(hurt_tilt_deg(self.cam_hurt as f32 - a) as f64);
        v.push(self.vit.tilt as f64);
        let dead = self.vit.dead;
        v.push(if dead { death_tilt_deg(self.death_ticks as f32 + a) as f64 } else { 0.0 });
        if dead {
            v[5] /= death_fov_div(self.death_ticks as f32 + a) as f64;
        }
        v
    }

    /// Рамка кадра (луч с интерполированными глазами): [x, y, z, грань, затем коробки обводки по 6 чисел] или пусто.
    pub(crate) fn outline(&self, alpha: f64) -> Vec<f64> {
        let mut w = self.world.d.borrow_mut();
        let Some(h) = self.pick(&mut w, alpha) else { return Vec::new() };
        let mut boxes = Vec::new();
        let c = w.get_cell(h.pos[0], h.pos[1], h.pos[2]);
        outline_at(c, h.pos[0], h.pos[1], h.pos[2], self.ctl.plant_offset, &mut boxes);
        let mut out = vec![h.pos[0] as f64, h.pos[1] as f64, h.pos[2] as f64, h.face as f64];
        for s in boxes {
            out.extend_from_slice(&[s.lo[0], s.lo[1], s.lo[2], s.hi[0], s.hi[1], s.hi[2]]);
        }
        out
    }

    /// Трещины: [x, y, z, стадия 0…9] или пусто. Свои; не ломаю сам — ближайшие чужие (BreakStage).
    pub(crate) fn crack(&self) -> Vec<i32> {
        if self.ctl.mine.stage >= 0 {
            let p = self.ctl.mine.pos;
            return vec![p[0], p[1], p[2], self.ctl.mine.stage];
        }
        let me = self.ctl.player.pos;
        let d2 = |p: &[i32; 3]| (0..3).map(|k| (p[k] as f64 + 0.5 - me[k]).powi(2)).sum::<f64>();
        let mut list: Vec<&([i32; 3], i8)> = self.cracks.values().collect();
        list.sort_by(|a, b| d2(&a.0).total_cmp(&d2(&b.0)));
        list.first().map_or(Vec::new(), |(p, s)| vec![p[0], p[1], p[2], *s as i32])
    }

    /// Другие игроки в кадре: по 13 чисел — id, x, y, z (ступни, интерполяция по alpha), yaw° и pitch° взгляда, фаза шага
    /// ног, размах шага (0…1), взмах руки (0…1, 0 — нет), флаги PM_* (на земле, присед, бег, полёт), поворот тела°
    /// (turnHead: голова от тела не дальше 75°), возраст в тиках с долей (качание рук в покое), номер скина 0…7.
    pub(crate) fn player_draws(&self, alpha: f64) -> Vec<f32> {
        let a = alpha as f32;
        let mut v = Vec::with_capacity(self.remotes.len() * 13);
        for (&id, r) in &self.remotes {
            let (pos, yaw, pitch, flags) = r.at(r.t_prev + (r.t - r.t_prev) * alpha);
            if flags & PM_DEAD != 0 {
                continue; // мёртвый: тело убрано (вещи выпали у него), модель и ник не рисуются
            }
            let swing = match (r.swing_prev, r.swing) {
                (_, s) if s < 0 => 0.0,
                (p, s) => (p.max(0) as f32 + (s - p.max(0)) as f32 * a) / crate::game::player::SWING_TICKS as f32,
            };
            let limb = r.limb_prev + (r.limb - r.limb_prev) * a;
            let amt = r.amt_prev + (r.amt - r.amt_prev) * a;
            v.extend_from_slice(&[id as f32, pos[0] as f32, pos[1] as f32, pos[2] as f32, yaw, pitch, limb, amt, swing, flags as f32, r.body_at(a), r.age as f32 + a, r.skin as f32]);
        }
        v
    }

    /// Своя модель для вида от третьего лица (G16): 13 чисел, как одна запись player_draws (номер — свой); мёртвый или
    /// до входа — пусто.
    pub(crate) fn self_draw(&self, alpha: f64) -> Vec<f32> {
        if !self.joined || self.vit.dead {
            return Vec::new();
        }
        let p = &self.ctl.player;
        let mut flags = 0;
        for (on, f) in [(p.on_ground, PM_GROUND), (p.crouch, PM_CROUCH), (p.sprinting, PM_SPRINT), (p.flying, PM_FLY)] {
            if on {
                flags |= f;
            }
        }
        let skin = self.skins.get(&self.me).copied().unwrap_or(0);
        self.self_body.draw(p, alpha, self.me, flags, skin).to_vec()
    }

    /// Камера за плечом (G16, game/third.rs camera_clip): на сколько можно отъехать от глаз eye по единичному dir, не
    /// дальше max, не заходя в блоки.
    pub(crate) fn camera_clip(&self, eye: [f64; 3], dir: [f64; 3], max: f64) -> f64 {
        crate::game::third::camera_clip(&mut *self.world.d.borrow_mut(), eye, dir, max)
    }

    /// Свободна ли линия a → b: на пути нет сплошных кубов (земля, камень, стены). Листва, стекло, вода и растения не
    /// мешают. Подписи ников (web/net.js) не показываются сквозь стены. Клетка a не считается (камера может быть в
    /// блоке), клетка b — да (точка внутри блока не видна). Обход клеток — Amanatides–Woo.
    pub(crate) fn line_clear(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        Self::line_clear_in(&mut self.world.d.borrow_mut(), a, b)
    }

    /// То же в мире w без клиента: удар по игроку на сервере (game/server/pvp.rs) проверяет видимость цели.
    pub(crate) fn line_clear_in(w: &mut WorldData, a: [f64; 3], b: [f64; 3]) -> bool {
        if !a.iter().chain(b.iter()).all(|v| v.is_finite()) {
            return true;
        }
        let cell = |p: [f64; 3]| [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
        let (mut c, end) = (cell(a), cell(b));
        let d: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
        let step: [i32; 3] = d.map(|v| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 });
        // Доля отрезка до первой границы клетки и приращение на клетку.
        let (mut tm, mut td) = ([f64::MAX; 3], [f64::MAX; 3]);
        for k in 0..3 {
            if step[k] != 0 {
                let next = if step[k] > 0 { c[k] as f64 + 1.0 } else { c[k] as f64 };
                tm[k] = (next - a[k]) / d[k];
                td[k] = step[k] as f64 / d[k];
            }
        }
        for _ in 0..1024 {
            if c == end {
                break;
            }
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
            if tm[k] > 1.0 {
                break; // отрезок кончился раньше (округление)
            }
            c[k] += step[k];
            tm[k] += td[k];
            if bdef(w.get_block(c[0], c[1], c[2])).suffocates {
                return false;
            }
        }
        true
    }

    /// Список игроков для Tab (mp/ui.md §1) по нику без учёта регистра: по 4 числа на игрока — номер, пинг (мс; −1 —
    /// ещё не измерен), флаги (1 — это вы), номер скина. Ники в том же порядке — `tab_names`.
    pub(crate) fn tab_list(&self) -> Vec<i32> {
        let mut v = Vec::with_capacity(self.names.len() * 4);
        for id in self.tab_order() {
            let ping = self.pings.get(&id).map_or(-1, |&p| p as i32);
            v.extend_from_slice(&[id as i32, ping, (id == self.me) as i32, self.skins.get(&id).copied().unwrap_or(0) as i32]);
        }
        v
    }
    /// Ники для Tab через «\n», порядок `tab_list`.
    pub(crate) fn tab_names(&self) -> String {
        self.tab_order().iter().map(|id| self.names[id].as_str()).collect::<Vec<_>>().join("\n")
    }
    fn tab_order(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.names.keys().copied().collect();
        ids.sort_by_cached_key(|id| (self.names[id].to_lowercase(), *id));
        ids
    }
    /// Заголовок Tab: имя сервера (локальный — пусто).
    pub(crate) fn tab_header(&self) -> String {
        self.server_name.clone()
    }
    /// Подвал Tab: «Игроков: 7 из 32 · день 12 · ваш пинг 64 мс».
    pub(crate) fn tab_footer(&self) -> String {
        let mut s = format!("Игроков: {}", self.names.len());
        if self.max_players > 0 {
            s += &format!(" из {}", self.max_players);
        }
        s += &format!(" · день {}", self.day + 1);
        if let Some(p) = self.pings.get(&self.me) {
            s += &format!(" · ваш пинг {p} мс");
        }
        s
    }

    /// Сколько строк чата пришло с начала сеанса.
    pub(crate) fn chat_total(&self) -> u32 {
        self.chat_total
    }
    /// Строки чата, пришедшие после первых `seen` (сколько читатель уже видел): «вид|текст» через «\n»; потерянные
    /// (старше CHAT_KEEP) не возвращаются.
    pub(crate) fn chat_since(&self, seen: u32) -> String {
        let new = (self.chat_total.wrapping_sub(seen) as usize).min(self.chat.len());
        self.chat.iter().skip(self.chat.len() - new).map(|l| format!("{}|{}", l.kind, l.text)).collect::<Vec<_>>().join("\n")
    }
    /// Отправить сообщение (текст чистится как на сервере; пустой — ничего).
    pub(crate) fn chat_send(&mut self, text: &str) {
        let text = clean_chat(text);
        if !text.is_empty() {
            self.out.push(C2S::Chat { text });
        }
    }

    /// Подсказка к строке чата (G17, game/commands.rs): text — до курсора. Пусто — не команда; иначе строки через «\n»:
    /// синтаксис, номер символа начала дополняемого слова, затем варианты.
    pub(crate) fn chat_hint(&self, text: &str) -> String {
        let names: Vec<String> = self.tab_order().iter().map(|id| self.names[id].clone()).collect();
        let creative = self.ctl.player.mode == crate::game::rules::GameMode::Creative;
        match crate::game::commands::hint(text, self.level, creative, &names) {
            None => String::new(),
            Some(h) => {
                let mut v = vec![h.usage, h.start.to_string()];
                v.extend(h.list);
                v.join("\n")
            }
        }
    }
    /// Режим игры (0 выживание, 1 творческий, 3 наблюдатель) и права (0 игрок, 1 админ, 2 владелец).
    pub(crate) fn game_mode(&self) -> Vec<u8> {
        vec![self.ctl.player.mode.code(), self.level]
    }

    /// Ник игрока id (вошедшего).
    pub(crate) fn player_nick(&self, id: u32) -> String {
        self.names.get(&id).cloned().unwrap_or_default()
    }

    /// HUD: [выбранный слот, затухание имени (тики), предмет имени, строка действия (тики), код сообщения,
    /// затем 9 ячеек хотбара и вторая рука по 4 числа: предмет, число, износ, подскок; последним — режим игры (G17)].
    pub(crate) fn hud(&self) -> Vec<i32> {
        let mut v = vec![self.ctl.inv.selected as i32, self.view.name_fade, self.view.name_item as i32, self.view.overlay, self.view.overlay_msg];
        for i in (0..9).chain(std::iter::once(OFFHAND)) {
            let s = if i == OFFHAND { self.ctl.inv.offhand } else { self.ctl.inv.main[i] };
            v.extend_from_slice(&[s.id() as i32, s.n(), s.damage as i32, self.ctl.inv.bob[i] as i32]);
        }
        v.push(self.ctl.player.mode.code() as i32);
        v
    }

    /// События с прошлого вызова: по 5 чисел [код EV_*, x, y, z, параметр] — свои предсказанные и присланные сервером.
    pub(crate) fn take_events(&mut self) -> Vec<i32> {
        self.view.events.take()
    }

    /// Время суток для неба: доля суток от полуночи (0.5 — полдень) на доле тика alpha.
    /// На Перекрёстке после кнопки неба (G15) — выбранное время (переход плавный), только у этого клиента.
    pub(crate) fn time_of_day(&self, alpha: f64) -> f64 {
        (time_of_day(self.day, self.day_cycle, alpha) + self.view.sky_shift(alpha)).rem_euclid(1.0)
    }

    /// Отладка: [x, y, z, vx, vy, vz, на земле, fallDistance, бег, полёт, присед, скорость б/с, yaw, pitch,
    /// прогресс ломания, задержка ломания, задержка атаки, задержка ПКМ, цель x, y, z, грань (−1 — нет)].
    pub(crate) fn debug(&self) -> Vec<f64> {
        let p = &self.ctl.player;
        let sp = ((p.pos[0] - p.prev[0]).powi(2) + (p.pos[2] - p.prev[2]).powi(2)).sqrt() * TPS as f64;
        let t = self.ctl.target.map(|h| [h.pos[0] as f64, h.pos[1] as f64, h.pos[2] as f64, h.face as f64]).unwrap_or([0.0, 0.0, 0.0, -1.0]);
        vec![
            p.pos[0], p.pos[1], p.pos[2], p.vel[0], p.vel[1], p.vel[2], p.on_ground as i32 as f64, p.fall as f64,
            p.sprinting as i32 as f64, p.flying as i32 as f64, p.crouch as i32 as f64, sp, p.yaw as f64, p.pitch as f64,
            self.ctl.mine.progress as f64, self.ctl.mine.cooldown as f64, self.ctl.attack_cd as f64, self.ctl.use_cd as f64, t[0], t[1], t[2], t[3],
        ]
    }

    /// Данные отрисовки предметов в мире (ASP-07 §8): по 8 f32 на предмет — x, y, z (низ коробки, интерполяция по
    /// alpha), поворот Y, покачивание Y, номер предмета, число в стеке, свет. Летящие копии подбора — в том же списке.
    pub(crate) fn item_draws(&self, alpha: f64) -> Vec<f32> {
        let p = &self.ctl.player;
        let l = |a: f64, b: f64| a + (b - a) * alpha;
        let feet = [l(p.prev[0], p.pos[0]), l(p.prev[1], p.pos[1]), l(p.prev[2], p.pos[2])];
        let target = [feet[0], feet[1] + p.eye_height() as f64 / 2.0, feet[2]];
        // Чужой подбор: к середине тела подобравшего на этот кадр (ступни по ходам — Remote::at).
        let remote = |id: u32| self.remotes.get(&id).map(|r| r.at(r.t).0).map(|f| [f[0], f[1] + 0.81, f[2]]);
        let mut v = self.items.draws_with(alpha, target, p.eye(alpha), &remote);
        self.transit.draws(alpha, p.eye(alpha), &mut v); // предметы в пути — тем же конвейером (M5)
        v
    }

    /// Открытое окно: 0 — нет, 1 — инвентарь, 2 — верстак, 3 — сундук, 4 — печь.
    pub(crate) fn screen_kind(&self) -> i32 {
        self.ctl.screen.as_ref().map_or(0, |m| screen_code(&m.kind))
    }

    /// Раскладка окна: [вид, ширина, высота, слотов N, затем на слот: x, y (левый верх иконки), вид слота (0 обычный,
    /// 1 результат, 2…5 броня ноги…голова, 6 вторая рука, 7 вход печи, 8 топливо, 9 выход печи), хранилище (0 игрок,
    /// 1 сетка, 2 результат, 3 блок)].
    pub(crate) fn screen_layout(&self) -> Vec<i32> {
        let Some(m) = &self.ctl.screen else { return Vec::new() };
        let mut v = vec![screen_code(&m.kind), m.w, m.h, m.slots.len() as i32];
        for s in &m.slots {
            let kind = match s.kind {
                SlotKind::Plain => SlotView::Plain as i32,
                SlotKind::Result => SlotView::Result as i32,
                SlotKind::Armor(e) => SlotView::ArmorFeet as i32 + e as i32,
                SlotKind::Offhand => SlotView::Offhand as i32,
                SlotKind::FurnaceIn => SlotView::FurnaceIn as i32,
                SlotKind::Fuel => SlotView::Fuel as i32,
                SlotKind::Output => SlotView::Output as i32,
                SlotKind::Mek(r) => mek_slot_view(r),
            };
            let store = match s.store {
                Store::Player(_) => SlotStore::Player as i32,
                Store::Grid(_) => SlotStore::Grid as i32,
                Store::Result => SlotStore::Result as i32,
                Store::Block(_) => SlotStore::Block as i32,
            };
            v.extend_from_slice(&[s.x as i32, s.y as i32, kind, store]);
        }
        v
    }

    /// Содержимое окна: [курсор: предмет, число, износ; затем на слот: предмет, число, износ].
    pub(crate) fn screen_view(&self) -> Vec<i32> {
        let Some(m) = &self.ctl.screen else { return Vec::new() };
        let mut v = vec![m.cursor.id() as i32, m.cursor.n(), m.cursor.damage as i32];
        let mut wd = self.world.d.borrow_mut();
        let pos = m.block_pos();
        for s in &m.slots {
            let st = match s.store {
                Store::Player(k) => self.ctl.inv.get(k as usize),
                Store::Grid(k) => m.grid.cells[k as usize],
                Store::Result => m.result,
                Store::Block(k) => pos.and_then(|p| wd.be_mut().container(p)).map_or(ItemStack::EMPTY, |c| c.get(k as usize)),
            };
            v.extend_from_slice(&[st.id() as i32, st.n(), st.damage as i32]);
        }
        v
    }

    /// Окно печи: [огонёк 0…14 пикселей снизу, стрелка 0…24 слева] (числа печи — Msg::WinData) или пусто.
    pub(crate) fn furnace_view(&self) -> Vec<i32> {
        let Some(MenuKind::Furnace(p)) = self.ctl.screen.as_ref().map(|m| m.kind) else { return Vec::new() };
        let w = self.world.d.borrow();
        w.be().furnace(p).map_or(Vec::new(), |f| vec![f.flame_px(), f.arrow_px()])
    }

    /// Модели блоков меньше куба (web/render/chest.js): по 6 чисел — x, y, z клетки, вид (0 сундук, 1 лестница,
    /// 2 настенный факел, 3 снежный слой), сторона FACING (2 N, 3 S, 4 W, 5 E), параметр (сундук — угол крышки в
    /// радианах на доле тика alpha; снег — слоёв). Из индекса моделей по кускам в квадрате ±r кусков от игрока.
    pub(crate) fn model_draws(&self, alpha: f64, r: i32) -> Vec<f32> {
        let w = self.world.d.borrow();
        let mut v = Vec::new();
        let p = self.ctl.player.pos;
        let (cx, cz) = crate::world::chunk_of(p[0].floor() as i32, p[2].floor() as i32);
        for (p, c) in w.models_near(cx, cz, r) {
            let Some(kind) = model_of(block_of(c)) else { continue };
            let param = match kind {
                Model::Chest => w.be().chest(p).map_or(0.0, |ch| ch.lid.angle(alpha as f32)),
                Model::Snow => snow_layers(c) as f32,
                Model::Ladder | Model::WallTorch => 0.0,
                Model::Mek => self.mek_model_param(&w, p, c),
                Model::Cable => self.cable_model_param(p, c),
            };
            v.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, kind as i32 as f32, facing_of(c) as f32, param]);
        }
        v
    }

    /// Падающие блоки: по 4 числа — x, y (низ), z (интерполяция по alpha), номер блока.
    pub(crate) fn falling_draws(&self, alpha: f64) -> Vec<f32> {
        let mut v = Vec::with_capacity(self.falling.len() * 4);
        for f in &self.falling {
            let l = |k: usize| (f.prev[k] + (f.pos[k] - f.prev[k]) * alpha) as f32;
            v.extend_from_slice(&[l(0), l(1), l(2), block_of(f.cell) as f32]);
        }
        v
    }
}

/// Итог движения игрока p (proto Move) с удержаниями keys: тик клиента и Resume сервера.
pub(crate) fn move_of(p: &Player, keys: u32) -> Move {
    let mut flags = 0;
    for (on, f) in [(p.on_ground, MV_GROUND), (p.sprinting, MV_SPRINT), (p.crouch, MV_CROUCH), (p.flying, MV_FLY), (p.sneak_in, MV_SNEAK), (p.h_coll, MV_HCOLL), (p.soft, MV_SOFT), (p.noclip, MV_NOCLIP)] {
        if on {
            flags |= f;
        }
    }
    Move {
        pos: p.pos,
        vel: p.vel,
        yaw: p.yaw,
        pitch: p.pitch,
        fall: p.fall,
        keys: keys as u16,
        flags,
        jump_cd: p.jump_cd.clamp(0, 255) as u8,
        dtap: p.dtap.clamp(0, 255) as u8,
        fwd_in: p.fwd_in,
        support: p.support,
    }
}

/// Удержания маски K_* → клавиши движения.
pub(crate) fn keys_of(mask: u32) -> Keys {
    let h = |k: u32| mask & k != 0;
    Keys { fwd: h(K_FWD), back: h(K_BACK), left: h(K_LEFT), right: h(K_RIGHT), jump: h(K_JUMP), sneak: h(K_SNEAK), sprint: h(K_SPRINT) }
}

/// Хеш инвентаря (41 ячейка, без выбранной и подскока): сверка предсказания с сервером (FNV-1a, 32 бита).
pub(crate) fn inv_hash(inv: &Inventory) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for i in 0..=OFFHAND {
        let s = inv.get(i);
        for x in [s.id() as u32, s.n() as u32, if s.is_empty() { 0 } else { s.damage as u32 }] {
            for b in x.to_le_bytes() {
                h = (h ^ b as u32).wrapping_mul(0x0100_0193);
            }
        }
    }
    h
}

/// Клиент с трубой: принять кадр сервера (байты), отдать накопленные кадры (байты). Разбор и сборка — proto.rs.
pub(crate) struct Client {
    pub(crate) sim: ClientSim,
    pub(crate) bytes_up: u64,
    pub(crate) bytes_down: u64,
}

impl Client {
    pub(crate) fn new(world: &World, nick: &str) -> Client {
        Client::with_token(world, nick, [0; 16])
    }
    pub(crate) fn with_token(world: &World, nick: &str, token: [u8; 16]) -> Client {
        let mut sim = ClientSim::new(world);
        sim.hello(nick, token);
        Client { sim, bytes_up: 0, bytes_down: 0 }
    }
    pub(crate) fn recv(&mut self, b: &[u8]) -> Result<(), ProtoError> {
        self.bytes_down += b.len() as u64;
        let m: S2C = decode(b)?;
        self.sim.apply(m);
        Ok(())
    }
    pub(crate) fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        let v: Vec<Vec<u8>> = self.sim.out.drain(..).map(|m| encode(&m)).collect();
        self.bytes_up += v.iter().map(|b| b.len() as u64).sum::<u64>();
        v
    }
}

#[cfg(test)]
mod remote_body_tests {
    use super::*;

    /// Игрок появился лицом на yaw 0 и идёт по ходу (dx, dz) блоков за тик, взгляд yaw°: за n тиков клиента.
    fn walk(dx: f32, dz: f32, yaw: f32, n: u32) -> Remote {
        let mv = |k: u32| PlayerMove { id: 1, n: k, pos: [dx * k as f32, 70.0, dz * k as f32], yaw: if k == 0 { 0.0 } else { yaw }, pitch: 0.0, flags: 1 };
        let mut r = Remote::new("Аня".into(), 0, mv(0));
        for k in 1..=n + 6 {
            r.push(mv(k));
        }
        for _ in 0..n {
            r.advance();
        }
        r
    }
    fn near(a: f32, b: f32) -> bool {
        wrap_deg(a - b).abs() < 1.0
    }

    #[test]
    fn body_follows_walk_direction() {
        // Вперёд (по взгляду, yaw 0 = +Z): тело на месте.
        assert!(near(walk(0.0, 0.2, 0.0, 30).body, 0.0));
        // Влево-вбок по +X при взгляде на +Z: тело тянется к направлению хода, но голова от тела не дальше 75°.
        // Равновесие MC: тянет к −90° на 0.3 от разницы, голова тянет обратно на 0.2 от угла сверх 50° — около −48°.
        let b = walk(0.2, 0.0, 0.0, 60).body;
        assert!(b < -40.0 && b > -75.0, "{b}");
        // Пятится (идёт по −Z, глядя на +Z): тело остаётся лицом по взгляду, а не разворачивается спиной.
        assert!(near(walk(0.0, -0.2, 0.0, 30).body, 0.0));
    }

    #[test]
    fn standing_body_follows_head_past_50_degrees() {
        // Стоит, голова ушла на 60°: тело подтягивается, пока разница не станет ≤ 50°.
        let r = walk(0.0, 0.0, 60.0, 40);
        let g = wrap_deg(60.0 - r.body);
        assert!(g.abs() <= 50.5, "разница {g}");
        assert!(g.abs() > 40.0, "тело не бежит за головой сразу: {g}");
        assert!(r.body > 5.0, "тело всё же подтянулось: {}", r.body);
        // Голова на 120°: сразу не дальше 75° (тело сдвинулось), потом дотягивается.
        let r = walk(0.0, 0.0, 120.0, 1);
        assert!(wrap_deg(120.0 - r.body).abs() <= 75.0 + 1e-3, "{}", r.body);
    }

    #[test]
    fn swing_turns_body_to_gaze() {
        let mut r = walk(0.0, 0.0, 0.0, 5);
        r.body = -40.0;
        r.body_prev = -40.0;
        r.swing = 0;
        r.advance();
        assert!(r.body > -40.0 + 5.0, "взмах — тело к взгляду: {}", r.body);
    }

    #[test]
    fn body_interpolates_short_arc() {
        let mut r = walk(0.0, 0.0, 0.0, 3);
        r.body_prev = 170.0;
        r.body = -170.0;
        assert!(near(r.body_at(0.5), 180.0));
        assert!(near(r.body_at(0.0), 170.0) && near(r.body_at(1.0), -170.0));
    }
}
