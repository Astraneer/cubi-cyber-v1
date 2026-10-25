//! Игра (E3): состояние, которое меняется только в тике 20 Гц. Кадр на стороне JS (web/game/loop.js): ввод → столько
//! тиков, сколько накопилось (не больше 10) → поворот мышью (game.turn) → отрисовка с долей alpha между прошлым и
//! текущим тиком (game.view). Порядок фаз тика — ASP-05 §11.1 (согласован с ASP-04, ASP-01 §2.2): ввод и
//! использование (луч, 1–9, E, Q, ЛКМ, ПКМ, СКМ, удержание, ломание) → камера (FOV, высота глаз — по концу прошлого
//! тика) → игрок → доступность открытого окна (ASP-09 §1.6: верстак, сундук, печь дальше 8.5 или пропали — закрыть) →
//! мир (время, запланированные и случайные тики блоков, сущности: предметы и падающие блоки, блок-сущности — печи; всё
//! в активной зоне) → подбор (ASP-07 §5.6: отдельной фазой после сущностей) → «клиент»: шаг крышек сундуков и
//! случайная выборка блоков вокруг игрока (огоньки и треск печи, ASP-10 §5).
//!
//! Сеть (MP-0, design.md §2.2, §5): игра разделена на сервер и клиент.
//!   server.rs — ServerSim: симуляция мира (sim.rs: мир, время, сущности, блок-сущности, очередь тиков, ГСЧ), игроки
//!               (теневое тело, инвентарь, окно), решение по действиям (interact.rs в режиме Auth), кадры клиентам;
//!   client.rs — ClientSim: свой игрок и управление (ввод, ломание, предсказание в режиме Predict), представление
//!               (view.rs, hand.rs: рука, HUD, события, ГСЧ эффектов), зеркало мира и сущностей;
//!   proto.rs  — сообщения между ними (postcard).
//! Game ниже — пара Client + LocalServer в одном потоке (браузер без ?server=, проверки): тик — тик клиента →
//! кадр серверу → тик сервера → кадр клиенту → «клиентская» фаза; всё через сериализацию, как по сети. Это путь
//! разработки, а не одиночная игра (игра только сетевая). С сервером (go_online, web/net.js) — только клиент: кадры
//! уходят и приходят через WebSocket (recv / take_frame). Окна и колесо — очередью до границы тика.

pub(crate) mod blockticks;
pub(crate) mod bucket;
pub(crate) mod commands;
pub(crate) mod client;
pub(crate) mod crafting;
pub(crate) mod digest;
pub(crate) mod entity;
pub(crate) mod farm;
pub(crate) mod equip;
pub(crate) mod fluids;
pub(crate) mod hand;
pub(crate) mod hazards;
pub(crate) mod health;
pub(crate) mod interact;
pub(crate) mod inventory;
pub(crate) mod mek_client;
pub(crate) mod mek_demo;
pub(crate) mod mek_tool;
pub(crate) mod mob;
pub(crate) mod nei;
pub(crate) mod physics;
pub(crate) mod player;
pub(crate) mod pvp;
pub(crate) mod rules;
pub(crate) mod screen;
pub(crate) mod server;
pub(crate) mod showcase;
pub(crate) mod sim;
pub(crate) mod sound;
pub(crate) mod social;
pub(crate) mod third;
pub(crate) mod view;
#[cfg(test)]
mod admin_tests;
#[cfg(test)]
mod crossroads_tests;
#[cfg(test)]
mod fluid_tests;
#[cfg(test)]
mod flight_tests;
#[cfg(test)]
mod g7_tests;
#[cfg(test)]
mod net_tests;
#[cfg(test)]
mod boxes_tests;
#[cfg(test)]
mod persist_tests;
#[cfg(test)]
mod slice3_tests;
#[cfg(test)]
mod survival_tests;
#[cfg(test)]
mod pvp_tests;
#[cfg(test)]
mod mek_tests;
#[cfg(test)]
mod mek3_tests;
#[cfg(test)]
mod showcase_tests;
#[cfg(test)]
mod mek4_tests;
#[cfg(test)]
mod mek5_tests;

use crate::abi::ScreenKind;
use crate::cell::{Cell, H, SEA};
use crate::registry::blocks::bdef;
use crate::registry::items::{idef, ItemId, ItemKind, I_CHARCOAL, I_COAL, I_IRON_INGOT, I_LEATHER_HELMET, I_RAW_IRON, I_WOODEN_PICKAXE};
use crate::registry::ids::*;
use crate::registry::shapes::Aabb;
use crate::world::stack::ItemStack;
use crate::world::{World, WorldData};
use client::Client;
use inventory::Inventory;
use physics::space_empty;
use player::Player;
use screen::{Menu, MenuKind, Ui};
use server::{LocalServer, ServerSim};

/// Тиков в секунду (Minecraft: 20, тик — 50 мс).
pub(crate) const TPS: u32 = 20;

/// Детерминированный ГСЧ (splitmix64) — тесты с фиксированным сидом. У игры их три: дроп и сущности, тики блоков,
/// эффекты (звуки и частицы не сдвигают эталоны дропа — решение после ASP-13).
pub(crate) struct GameRng(pub(crate) u64);
impl GameRng {
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    pub(crate) fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16777216.0
    }
    pub(crate) fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9007199254740992.0
    }
    /// Равновероятно от lo до hi включительно.
    pub(crate) fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i32
    }
    /// Random.nextInt(n): 0…n−1.
    pub(crate) fn below(&mut self, n: i32) -> i32 {
        (self.next_u64() % n as u64) as i32
    }
}

/// Номера сущностей (Entity.getId; ревью P2 «Сеть»): один счётчик на игроков, предметы, падающие блоки (дальше — мобов)
/// — номер в протоколе однозначен. У предмета он же — фаза «ход раз в 4 тика». 0 — «нет номера».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ids(pub(crate) u32);
impl Ids {
    pub(crate) fn next(&mut self) -> u32 {
        let id = self.0.max(1);
        self.0 = id.wrapping_add(1);
        id
    }
}

/// Место появления (cubi FEEL-005: проверять всё тело): ближайший к (x0, z0) столбец суши, где коробка тела свободна.
pub(crate) fn find_spawn(w: &mut WorldData, x0: i32, z0: i32) -> [f64; 3] {
    let mut buf = Vec::new();
    for r in 0..=32i32 {
        for dz in -r..=r {
            for dx in -r..=r {
                if dx.abs().max(dz.abs()) != r {
                    continue;
                }
                let (x, z) = (x0 + dx, z0 + dz);
                let h = w.gen().column(x, z).h;
                if h < SEA || h + 2 >= H {
                    continue;
                }
                let p = [x as f64 + 0.5, (h + 1) as f64, z as f64 + 0.5];
                let body = Aabb { lo: [p[0] - 0.3, p[1], p[2] - 0.3], hi: [p[0] + 0.3, p[1] + 1.8, p[2] + 0.3] };
                if bdef(w.get_block(x, h, z)).collision.is_empty() || !space_empty(w, &body, &mut buf) {
                    continue;
                }
                return p;
            }
        }
    }
    [x0 as f64 + 0.5, (w.gen().column(x0, z0).h.max(SEA) + 1) as f64, z0 as f64 + 0.5]
}

/// Точка появления на Перекрёстке (mp/spawn.md §6) — вход нового игрока и возрождение после смерти: юго-западная часть
/// площади (угол 15…55° от юга к западу, r 10…15), под ногами пол, тело помещается, до тел others по горизонтали не
/// меньше 1.5 (игроки не появляются друг в друге). n — номер попытки: разные n дают разные точки (сервер берёт номер
/// тика и число игроков). Ответ — ступни и yaw: взгляд на SPAWN_LOOK — с первого кадра справа Маяк, слева галерея витрины
/// Mekanism, впереди площадь (G13: раньше — лицом в стену Маяка с доской).
pub(crate) const SPAWN_LOOK: (f64, f64) = (-8.0, -8.0);
pub(crate) fn crossroads_spawn(w: &mut WorldData, n: u64, others: &[[f64; 3]]) -> ([f64; 3], f32) {
    let (sx, sz, y0) = {
        let s = &w.gen().site;
        (s.x, s.z, s.y0)
    };
    let mut rng = GameRng(n ^ 0x5EED_C0DE);
    let mut buf = Vec::new();
    for _ in 0..64 {
        let a = -(15.0 + rng.next_f64() * 40.0).to_radians();
        let r = 10.0 + rng.next_f64() * 5.0;
        let (x, z) = ((sx as f64 + 0.5 + r * libm::sin(a)).floor(), (sz as f64 + 0.5 + r * libm::cos(a)).floor());
        let p = [x + 0.5, (y0 + 1) as f64, z + 0.5];
        let body = Aabb { lo: [p[0] - 0.3, p[1], p[2] - 0.3], hi: [p[0] + 0.3, p[1] + 1.8, p[2] + 0.3] };
        let near = others.iter().any(|o| (o[0] - p[0]).powi(2) + (o[2] - p[2]).powi(2) < 1.5 * 1.5 && (o[1] - p[1]).abs() < 2.0);
        if near || bdef(w.get_block(x as i32, y0, z as i32)).collision.is_empty() || !space_empty(w, &body, &mut buf) {
            continue;
        }
        return (p, look_yaw(p, sx, sz));
    }
    let p = find_spawn(w, sx, sz + 12);
    (p, look_yaw(p, sx, sz))
}

/// Yaw (градусы Minecraft: взгляд (−sin, cos)) из точки p на SPAWN_LOOK от центра площади (sx, sz).
fn look_yaw(p: [f64; 3], sx: i32, sz: i32) -> f32 {
    let (dx, dz) = (sx as f64 + 0.5 + SPAWN_LOOK.0 - p[0], sz as f64 + 0.5 + SPAWN_LOOK.1 - p[2]);
    let yaw = libm::atan2(-dx, dz).to_degrees();
    (if yaw < 0.0 { yaw + 360.0 } else { yaw }) as f32
}

/// Поставить тело в точку появления: на земле, скорость покоя.
pub(crate) fn place_at_spawn(p: &mut Player, at: [f64; 3]) {
    p.teleport(at);
    p.on_ground = true;
    p.vel = [0.0, -0.0784, 0.0];
}

/// Положить предмет в инвентарь по правилам подбора. Ответ — сколько не влезло.
pub(crate) fn give_into(inv: &mut Inventory, item: ItemId, count: i32) -> i32 {
    if idef(item).kind == ItemKind::None {
        return count;
    }
    let mut s = ItemStack::of(item, count);
    inv.insert(&mut s);
    s.n()
}

/// Отладка (клавиша K): набор для приёмки ASP-08/09/10 и ASP-05 — брёвна трёх пород, булыжник, уголь, слитки,
/// земля, две деревянные кирки (целая и изношенная), железный шлем; печь, сундуки, факелы, лестницы, песок, гравий,
/// сырое железо, снег.
pub(crate) fn kit_into(inv: &mut Inventory) {
    let items = [
        (LOG as ItemId, 12), (BIRCH_LOG as ItemId, 2), (SPRUCE_LOG as ItemId, 2), (COBBLE as ItemId, 20), (I_COAL, 3), (I_CHARCOAL, 1),
        (I_IRON_INGOT, 12), (DIRT as ItemId, 64), (FURNACE as ItemId, 1), (CHEST as ItemId, 2), (TORCH as ItemId, 16), (LADDER as ItemId, 16),
        (SAND as ItemId, 16), (GRAVEL as ItemId, 8), (I_RAW_IRON, 8), (SNOW_LAYER as ItemId, 12),
    ];
    for (item, n) in items {
        give_into(inv, item, n);
    }
    let mut worn = ItemStack { item: I_WOODEN_PICKAXE, count: 1, damage: 30 };
    inv.insert(&mut worn);
    give_into(inv, I_WOODEN_PICKAXE, 1);
    give_into(inv, I_LEATHER_HELMET + 4, 1);
}

/// Хеш окна (курсор и все слоты, FNV-1a 32 бита — design.md §4.4): клиент шлёт свой после предсказания клика, сервер
/// сверяет со своим.
pub(crate) fn menu_hash(m: &Menu, u: &Ui) -> u32 {
    let mut h = 0x811c_9dc5u32;
    let mut eat = |s: ItemStack| {
        for x in [s.id() as u32, s.n() as u32, if s.is_empty() { 0 } else { s.damage as u32 }] {
            for b in x.to_le_bytes() {
                h = (h ^ b as u32).wrapping_mul(0x0100_0193);
            }
        }
    };
    eat(m.cursor);
    for i in 0..m.slots.len() {
        eat(m.get(u, i));
    }
    h
}

/// Код вида окна для JS: 1 — инвентарь, 2 — верстак, 3 — сундук, 4 — печь.
pub(crate) fn screen_code(k: &MenuKind) -> i32 {
    (match k {
        MenuKind::Player => ScreenKind::Inventory,
        MenuKind::Crafting(_) => ScreenKind::Crafting,
        MenuKind::Chest(_) => ScreenKind::Chest,
        MenuKind::Furnace(_) => ScreenKind::Furnace,
        MenuKind::Mek(..) => ScreenKind::Machine,
    }) as i32
}

/// Игра в браузере (и в проверках): клиент и локальный сервер в одном потоке, связанные петлёй с сериализацией, — или
/// (go_online) клиент сетевого сервера. API для рендера и JS — прежнее: данные кадра — у клиента, отладочные счётчики
/// мира — у локального сервера.
pub struct Game {
    pub(crate) client: Client,
    pub(crate) server: LocalServer,
    /// Клиент сетевого сервера: локальный сервер не используется, кадры — recv / take_frame.
    online: bool,
    outq: std::collections::VecDeque<Vec<u8>>,
    /// Токен игрока (128 бит из localStorage, web/session.js): уходит в Hello при go_online; нули — гость.
    token: [u8; 16],
}

/// Ник игрока локального сервера.
pub(crate) const LOCAL_NICK: &str = "Игрок";

impl Game {
    /// Клиент над миром world и локальный сервер с копией его правок; сырая запись в world (отладка движка) идёт и в
    /// мир сервера. Игрок встаёт у (8, 8).
    pub fn new(world: &World) -> Game {
        Game::with_nick(world, LOCAL_NICK)
    }
    pub(crate) fn with_nick(world: &World, nick: &str) -> Game {
        let server = LocalServer::new(ServerSim::from_world(world));
        world.set_mirror(&server.sim.sim.world);
        Game::join(server, world, nick)
    }
    /// Готовый сервер (из хранилища) и новый клиент: мир клиента — генератор и правки, присланные при входе. Сервер
    /// локальный — отладка разрешена.
    pub(crate) fn from_server(mut sim: ServerSim, nick: &str) -> Game {
        sim.debug = true;
        sim.edit_budget = usize::MAX; // труба в памяти: правки входа сразу, без бюджета сети
        let world = World::new(sim.sim.world.seed());
        world.set_mirror(&sim.sim.world);
        Game::join(LocalServer::new(sim), &world, nick)
    }
    fn join(server: LocalServer, world: &World, nick: &str) -> Game {
        let mut g = Game { client: Client::new(world, nick), server, online: false, outq: Default::default(), token: [0; 16] };
        g.pump();
        g
    }

    /// Перейти к сетевому серверу (web/net.js, ?server=): локальный сервер — прочь (сырая запись мира больше не
    /// зеркалится), клиент — заново с ником nick, Hello — в очереди кадров (take_frame).
    pub fn go_online(&mut self, nick: &str) {
        let world = self.client.sim.world.share();
        world.clear_mirror();
        self.server = LocalServer::new(ServerSim::new(World::new(world.seed())));
        self.client = Client::with_token(&world, nick, self.token);
        self.online = true;
        self.outq.clear();
    }
    /// Токен для следующего go_online (16 байт; другая длина — гость). Ставится до go_online, переживает переподключение.
    pub fn set_token(&mut self, token: &[u8]) {
        self.token = token.try_into().unwrap_or([0; 16]);
    }
    /// Код отказа во входе: пусто — не отказали; «outdated» (обновите страницу), «bad_token» (ник чужой), «nick_taken»,
    /// «bad_nick», «full», «banned», «gen_mismatch».
    pub fn net_reject(&self) -> String {
        use crate::proto::Reject;
        match self.client.sim.stats.reject {
            None => "",
            Some(Reject::Outdated) => "outdated",
            Some(Reject::NickTaken) => "nick_taken",
            Some(Reject::BadNick) => "bad_nick",
            Some(Reject::Full) => "full",
            Some(Reject::Banned) => "banned",
            Some(Reject::GenMismatch) => "gen_mismatch",
            Some(Reject::BadToken) => "bad_token",
        }
        .into()
    }
    pub fn online(&self) -> bool {
        self.online
    }
    /// Перезаход после обрыва (web/net.js): мир клиента — без правок прошлого сеанса (сервер пришлёт нужные заново,
    /// меши — на перестройку), клиент — новый с тем же ником, Hello — в очереди кадров.
    pub fn reconnect(&mut self, nick: &str) {
        let world = self.client.sim.world.share();
        let chunks: Vec<(i32, i32)> = world.d.borrow().edited_chunks().collect();
        for (cx, cz) in chunks {
            world.d.borrow_mut().forget_chunk(cx, cz);
        }
        self.client = Client::with_token(&world, nick, self.token); // тот же токен: иначе «ник занят» после обрыва
        self.outq.clear();
    }
    /// Радиус правок мира в кусках — «блоки до» из настроек (web/net.js): при входе и при смене; сервер ограничивает.
    pub fn set_view(&mut self, radius: u8) {
        self.client.sim.set_view(radius);
    }
    /// Вкладка скрыта или кадры меню заморожены (on) / игрок вернулся (!on): сервер тикает его сам; после возврата
    /// тики стоят до ответа сервера (Resume). До входа — ничего.
    pub fn set_away(&mut self, on: bool) {
        self.client.sim.set_away(on);
    }
    /// Кадр сетевого сервера; ошибка разбора — текст.
    pub fn recv(&mut self, b: &[u8]) -> Result<(), String> {
        self.client.recv(b).map_err(|e| e.to_string())
    }
    /// Следующий кадр серверу (None — нет).
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.outq.is_empty() {
            self.outq.extend(self.client.take_outbox());
        }
        self.outq.pop_front()
    }
    /// Состояние входа: пусто — играем; иначе текст («вход…», «правки…», отказ).
    pub fn net_status(&self) -> String {
        let c = &self.client.sim;
        if let Some(r) = &c.stats.rejected {
            return format!("отказ: {r}");
        }
        match (c.joined, c.ready()) {
            (false, _) => "вход…".into(),
            (true, false) => "загрузка правок…".into(),
            _ => String::new(),
        }
    }

    /// Один игровой тик: тик клиента (ввод, игрок) → кадр серверу → тик сервера (действия, теневой тик, мир, подбор) →
    /// кадр клиенту (правки, сущности, окна, события этого тика) → «клиентская» фаза (крышки, выборка вокруг игрока).
    /// По сети — только клиент: кадры сервера приходят между кадрами (recv).
    pub fn tick(&mut self) {
        self.client.sim.tick();
        if self.online {
            self.client.sim.tick_visual();
            return;
        }
        self.pump_up();
        self.server.tick();
        self.pump_down();
        self.client.sim.tick_visual();
    }

    fn pump_up(&mut self) {
        for b in self.client.take_outbox() {
            self.server.recv(&b).expect("локальный сервер: кадр клиента");
        }
    }
    fn pump_down(&mut self) {
        for b in self.server.take_outbox() {
            self.client.recv(&b).expect("клиент: кадр локального сервера");
        }
    }
    fn pump(&mut self) {
        self.pump_up();
        self.pump_down();
    }

    /// Между тиками: действия клиента из очереди — серверу сразу (C2S::Acts), затем всё, что сервер изменил, — клиенту.
    /// Для отладки и проверок, которые ставят мир сервера напрямую: порядок с действиями клиента сохраняется.
    pub(crate) fn sync(&mut self) {
        if self.online {
            return;
        }
        let c = &mut self.client.sim;
        c.sync_sel();
        if !c.acts.is_empty() {
            let acts = std::mem::take(&mut c.acts);
            c.out.push(crate::proto::C2S::Acts(acts));
        }
        self.pump_up();
        self.server.flush();
        self.pump_down();
    }

    // ------------------------------------------------------------ ввод и отладка (клиент; отладка — командами Debug)

    pub fn spawn(&mut self, x: f64, z: f64) {
        self.client.sim.spawn(x, z)
    }
    pub fn teleport(&mut self, x: f64, y: f64, z: f64) {
        self.client.sim.teleport(x, y, z)
    }
    pub fn set_look(&mut self, yaw: f32, pitch: f32) {
        self.client.sim.set_look(yaw, pitch)
    }
    /// Отладочный полёт сквозь стены (?fly, сцены, стенд, клавиша отладки) — только с отладкой сервера.
    pub fn set_flying(&mut self, on: bool) {
        self.client.sim.set_flying(on)
    }
    /// Идёт отладочный полёт сквозь стены (креатив: HUD здоровья скрыт).
    pub fn flying(&self) -> bool {
        self.client.sim.ctl.player.noclip
    }
    /// Идёт полёт выживания (F, G16) или отладочный.
    pub fn flight(&self) -> bool {
        self.client.sim.ctl.player.flying
    }
    /// Своя модель для вида от третьего лица (G16): 13 чисел, как одна запись player_draws.
    pub fn self_draw(&self, alpha: f64) -> Vec<f32> {
        self.client.sim.self_draw(alpha)
    }
    /// Камера за плечом: на сколько можно отъехать от глаз (x, y, z) по единичному (dx, dy, dz), не дальше max.
    pub fn camera_clip(&self, x: f64, y: f64, z: f64, dx: f64, dy: f64, dz: f64, max: f64) -> f64 {
        self.client.sim.camera_clip([x, y, z], [dx, dy, dz], max)
    }
    pub fn set_plant_offset(&mut self, on: bool) {
        self.client.sim.set_plant_offset(on)
    }
    pub fn set_keys(&mut self, mask: u32) {
        self.client.sim.set_keys(mask)
    }
    pub fn press(&mut self, code: u32) {
        self.client.sim.press(code)
    }
    pub fn release_all(&mut self) {
        self.client.sim.release_all()
    }
    pub fn grab(&mut self) {
        self.client.sim.grab()
    }
    pub fn scroll(&mut self, amount: f64) {
        self.client.sim.scroll(amount)
    }
    pub fn turn(&mut self, dx: f64, dy: f64, sens: f64) {
        self.client.sim.turn(dx, dy, sens)
    }
    pub fn give(&mut self, item: ItemId, count: i32) -> i32 {
        self.client.sim.give(item, count)
    }
    pub fn kit(&mut self) {
        self.client.sim.kit()
    }
    /// Площадка Mekanism у ног (отладка, сцены mek_*): 1 — «сталь» (лава, набор), 2 — «линия» (всё стоит).
    pub fn mek_build(&mut self, kind: u8) {
        self.client.sim.debug_build(kind)
    }

    pub fn clear_inventory(&mut self) {
        self.client.sim.clear_inventory()
    }
    pub fn debug_shift(&mut self, dx: f64, dy: f64, dz: f64) {
        self.client.sim.debug_shift(dx, dy, dz)
    }
    pub fn set_time_of_day(&mut self, f: f64) {
        self.client.sim.set_time_of_day(f)
    }
    pub fn set_day_cycle(&mut self, on: bool) {
        self.client.sim.set_day_cycle(on)
    }

    // ------------------------------------------------------------ данные кадра (клиент)

    pub fn ticks(&self) -> f64 {
        self.client.sim.ticks()
    }
    pub fn time_of_day(&self, alpha: f64) -> f64 {
        self.client.sim.time_of_day(alpha)
    }
    pub fn view(&self, alpha: f64) -> Vec<f64> {
        self.client.sim.view(alpha)
    }
    pub fn outline(&self, alpha: f64) -> Vec<f64> {
        self.client.sim.outline(alpha)
    }
    pub fn crack(&self) -> Vec<i32> {
        self.client.sim.crack()
    }
    pub fn hud(&self) -> Vec<i32> {
        self.client.sim.hud()
    }
    /// Здоровье, воздух, броня, горение, смерть (game/client.rs vitals): для сердец, пузырей и экрана смерти.
    pub fn vitals(&self) -> Vec<f64> {
        self.client.sim.vitals()
    }
    /// Экран смерти: «Возродиться» (активна с 20-го тика смерти).
    pub fn respawn(&mut self) {
        self.client.sim.respawn()
    }
    /// Отладка: урон N обычным путём сервера.
    pub fn debug_hurt(&mut self, amount: f32) {
        self.client.sim.debug_hurt(amount)
    }
    /// Отладка: поставить здоровье.
    pub fn debug_tick_speed(&mut self, n: u32) {
        self.client.sim.debug_tick_speed(n)
    }
    pub fn debug_set_hp(&mut self, hp: f32) {
        self.client.sim.debug_set_hp(hp)
    }
    /// Отладка: n мобов вида kind (0 свинья, 1 корова, 2 овца, 3 курица, 4 зомби) у ступней (x, y, z).
    pub fn debug_spawn_mob(&mut self, kind: u8, x: f64, y: f64, z: f64, n: u8) {
        self.client.sim.debug_spawn_mob(kind, [x, y, z], n);
        self.sync();
    }
    /// Мобы в кадре (game/mob/view.rs): по 16 чисел на моба — номер, вид, ступни x y z, тело°, голова от тела°, тангаж°,
    /// фаза и размах ног, красный 0/1, крен смерти°, флаги MM_*, вариант (цвет овцы), возраст, доп. угол.
    pub fn mob_draws(&self, alpha: f64) -> Vec<f32> {
        self.client.sim.mob_draws(alpha)
    }
    /// Мобы локального сервера: [всего, монстров, поисков пути, раскрытий A*, появилось, исчезло] (отладка, проверки).
    pub fn mob_stats(&self) -> Vec<f64> {
        let m = &self.server.sim.sim.mobs;
        let st = &m.stats;
        vec![m.list.len() as f64, m.count(true) as f64, st.paths as f64, st.path_nodes as f64, st.spawned as f64, st.despawned as f64]
    }
    pub fn take_events(&mut self) -> Vec<i32> {
        self.client.sim.take_events()
    }
    pub fn debug(&self) -> Vec<f64> {
        self.client.sim.debug()
    }
    pub fn item_draws(&self, alpha: f64) -> Vec<f32> {
        self.client.sim.item_draws(alpha)
    }
    /// Подсветка граней машин конфигуратором: по 5 чисел [x, y, z, грань, цвет] (game/mek_client.rs).
    pub fn mek_sides_overlay(&self) -> Vec<f32> {
        self.client.sim.mek_sides_overlay()
    }
    pub fn model_draws(&self, alpha: f64, r: i32) -> Vec<f32> {
        self.client.sim.model_draws(alpha, r)
    }
    pub fn falling_draws(&self, alpha: f64) -> Vec<f32> {
        self.client.sim.falling_draws(alpha)
    }
    pub fn hand_pose(&self, alpha: f64, bob: bool) -> Vec<f32> {
        self.client.sim.hand_pose(alpha, bob)
    }
    pub fn hand_debug_eat(&mut self, on: bool) {
        self.client.sim.hand_debug_eat(on)
    }
    pub fn hand_debug(&self) -> Vec<f32> {
        self.client.sim.hand_debug()
    }

    // ------------------------------------------------------------ окно (предсказание у клиента, серверу — очередью)

    pub fn screen_kind(&self) -> i32 {
        self.client.sim.screen_kind()
    }
    pub fn screen_layout(&self) -> Vec<i32> {
        self.client.sim.screen_layout()
    }
    pub fn screen_view(&self) -> Vec<i32> {
        self.client.sim.screen_view()
    }
    pub fn screen_click(&mut self, slot: i32, button: i32, action: i32) {
        self.client.sim.screen_click(slot, button, action)
    }
    pub fn screen_quick_move_all(&mut self, slot: i32, item: ItemId, damage: u16) {
        self.client.sim.screen_quick_move_all(slot, item, damage)
    }
    pub fn screen_drag_accepts(&mut self, slot: i32, mode: u8, n: u32) -> bool {
        self.client.sim.screen_drag_accepts(slot, mode, n)
    }
    pub fn screen_drag_preview(&mut self, mode: u8, slots: &[i32]) -> Vec<i32> {
        self.client.sim.screen_drag_preview(mode, slots)
    }
    pub fn open_inventory(&mut self) {
        self.client.sim.open_inventory()
    }
    pub fn close_screen(&mut self) {
        self.client.sim.close_screen()
    }
    pub fn furnace_view(&self) -> Vec<i32> {
        self.client.sim.furnace_view()
    }
    /// Окно машины (Mekanism): заголовок, виджеты [n, вид, x, y, w, h …], значения [пиксели, флаги …], подсказка.
    pub fn screen_title(&self) -> String {
        self.client.sim.screen_title()
    }
    pub fn screen_widgets(&self) -> Vec<i32> {
        self.client.sim.screen_widgets()
    }
    pub fn screen_values(&self) -> Vec<i32> {
        self.client.sim.screen_values()
    }
    pub fn screen_tip(&self, i: u32) -> String {
        self.client.sim.screen_tip(i as usize)
    }
    pub fn screen_button(&mut self, i: u32) {
        self.client.sim.screen_button(i as usize)
    }

    // ------------------------------------------------------------ отладка мира (локальный сервер напрямую)

    /// Записать ячейку по правилам игры (обновления соседей, тики, блок-сущности, опад) в мир сервера — сразу, между
    /// тиками; клиент видит правку тем же кадром. false — вне высоты или то же.
    pub fn world_edit(&mut self, x: i32, y: i32, z: i32, cell: Cell) -> bool {
        if self.online {
            return false;
        }
        self.sync();
        let r = self.server.sim.world_edit([x, y, z], cell);
        self.sync();
        r
    }
    /// [запланированных тиков, падающих блоков, блок-сущностей] — у локального сервера (по сети — что видит клиент).
    pub fn world_stats(&self) -> Vec<u32> {
        if self.online {
            let c = &self.client.sim;
            return vec![0, c.falling.len() as u32, c.world.d.borrow().be().len() as u32];
        }
        self.server.sim.world_stats()
    }
    /// Предметов в мире (у локального сервера; по сети — вокруг клиента).
    pub fn item_count(&self) -> u32 {
        if self.online {
            return self.client.sim.items.list.len() as u32;
        }
        self.server.sim.item_count()
    }
    /// Другие игроки в кадре (client.rs player_draws): по 13 чисел на игрока.
    pub fn player_draws(&self, alpha: f64) -> Vec<f32> {
        self.client.sim.player_draws(alpha)
    }
    /// Свободна ли линия взгляда между точками (нет сплошных блоков) — подписи ников не сквозь стены.
    pub fn line_clear(&self, ax: f64, ay: f64, az: f64, bx: f64, by: f64, bz: f64) -> bool {
        self.client.sim.line_clear([ax, ay, az], [bx, by, bz])
    }
    /// Ник игрока id.
    pub fn player_nick(&self, id: u32) -> String {
        self.client.sim.player_nick(id)
    }
    /// Tab (G2): по 4 числа на игрока по нику — номер, пинг мс (−1 — не измерен), флаги (1 — вы), скин.
    pub fn tab_list(&self) -> Vec<i32> {
        self.client.sim.tab_list()
    }
    /// Ники Tab через «\n» в порядке tab_list.
    pub fn tab_names(&self) -> String {
        self.client.sim.tab_names()
    }
    /// Заголовок Tab — имя сервера (локальный — пусто).
    pub fn tab_header(&self) -> String {
        self.client.sim.tab_header()
    }
    /// Подвал Tab: «Игроков: 7 из 32 · день 12 · ваш пинг 64 мс».
    pub fn tab_footer(&self) -> String {
        self.client.sim.tab_footer()
    }
    /// Сколько строк чата пришло с начала сеанса.
    pub fn chat_total(&self) -> u32 {
        self.client.sim.chat_total()
    }
    /// Строки чата после первых seen: «вид|текст» через «\n» (вид — proto::CHAT_*: 0 игрок, 1 вход, 2 выход, 3 смерть,
    /// 4 системное, 5 отказ).
    pub fn chat_since(&self, seen: u32) -> String {
        self.client.sim.chat_since(seen)
    }
    /// Отправить сообщение в чат (сервер чистит и ограничивает частоту).
    pub fn chat_send(&mut self, text: &str) {
        self.client.sim.chat_send(text)
    }
    /// Подсказка команды (G17) к тексту до курсора: «синтаксис\nначало слова\nвариант…»; пусто — не команда.
    pub fn chat_hint(&self, text: &str) -> String {
        self.client.sim.chat_hint(text)
    }
    /// [режим игры (0 выживание, 1 творческий, 3 наблюдатель), права (0 игрок, 1 админ, 2 владелец)].
    pub fn game_mode(&self) -> Vec<u8> {
        self.client.sim.game_mode()
    }
    /// Сеть (отладка MP-0): [поправок движения, откатов предсказаний, расхождений окна, расхождений инвентаря,
    /// телепортов, байт вверх, байт вниз, кадров вниз].
    pub fn net_stats(&self) -> Vec<f64> {
        let (s, c) = (&self.server.sim.stats, &self.client.sim.stats);
        vec![
            s.corrections as f64,
            c.rollbacks as f64,
            s.win_mismatch as f64,
            s.inv_mismatch as f64,
            c.teleports as f64,
            self.client.bytes_up as f64,
            self.client.bytes_down as f64,
            c.frames_in as f64,
        ]
    }
}

/// Проверки: доступ к обеим сторонам и прямые правки сервера между тиками (с синхронизацией клиента).
#[cfg(test)]
impl Game {
    /// Клиент: игрок и управление.
    pub(crate) fn ctl(&self) -> &client::Ctl {
        &self.client.sim.ctl
    }
    /// Сервер: симуляция мира.
    pub(crate) fn sim(&self) -> &sim::Sim {
        &self.server.sim.sim
    }
    /// Сервер: свой игрок.
    pub(crate) fn me(&self) -> &server::ServerPlayer {
        let id = self.server.pid.expect("вошёл");
        self.server.sim.player(id).unwrap()
    }
    /// Мир сервера.
    pub(crate) fn sw(&self) -> World {
        self.server.sim.sim.world.share()
    }
    /// Правка сервера напрямую: сначала очередь клиента, потом f, потом всё изменённое — клиенту.
    pub(crate) fn on_server<R>(&mut self, f: impl FnOnce(&mut ServerSim, usize) -> R) -> R {
        self.sync();
        let i = self.server.sim.idx(self.server.pid.unwrap()).unwrap();
        let r = f(&mut self.server.sim, i);
        self.sync();
        r
    }
    /// Положить стек в ячейку инвентаря (у сервера и клиента).
    pub(crate) fn set_slot(&mut self, k: usize, s: ItemStack) {
        self.on_server(|sv, i| sv.players[i].inv.set(k, s));
    }
    /// Выбрать ячейку хотбара (у клиента и сервера).
    pub(crate) fn select(&mut self, k: usize) {
        self.client.sim.ctl.inv.selected = k;
        self.sync();
    }
    /// Поправить тело (у клиента и сервера одинаково).
    pub(crate) fn body(&mut self, f: impl Fn(&mut Player)) {
        self.sync();
        f(&mut self.client.sim.ctl.player);
        let id = self.server.pid.unwrap();
        f(&mut self.server.sim.player_mut(id).unwrap().player);
    }
    /// Открыть окно у сервера (WinOpen) — клиент видит его сразу.
    pub(crate) fn open_screen(&mut self, m: Menu) {
        self.on_server(|sv, i| sv.open_screen(i, m));
    }
    /// Запись по правилам игры у сервера.
    pub(crate) fn put(&mut self, p: [i32; 3], c: Cell) -> bool {
        self.on_server(|sv, _| sv.world_edit(p, c))
    }
    /// Поломка игроком у сервера (Auth).
    pub(crate) fn break_block(&mut self, p: [i32; 3]) {
        self.on_server(|sv, i| sv.auth(i, |c| interact::break_block(c, p)));
    }
    /// ПКМ по блоку у сервера (Auth): окно открывается, как от Act::Use.
    pub(crate) fn interact(&mut self, hand: usize, h: interact::Hit) -> interact::Use {
        self.on_server(|sv, i| {
            let (r, open) = sv.auth(i, |c| interact::interact_block(c, hand, h));
            if let Some(m) = open {
                sv.open_screen(i, m);
            }
            r
        })
    }
    /// Поправки, откаты, расхождения окна и инвентаря — все нули в одной петле.
    pub(crate) fn assert_in_sync(&self) {
        let (s, c) = (&self.server.sim.stats, &self.client.sim.stats);
        assert_eq!((s.corrections, c.rollbacks, s.win_mismatch, s.inv_mismatch, c.teleports), (0, 0, 0, 0, 0), "сеть разошлась");
        let a = self.ctl();
        let b = self.me();
        assert_eq!((0..41).map(|i| a.inv.get(i)).collect::<Vec<_>>(), (0..41).map(|i| b.inv.get(i)).collect::<Vec<_>>(), "инвентарь");
        assert_eq!(a.player.pos.map(f64::to_bits), b.player.pos.map(f64::to_bits), "позиция");
        let (cw, sw) = (self.client.sim.world.d.borrow(), self.server.sim.sim.world.d.borrow());
        let mut ch: Vec<(i32, i32)> = sw.edited_chunks().chain(cw.edited_chunks()).collect();
        ch.sort_unstable();
        ch.dedup();
        for c in ch {
            assert_eq!(cw.chunk_edits(c.0, c.1), sw.chunk_edits(c.0, c.1), "правки куска {c:?}");
        }
    }
}

#[cfg(test)]
mod game_tests {
    use super::*;
    use crate::abi::*;
    use crate::cell::F_UP;
    use crate::cell::F_WEST;
    use crate::cell::BlockId;
    use crate::game::entity::DELAY_BLOCK;
    use crate::game::entity::DELAY_THROWN;
    use crate::game::interact::{blocks_placement, raycast};
    use crate::game::view::{item_name, item_tooltip};
    use crate::game::inventory::OFFHAND;
    use crate::registry::blocks::{bdef, is_full_cube, layer, opaque, plant_supported, Loot};
    use crate::registry::items::*;
    use crate::cell::offset;
    use crate::mesh::ChunkMesh;

    /// Сцена высоко над рельефом (воздух): пол из камня 9×9 на y = 100, игрок в центре на полу.
    fn scene() -> (World, Game) {
        let w = World::new(1337);
        for x in -4..=4 {
            for z in -4..=4 {
                w.set_block(x, 100, z, STONE);
            }
        }
        let mut g = Game::new(&w);
        g.teleport(0.5, 101.0, 0.5);
        g.body(|p| {
            p.on_ground = true;
            p.vel = [0.0, -0.0784, 0.0];
        });
        g.tick(); // устоялся на полу
        (w, g)
    }
    /// Держать ЛКМ на цели до поломки: тики от нажатия (тик нажатия — 1-й) до исчезновения блока.
    fn hold_until_broken(g: &mut Game, w: &World, p: [i32; 3], first_click: bool) -> u32 {
        g.set_keys(K_LOCKED | K_ATTACK);
        if first_click {
            g.press(P_ATTACK);
        }
        let mut t = 0;
        while w.get_block(p[0], p[1], p[2]) != AIR {
            g.tick();
            t += 1;
            assert!(t < 2000, "не сломалось");
        }
        t
    }

    #[test]
    fn break_times_in_world() {
        // Земля рукой: первый блок — 15 тиков; бревно рукой — 61.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, DIRT);
        g.set_look(0.0, 30.0);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 101, 1]));
        assert_eq!(hold_until_broken(&mut g, &w, [0, 101, 1], true), 15);
        // Выпала предметом в мире: в клетке ±0.25 (низ −0.125), задержка подбора 10 (в этом же тике уже −1).
        let e = &g.sim().items.list[0];
        assert_eq!((e.stack, g.sim().items.list.len()), (ItemStack::of(DIRT as ItemId, 1), 1));
        assert!(e.pickup_delay <= DELAY_BLOCK && (e.pos[2] - 1.5).abs() <= 0.25 + 0.21 && (e.pos[0] - 0.5).abs() <= 0.25 + 0.21);
        for _ in 0..60 {
            g.tick();
        }
        assert_eq!(have_all(&g, DIRT), 1, "в мире или в инвентаре");
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, LOG);
        g.set_look(0.0, 30.0);
        g.tick();
        assert_eq!(hold_until_broken(&mut g, &w, [0, 101, 1], true), 61);
        assert_eq!(have_all(&g, LOG), 1);
        // Камень рукой: 151 тик, ничего не выпало.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, STONE);
        g.set_look(0.0, 30.0);
        g.tick();
        assert_eq!(hold_until_broken(&mut g, &w, [0, 101, 1], true), 151);
        assert!(g.ctl().inv.main.iter().all(|s| s.is_empty()) && g.sim().items.list.is_empty(), "ничего не выпало");
    }

    /// Сколько штук предмета блока b у игрока и в мире вместе.
    fn have_all(g: &Game, b: BlockId) -> i32 {
        let it = b as ItemId;
        g.me().inv.main.iter().filter(|s| s.item == it).map(|s| s.n()).sum::<i32>() + g.sim().items.list.iter().filter(|e| e.stack.item == it).map(|e| e.stack.n()).sum::<i32>()
    }

    #[test]
    fn breaking_rhythm() {
        // Цепочка при удержании: период N + 6 (земля рукой — 21); звук удара и стадии трещин по тикам прогресса.
        // Рука — пустая ячейка 1: подобранная земля ложится в ячейку 0 и стек в руке не меняется (иначе ломание
        // началось бы заново, как в Minecraft).
        let (w, mut g) = scene();
        for z in 1..=3 {
            w.set_block(0, 101, z, DIRT);
        }
        g.select(1);
        g.set_look(0.0, 30.0);
        g.tick();
        g.set_keys(K_LOCKED | K_ATTACK);
        g.press(P_ATTACK);
        let mut broken_at = Vec::new();
        let mut hits = Vec::new();
        let mut stages = Vec::new();
        for t in 1..=80 {
            g.tick();
            for e in g.take_events().chunks(5) {
                if e[0] == EV_BROKEN {
                    broken_at.push(t);
                }
                if e[0] == EV_HIT && broken_at.is_empty() {
                    hits.push(t);
                }
            }
            if broken_at.is_empty() {
                stages.push(g.ctl().mine.stage);
            }
        }
        assert!(broken_at.len() >= 3, "{broken_at:?}");
        assert_eq!(broken_at[..3], [15, 36, 57], "период 21");
        assert_eq!(hits, vec![1, 5, 9, 13], "звук удара на тиках прогресса 1, 5, 9, 13");
        assert_eq!(stages[..14], [0, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7, 8, 8, 9]);
        // После паузы (кулдаун 5 переживает отпускание): следующий блок — N + 5 = 20.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, DIRT);
        w.set_block(0, 101, 2, DIRT);
        g.set_look(0.0, 30.0);
        g.tick();
        hold_until_broken(&mut g, &w, [0, 101, 1], true);
        g.set_keys(K_LOCKED);
        for _ in 0..30 {
            g.tick();
        }
        assert_eq!(hold_until_broken(&mut g, &w, [0, 101, 2], true), 20);
        // Клик в воздух (тик 1) и удержание на блоке: тики 1–10 — ничего, 11 — начало, 12 — первый прогресс.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, DIRT);
        g.set_look(180.0, -30.0); // в небо
        g.tick();
        g.set_keys(K_LOCKED | K_ATTACK);
        g.press(P_ATTACK);
        g.tick();
        assert_eq!(g.ctl().attack_cd, 9);
        g.set_look(0.0, 30.0);
        let mut first = None;
        for t in 2..=20 {
            g.tick();
            if first.is_none() && g.ctl().mine.progress > 0.0 {
                first = Some(t);
            }
        }
        assert_eq!(first, Some(12));
        // Мгновенные: кликом — сразу, в тике клика.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, TALL_GRASS);
        g.set_look(0.0, 50.0);
        g.tick();
        g.set_keys(K_LOCKED | K_ATTACK);
        g.press(P_ATTACK);
        g.tick();
        assert_eq!(w.get_block(0, 101, 1), AIR, "клик срезал траву");
    }

    #[test]
    fn grab_click_does_not_break() {
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, TALL_GRASS);
        g.set_look(0.0, 50.0);
        g.tick();
        g.grab();
        g.set_keys(K_LOCKED | K_ATTACK); // ЛКМ ещё зажата после клика захвата
        for _ in 0..5 {
            g.tick();
        }
        assert_eq!(w.get_block(0, 101, 1), TALL_GRASS);
        g.set_keys(K_LOCKED);
        g.tick();
        g.set_keys(K_LOCKED | K_ATTACK);
        g.tick();
        assert_eq!(w.get_block(0, 101, 1), AIR, "после отпускания ломает");
    }

    #[test]
    fn placement_rules() {
        // «Не в себя»: границы 1e-7 (ASP-05 §10 п. 2).
        let cell = Aabb { lo: [0.0, 0.0, 0.0], hi: [1.0, 1.0, 1.0] };
        let body = |y: f64| Aabb { lo: [0.2, y, 0.2], hi: [0.8, y + 1.8, 0.8] };
        assert!(!blocks_placement(&cell, &body(1.0 - 1e-8)));
        assert!(blocks_placement(&cell, &body(1.0 - 1e-6)));
        let side = Aabb { lo: [5.0, 0.0, 0.0], hi: [6.0, 1.0, 1.0] };
        assert!(!blocks_placement(&side, &Aabb { lo: [4.4, 0.0, 0.2], hi: [5.0, 1.8, 0.8] }));
        // Под ноги, стоя на полу, — нельзя: ни блока, ни расхода.
        let (w, mut g) = scene();
        g.give(DIRT as ItemId, 10);
        g.set_look(0.0, 90.0);
        g.set_keys(K_LOCKED);
        g.press(P_USE);
        g.tick();
        assert_eq!(w.get_block(0, 101, 0), AIR);
        assert_eq!(g.ctl().inv.main[0].count, 10);
        // На траву-растение — в её клетку.
        w.set_block(0, 100, 1, DIRT);
        w.set_block(0, 101, 1, TALL_GRASS);
        g.set_look(0.0, 50.0);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 101, 1]));
        g.press(P_USE);
        g.tick();
        assert_eq!(w.get_block(0, 101, 1), DIRT, "земля встала на место травы");
        assert_eq!(g.ctl().inv.main[0].count, 9);
        // По цветку — рядом (над ним по грани обводки), цветок остаётся.
        let (w, mut g) = scene();
        g.give(DIRT as ItemId, 10);
        w.set_block(0, 100, 1, DIRT);
        w.set_block(0, 101, 1, DANDELION);
        g.set_look(0.0, 50.0);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 101, 1]));
        g.set_keys(K_LOCKED);
        g.press(P_USE);
        g.tick();
        assert_eq!(w.get_block(0, 101, 1), DANDELION);
        assert_eq!(g.ctl().inv.main[0].count, 9);
        // Удержание ПКМ: попытки в тиках 0, 4, 8, 12.
        let (_w, mut g) = scene();
        g.give(DIRT as ItemId, 64);
        g.set_look(0.0, 25.0);
        g.set_keys(K_LOCKED | K_USE);
        let mut placed = Vec::new();
        for t in 0..13 {
            g.tick();
            for e in g.take_events().chunks(5) {
                if e[0] == EV_PLACED {
                    placed.push(t);
                }
            }
        }
        assert_eq!(placed, vec![0, 4, 8, 12]);
    }

    #[test]
    fn pillar_jump_place() {
        // Space + ПКМ взглядом вниз: первый блок под себя — на 3-м тике после прыжка; столб без пропусков (ASP-05 §7.3).
        let (_w, mut g) = scene();
        g.give(DIRT as ItemId, 64);
        g.set_look(0.0, 90.0);
        g.set_keys(K_LOCKED | K_JUMP | K_USE);
        let mut placed = Vec::new();
        for t in 1..=200 {
            g.tick();
            for e in g.take_events().chunks(5) {
                if e[0] == EV_PLACED {
                    placed.push((t, e[2]));
                }
            }
        }
        assert_eq!(placed.len(), 20, "{placed:?}");
        for (i, (_, y)) in placed.iter().enumerate() {
            assert_eq!(*y, 101 + i as i32, "столб без пропусков");
        }
        let gaps: Vec<i32> = placed.windows(2).map(|p| p[1].0 - p[0].0).collect();
        assert!(gaps.iter().all(|&d| d == 8 || d == 12), "{gaps:?}");
        // Клик каждый тик: прыжок в тике 1, первое удачное ставление — через 3 тика (по коробке конца прошлого тика).
        let (_w, mut g) = scene();
        g.give(DIRT as ItemId, 64);
        g.set_look(0.0, 90.0);
        g.set_keys(K_LOCKED | K_JUMP);
        let mut first = None;
        for t in 1..=12 {
            g.press(P_USE);
            g.tick();
            if first.is_none() && g.take_events().chunks(5).any(|e| e[0] == EV_PLACED) {
                first = Some(t);
            }
        }
        assert_eq!(first, Some(4));
    }

    #[test]
    fn height_limit_message() {
        let w = World::new(1337);
        for x in -2..=2 {
            for z in -2..=2 {
                w.set_block(x, 126, z, STONE);
            }
        }
        w.set_block(0, 127, 1, STONE);
        let mut g = Game::new(&w);
        g.teleport(0.5, 127.0, 0.5);
        g.give(DIRT as ItemId, 5);
        g.set_look(0.0, 35.0);
        g.tick();
        let t = g.ctl().target.map(|h| (h.pos, h.face));
        assert_eq!(t, Some(([0, 127, 1], F_UP)));
        g.set_keys(K_LOCKED);
        g.press(P_USE);
        g.tick();
        assert_eq!(g.client.sim.view.overlay_msg, MSG_HEIGHT);
        assert!(g.client.sim.view.overlay > 50);
        assert_eq!(g.ctl().inv.main[0].count, 5);
    }

    #[test]
    fn plants_fall_with_ground() {
        // Ломаю землю под цветком — цветок исчезает в том же тике (обновление формы соседа) и выпадает.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, DIRT);
        w.set_block(0, 102, 1, POPPY);
        g.set_look(0.0, 60.0);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 101, 1]));
        hold_until_broken(&mut g, &w, [0, 101, 1], true);
        assert_eq!(w.get_block(0, 102, 1), AIR);
        assert_eq!(have_all(&g, POPPY), 1, "мак выпал");
        assert_eq!(have_all(&g, DIRT), 1);
    }

    #[test]
    fn ray_asp04() {
        // Глаза (0.5, 65.62, 0.5), пол y = 64 (ASP-04 §15).
        let w = World::new(1337);
        let mut d = w.d.borrow_mut();
        for x in -1..=6 {
            for y in 60..=70 {
                for z in -1..=1 {
                    d.set_cell(x, y, z, if y < 64 { STONE as Cell } else { AIR as Cell });
                }
            }
        }
        let eye = [0.5, 65.62, 0.5];
        let h = raycast(&mut d, eye, [0.0, -1.0, 0.0], 4.5, false).unwrap();
        assert_eq!((h.pos, h.face), ([0, 63, 0], F_UP));
        assert!((h.point[1] - 64.0).abs() < 1e-9);
        d.set_cell(4, 65, 0, STONE as Cell);
        let h = raycast(&mut d, eye, [1.0, 0.0, 0.0], 4.5, false).unwrap();
        assert_eq!((h.pos, h.face), ([4, 65, 0], F_WEST));
        assert!((h.point[0] - eye[0] - 3.5).abs() < 1e-9);
        d.set_cell(4, 65, 0, AIR as Cell);
        d.set_cell(5, 65, 0, STONE as Cell);
        assert!(raycast(&mut d, eye, [1.0, 0.0, 0.0], 4.5, false).is_none(), "грань ровно на 4.5 — промах");
        // Над травой-растением в соседней клетке на высоте 64.9: трава 13/16 не выбрана, выбран блок за ней.
        d.set_cell(2, 64, 0, TALL_GRASS as Cell);
        d.set_cell(3, 64, 0, STONE as Cell);
        let h = raycast(&mut d, [0.5, 64.9, 0.5], [1.0, 0.0, 0.0], 4.5, false).unwrap();
        assert_eq!(h.pos, [3, 64, 0]);
        // Начало внутри полного блока — попадание в него, inside, грань против взгляда.
        let h = raycast(&mut d, [0.5, 62.5, 0.5], [1.0, 0.0, 0.0], 4.5, false).unwrap();
        assert!(h.inside && h.pos == [0, 62, 0] && h.face == F_WEST);
        // Сквозь воду — к дну.
        d.set_cell(0, 64, 0, WATER as Cell);
        d.set_cell(0, 65, 0, WATER as Cell);
        let h = raycast(&mut d, eye, [0.0, -1.0, 0.0], 4.5, false).unwrap();
        assert_eq!(h.pos, [0, 63, 0]);
    }

    #[test]
    fn loot_frequencies() {
        // Гравий: кремень 10 %, иначе гравий, никогда оба; трава: семя 12.5 %; мёртвый куст: 0/1/2 палки поровну.
        let w = World::new(7);
        let mut g = Game::new(&w);
        let n = 20000;
        let (mut flint, mut gravel, mut seeds) = (0, 0, 0);
        let mut sticks = [0; 3];
        // Дроп — предметы в мире: считаем их.
        let drops = |g: &mut Game, b: BlockId| -> Vec<ItemStack> {
            let s = &mut g.server.sim.sim;
            s.items.list.clear();
            s.drop_loot(b as Cell, [0; 3], true);
            s.items.list.iter().map(|e| e.stack).collect()
        };
        for _ in 0..n {
            let d = drops(&mut g, GRAVEL);
            let f = d.iter().filter(|s| s.item == I_FLINT).count();
            let gr = d.iter().filter(|s| s.item == GRAVEL as ItemId).count();
            assert_eq!(f + gr, 1);
            flint += f;
            gravel += gr;
            seeds += drops(&mut g, TALL_GRASS).iter().filter(|s| s.item == I_WHEAT_SEEDS).count();
            let d = drops(&mut g, DEAD_BUSH);
            sticks[d.iter().map(|s| s.n()).sum::<i32>() as usize] += 1;
        }
        let near = |k: usize, p: f64| ((k as f64 / n as f64) - p).abs() < 3.0 * (p * (1.0 - p) / n as f64).sqrt() + 1e-3;
        assert!(near(flint, 0.1) && near(gravel, 0.9), "кремень {flint}");
        assert!(near(seeds, 0.125), "семена {seeds}");
        for s in sticks {
            assert!(near(s, 1.0 / 3.0), "палки {sticks:?}");
        }
    }

    /// Подобрать всё, что лежит: встать на каждый предмет и подождать задержку подбора.
    fn collect(g: &mut Game) {
        for _ in 0..200 {
            let Some(p) = g.sim().items.list.first().map(|e| e.pos) else { break };
            g.set_keys(K_LOCKED);
            g.teleport(p[0], 101.0, p[2]);
            g.tick();
        }
        assert!(g.sim().items.list.is_empty(), "не всё подобрано");
    }
    fn count(g: &Game, it: ItemId) -> i32 {
        g.ctl().inv.main.iter().filter(|s| s.item == it).map(|s| s.n()).sum()
    }
    fn slot_of(g: &Game, it: ItemId) -> usize {
        g.ctl().inv.main.iter().position(|s| s.item == it).expect("нет предмета")
    }
    /// Слот окна первой пустой ячейки инвентаря (положить курсор).
    fn empty_ui(g: &Game) -> i32 {
        ui_slot(g, g.ctl().inv.main.iter().position(|s| s.is_empty()).expect("нет места"))
    }
    /// Номер слота окна для ячейки инвентаря: хотбар 0–8 → 36… (инвентарь) / 37… (верстак), рюкзак 9–35 → 9… / 10….
    fn ui_slot(g: &Game, inv: usize) -> i32 {
        let base = if g.screen_kind() == 2 { (37, 10) } else { (36, 9) };
        (if inv < 9 { base.0 + inv } else { base.1 + inv - 9 }) as i32
    }

    /// Отпечаток состояния (MP-0): мир, блок-сущности, инвентарь, предметы, игрок, ГСЧ — у сервера (с ним сверены
    /// клиент: assert_in_sync). Эталоны — тот же отпечаток прежней игры (Game до разделения, после перехода на libm).
    pub(crate) fn fingerprint(g: &Game) -> String {
        let b = |v: [f64; 3]| format!("{:x},{:x},{:x}", v[0].to_bits(), v[1].to_bits(), v[2].to_bits());
        let s = g.sim();
        let w = s.world.d.borrow();
        let mut chunks: Vec<(i32, i32)> = w.edited_chunks().collect();
        chunks.sort();
        let mut out = String::new();
        for c in chunks {
            out += &format!("{c:?}:{:?};", w.chunk_edits(c.0, c.1));
        }
        let mut bes: Vec<String> = w.be().iter().map(|(p, _, be)| format!("{p:?}{be:?}")).collect();
        bes.sort();
        out += &bes.join(",");
        let me = g.me();
        out += &format!("|inv {} {:?}", me.inv.selected, (0..41).map(|i| me.inv.get(i)).collect::<Vec<_>>());
        for e in &s.items.list {
            out += &format!("|it#{} {:?} {} {} {} {}", e.id, e.stack, b(e.pos), b(e.vel), e.age, e.pickup_delay);
        }
        let p = &me.player;
        out += &format!("|pl {} {} {} {:x}", b(p.pos), b(p.vel), p.on_ground, p.fall.to_bits());
        out += &format!("|rng {:x} {:x} {:x} {} {}", s.rng.0, s.tick_rng.0, g.client.sim.view.fx_rng.0, s.time, s.day);
        out
    }
    pub(crate) fn fnv(s: &str) -> u64 {
        s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
    }

    #[test]
    fn path_tree_to_stone_pickaxe() {
        // Дерево → доски → палки → верстак → деревянная кирка → камень → каменная кирка (ASP-09 §4.3, ASP-06 §2.3).
        let (w, mut g) = scene();
        g.select(8);
        w.set_block(0, 101, 1, LOG);
        g.set_look(0.0, 30.0);
        g.tick();
        assert_eq!(hold_until_broken(&mut g, &w, [0, 101, 1], true), 61, "бревно рукой");
        collect(&mut g);
        g.give(LOG as ItemId, 2);
        assert_eq!(count(&g, LOG as ItemId), 3);
        // Окно инвентаря (E в тике), сетка 2×2.
        g.press(P_INVENTORY);
        g.tick();
        assert_eq!(g.screen_kind(), 1);
        let logs = ui_slot(&g, slot_of(&g, LOG as ItemId));
        g.screen_click(logs, 0, A_PICKUP);
        g.screen_click(1, 0, A_PICKUP);
        g.screen_click(0, 0, A_QUICK_MOVE);
        assert_eq!(count(&g, PLANKS as ItemId), 12, "3 бревна → 12 досок");
        // Палки: доски в 1 и 3 (столбец), остаток досок — обратно.
        let planks = ui_slot(&g, slot_of(&g, PLANKS as ItemId));
        g.screen_click(planks, 0, A_PICKUP);
        g.screen_click(1, 1, A_PICKUP);
        g.screen_click(3, 1, A_PICKUP);
        g.screen_click(0, 0, A_QUICK_MOVE);
        g.screen_click(empty_ui(&g), 0, A_PICKUP);
        assert_eq!((count(&g, I_STICK), count(&g, PLANKS as ItemId)), (4, 10));
        // Верстак: 4 доски квадратом.
        let planks = ui_slot(&g, slot_of(&g, PLANKS as ItemId));
        g.screen_click(planks, 0, A_PICKUP);
        for c in 1..=4 {
            g.screen_click(c, 1, A_PICKUP);
        }
        g.screen_click(0, 0, A_QUICK_MOVE);
        g.screen_click(empty_ui(&g), 0, A_PICKUP);
        assert_eq!((count(&g, CRAFTING_TABLE as ItemId), count(&g, PLANKS as ItemId)), (1, 6));
        g.close_screen();
        assert!(g.screen_kind() == 0 && g.sim().items.list.is_empty());
        // Ставлю верстак и открываю его ПКМ (верстак в руке — открытие, не ставление).
        g.press(P_HOTBAR + slot_of(&g, CRAFTING_TABLE as ItemId) as u32);
        g.set_look(0.0, 30.0);
        g.set_keys(K_LOCKED);
        g.tick();
        let t = g.ctl().target.unwrap();
        g.press(P_USE);
        g.tick();
        let table = offset(t.pos, t.face);
        assert_eq!(w.get_block(table[0], table[1], table[2]), CRAFTING_TABLE);
        for _ in 0..5 {
            g.tick();
        }
        assert_eq!(g.ctl().target.map(|h| h.pos), Some(table));
        g.press(P_USE);
        g.tick();
        assert_eq!(g.screen_kind(), 2, "окно верстака");
        // Деревянная кирка: доски — верхний ряд (1, 2, 3), палки — 5 и 8.
        let planks = ui_slot(&g, slot_of(&g, PLANKS as ItemId));
        let sticks = ui_slot(&g, slot_of(&g, I_STICK));
        g.screen_click(planks, 0, A_PICKUP);
        for c in 1..=3 {
            g.screen_click(c, 1, A_PICKUP);
        }
        g.screen_click(planks, 0, A_PICKUP);
        g.screen_click(sticks, 0, A_PICKUP);
        g.screen_click(5, 1, A_PICKUP);
        g.screen_click(8, 1, A_PICKUP);
        g.screen_click(sticks, 0, A_PICKUP);
        g.screen_click(0, 0, A_QUICK_MOVE);
        assert_eq!(count(&g, I_WOODEN_PICKAXE), 1, "деревянная кирка");
        g.close_screen();
        // Три камня деревянной киркой: по 23 тика, булыжник выпадает, износ 3.
        g.press(P_HOTBAR + slot_of(&g, I_WOODEN_PICKAXE) as u32);
        g.teleport(0.5, 101.0, 0.5);
        for _ in 0..3 {
            w.set_block(1, 101, 0, STONE);
            g.set_look(-90.0, 30.0);
            g.set_keys(K_LOCKED);
            g.tick();
            assert_eq!(g.ctl().target.map(|h| h.pos), Some([1, 101, 0]));
            // 23 тика + 5 задержки после прошлой поломки (она убывает только при удержании на блоке, ASP-04).
            assert_eq!(hold_until_broken(&mut g, &w, [1, 101, 0], true), 23 + 5, "камень деревянной киркой");
            g.set_keys(K_LOCKED);
            for _ in 0..10 {
                g.tick();
            }
            collect(&mut g);
            g.teleport(0.5, 101.0, 0.5);
        }
        assert_eq!(count(&g, COBBLE as ItemId), 3);
        assert_eq!(g.ctl().inv.main[slot_of(&g, I_WOODEN_PICKAXE)].damage, 3);
        // Каменная кирка на верстаке.
        g.set_look(0.0, 30.0);
        g.set_keys(K_LOCKED);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some(table));
        g.press(P_USE);
        g.tick();
        assert_eq!(g.screen_kind(), 2);
        let cob = ui_slot(&g, slot_of(&g, COBBLE as ItemId));
        let sticks = ui_slot(&g, slot_of(&g, I_STICK));
        g.screen_click(cob, 0, A_PICKUP);
        assert!(g.screen_drag_accepts(1, 0, 0));
        assert_eq!(g.screen_drag_preview(0, &[1, 2, 3]), vec![0, 1, 0, 1, 0, 1, 0]);
        // Тяга ЛКМ по верхнему ряду: по одному булыжнику.
        g.screen_click(SLOT_OUTSIDE, 0, A_QUICK_CRAFT);
        for c in 1..=3 {
            g.screen_click(c, 1, A_QUICK_CRAFT);
        }
        g.screen_click(SLOT_OUTSIDE, 2, A_QUICK_CRAFT);
        g.screen_click(sticks, 0, A_PICKUP);
        g.screen_click(5, 1, A_PICKUP);
        g.screen_click(8, 1, A_PICKUP);
        g.screen_click(sticks, 0, A_PICKUP);
        g.screen_click(0, 0, A_QUICK_MOVE);
        assert_eq!(count(&g, I_WOODEN_PICKAXE + 5), 1, "каменная кирка");
        g.close_screen();
        assert!(g.sim().items.list.is_empty(), "ничего не выпало на землю");
        // Главная проверка MP-0: через Client + LocalServer — тот же мир и инвентарь, что у прежней игры, при нуле
        // поправок движения и откатов. Закрытие окна — в очереди до тика: сначала к серверу.
        g.sync();
        g.assert_in_sync();
        let f = fingerprint(&g);
        if fnv(&f) != FP_PATH_TREE {
            std::fs::write(std::env::temp_dir().join("fp_path_tree_new.txt"), &f).unwrap();
        }
        assert_eq!(fnv(&f), FP_PATH_TREE, "отпечаток разошёлся с прежней игрой (fp_path_tree_new.txt во временной папке)");
    }
    /// Отпечаток пути «дерево → каменная кирка» у прежней игры (до разделения, libm).
    const FP_PATH_TREE: u64 = 0x0e44_3184_ead6_a60c;

    #[test]
    fn screen_input_and_validity() {
        // Пока окно открыто, клавиши мира не работают; закрытие возвращает сетку и курсор.
        let (w, mut g) = scene();
        g.give(LOG as ItemId, 5);
        g.open_inventory();
        g.press(P_HOTBAR + 4);
        g.press(P_DROP);
        g.tick();
        assert_eq!((g.ctl().inv.selected, g.sim().items.list.len()), (0, 0));
        g.screen_click(36, 1, A_PICKUP); // половина — 3 в курсоре
        g.screen_click(2, 1, A_PICKUP); // одно бревно в сетку
        assert_eq!(g.screen_view()[3], PLANKS as i32, "результат — доски");
        g.close_screen();
        assert_eq!((count(&g, LOG as ItemId), g.sim().items.list.len()), (5, 0));
        // Верстак: сдвиг игрока на +10 по X — окно закрывается в следующем тике, бревно возвращается.
        w.set_block(1, 101, 1, CRAFTING_TABLE);
        g.open_screen(Menu::crafting([1, 101, 1]));
        g.screen_click(37, 0, A_PICKUP);
        g.screen_click(5, 1, A_PICKUP);
        g.screen_click(37, 0, A_PICKUP);
        g.tick();
        assert_eq!(g.screen_kind(), 2);
        g.debug_shift(10.0, 0.0, 0.0);
        g.tick();
        assert_eq!((g.screen_kind(), count(&g, LOG as ItemId)), (0, 5));
        // Верстак сломан — окно закрывается.
        g.teleport(0.5, 101.0, 0.5);
        g.open_screen(Menu::crafting([1, 101, 1]));
        w.set_block(1, 101, 1, AIR);
        g.tick();
        assert_eq!(g.screen_kind(), 0);
        // E в мире открывает окно в тике; Shift+ПКМ с предметом по верстаку — ставит, а не открывает.
        w.set_block(0, 101, 2, CRAFTING_TABLE);
        g.press(P_INVENTORY);
        g.tick();
        assert_eq!(g.screen_kind(), 1);
        g.close_screen();
        g.set_look(0.0, 8.0); // глаза в приседе 1.27 — верх верстака
        g.set_keys(K_LOCKED | K_SNEAK);
        g.tick();
        g.tick();
        assert_eq!(g.ctl().target.map(|h| (h.pos, h.face)), Some(([0, 101, 2], F_UP)));
        g.give(DIRT as ItemId, 1);
        let k = slot_of(&g, DIRT as ItemId);
        g.select(k);
        g.press(P_USE);
        g.tick();
        assert_eq!(g.screen_kind(), 0, "Shift с предметом — ставит");
        assert_eq!(w.get_block(0, 102, 2), DIRT);
        // Пустой рукой Shift+ПКМ — открывает.
        g.select(7);
        g.set_slot(OFFHAND, ItemStack::EMPTY);
        w.set_block(0, 102, 2, AIR);
        g.tick();
        g.press(P_USE);
        g.tick();
        assert_eq!(g.screen_kind(), 2);
    }

    #[test]
    fn throw_and_pick_block() {
        // Q под ноги: предмет с задержкой 40, подбирается сам ровно через 40 тиков; Ctrl+Q — весь стек.
        let (_w, mut g) = scene();
        g.give(DIRT as ItemId, 10);
        g.set_look(0.0, 90.0);
        g.set_keys(K_LOCKED);
        g.press(P_DROP);
        g.tick();
        assert_eq!((count(&g, DIRT as ItemId), g.sim().items.list.len()), (9, 1));
        assert!(g.sim().items.list[0].thrown && g.sim().items.list[0].pickup_delay == DELAY_THROWN - 1);
        let mut t = 1;
        while !g.sim().items.list.is_empty() {
            g.tick();
            t += 1;
            assert!(t < 100);
        }
        assert_eq!((t, count(&g, DIRT as ItemId)), (40, 10));
        g.press(P_DROP_ALL);
        g.tick();
        assert_eq!((count(&g, DIRT as ItemId), g.sim().items.list[0].stack.count), (0, 10));
        // Глядя прямо — улетает на ~3.3 блока.
        let (_w, mut g) = scene();
        g.give(DIRT as ItemId, 1);
        g.set_look(0.0, 0.0);
        g.set_keys(K_LOCKED);
        g.press(P_DROP);
        for _ in 0..40 {
            g.tick();
        }
        let d = g.sim().items.list[0].pos[2] - 0.5;
        assert!(d > 2.5 && d < 4.1, "полёт {d}");
        // Средняя кнопка: в хотбаре — выбрать ячейку; в рюкзаке — поменять с пустой ячейкой хотбара.
        let (w, mut g) = scene();
        w.set_block(0, 101, 1, COBBLE);
        g.set_look(0.0, 30.0);
        g.set_slot(5, ItemStack::of(COBBLE as ItemId, 3));
        g.set_keys(K_LOCKED);
        g.press(P_PICK);
        g.tick();
        assert_eq!(g.ctl().inv.selected, 5);
        g.set_slot(5, ItemStack::EMPTY);
        g.set_slot(20, ItemStack::of(COBBLE as ItemId, 7));
        g.set_slot(0, ItemStack::of(DIRT as ItemId, 1));
        g.select(0);
        g.press(P_PICK);
        g.tick();
        assert_eq!((g.ctl().inv.selected, g.ctl().inv.main[1], g.ctl().inv.main[20]), (1, ItemStack::of(COBBLE as ItemId, 7), ItemStack::EMPTY));
    }

    #[test]
    fn tool_wear_and_torch() {
        // Деревянная кирка ломается на 59-м камне, и он выпадает (ASP-06 §9).
        let (w, mut g) = scene();
        g.give(I_WOODEN_PICKAXE, 1);
        g.set_look(0.0, 30.0);
        g.set_keys(K_LOCKED);
        g.tick();
        let mut n = 0;
        while !g.ctl().inv.main[0].is_empty() {
            w.set_block(0, 101, 1, STONE);
            hold_until_broken(&mut g, &w, [0, 101, 1], true);
            n += 1;
            g.set_keys(K_LOCKED);
            for _ in 0..6 {
                g.tick();
            }
            assert!(n < 70);
        }
        assert_eq!(n, 59);
        let cobble = g.sim().items.list.iter().filter(|e| e.stack.item == COBBLE as ItemId).map(|e| e.stack.n()).sum::<i32>() + count(&g, COBBLE as ItemId);
        assert_eq!(cobble, 59, "последний блок тоже выпал");
        // Факел: ставится на верх блока; на листву — нет; без опоры отваливается предметом.
        let (w, mut g) = scene();
        g.give(TORCH as ItemId, 2);
        w.set_block(0, 101, 2, DIRT);
        g.set_look(0.0, 18.0); // верх земли
        g.set_keys(K_LOCKED);
        g.tick();
        assert_eq!(g.ctl().target.map(|h| (h.pos, h.face)), Some(([0, 101, 2], F_UP)));
        g.press(P_USE);
        g.tick();
        assert_eq!(w.get_block(0, 102, 2), TORCH);
        assert!(!plant_supported(TORCH, LEAVES) && plant_supported(TORCH, GLASS) && plant_supported(TORCH, STONE));
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 102, 2]), "обводка факела ловит луч");
        g.set_look(0.0, 25.0); // северная грань опоры
        g.tick();
        assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, 101, 2]));
        let (at, support) = ([0, 102, 2], [0, 101, 2]);
        hold_until_broken(&mut g, &w, support, true);
        assert_eq!(w.get_block(at[0], at[1], at[2]), AIR);
        assert_eq!(have_all(&g, TORCH), 2);
    }

    #[test]
    fn new_blocks_break_times() {
        // ASP-06 §2.3 / §9 (f32): доски рукой 61, верстак рукой 76, печь деревянной киркой 53 (рукой 350 без дропа),
        // угольная руда деревянной 45, железная деревянной 151 без дропа, каменной 23.
        let ticks = |item: ItemId, b: BlockId| {
            let d = break_delta(item, bdef(b), true, false);
            let (mut p, mut n) = (0f32, 0);
            while p < 1.0 {
                p += d;
                n += 1;
            }
            n
        };
        let (wp, sp) = (I_WOODEN_PICKAXE, I_WOODEN_PICKAXE + 5);
        assert_eq!((ticks(0, PLANKS), ticks(0, CRAFTING_TABLE), ticks(wp, FURNACE), ticks(0, FURNACE)), (61, 76, 53, 350));
        assert_eq!((ticks(wp, COAL_ORE), ticks(wp, IRON_ORE), ticks(sp, IRON_ORE)), (45, 151, 23));
        assert!(!can_harvest(0, bdef(FURNACE)) && !can_harvest(wp, bdef(IRON_ORE)) && can_harvest(sp, bdef(IRON_ORE)));
        assert!(break_delta(0, bdef(TORCH), true, false) >= 1.0, "факел — мгновенно");
        assert!(matches!(bdef(GLASS).loot, Loot::None) && is_full_cube(GLASS) && !opaque(GLASS));
        assert_eq!(layer(PLANKS, 2), L_OAK_PLANKS);
        assert_eq!((layer(CRAFTING_TABLE, 2), layer(CRAFTING_TABLE, 5), layer(CRAFTING_TABLE, 0)), (L_TABLE_TOP, L_TABLE_FRONT, L_TABLE_SIDE));
        assert_eq!(item_name(TORCH as ItemId), "Факел");
        assert!(item_tooltip(I_WOODEN_PICKAXE).contains("2 урона"));
    }

    #[test]
    fn glass_mesh() {
        // Стекло — в потоке cutout; к соседнему стеклу грань не рисуется; соседний камень рисует грань к стеклу.
        let w = World::new(1337);
        for x in 0..2 {
            w.set_block(x, 120, 8, GLASS);
        }
        let before = w.mesh_chunk(0, 0, 1);
        let quads = |v: Vec<u32>| v.len() / 8;
        let glass_q = |m: &ChunkMesh| m.cutout.chunks(2).filter(|v| v[1] & 0xFF == L_GLASS).count() / 4;
        assert_eq!(glass_q(&before), 10, "2 куба стекла без общей грани");
        w.set_block(2, 120, 8, STONE);
        let after = w.mesh_chunk(0, 0, 1);
        assert_eq!(glass_q(&after), 9, "грань стекла к камню закрыта");
        assert!(quads(after.solid.clone()) > quads(before.solid.clone()), "камень рисует грань к стеклу");
    }
}
