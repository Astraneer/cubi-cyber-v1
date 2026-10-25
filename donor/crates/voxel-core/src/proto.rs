//! Протокол клиент ↔ сервер (MP-0, mp/design.md §3): типы C2S и S2C, кодирование postcard (serde) — один двоичный
//! кадр WebSocket на сообщение. Сборки клиента и сервера обязаны совпадать (§2.7): совместимости между версиями нет,
//! PROTO — для понятного отказа. Разбор и сборка — только здесь, JS — труба (Client::recv / take_outbox).
//!
//! Один кадр на тик в каждую сторону: C2S::Tick (всё, что клиент сделал за тик и между тиками, и итог движения) и
//! S2C::Bundle (всё, что сервер отдаёт за свой тик). Отдельно — вход (Hello → Welcome | Reject).
//!
//! В MP-0 это петля в памяти (game/mod.rs: Client + LocalServer в одном потоке): сообщения проходят ту же сериализацию,
//! что пойдёт в сеть в MP-1. Числа в сообщениях — коды abi.rs (удержания Hold, виды окон ScreenKind, события Ev).

use serde::{Deserialize, Serialize};

use crate::world::stack::ItemStack;

/// Версия протокола: несовпадение — Reject(Outdated). 2 — MP-1: игроки (Ent::Player, PlayerMoves, Anim). 3 — MP-2:
/// Reject::BadToken, правки порциями. 4 — Mekanism M1: Act::WinButton, Msg::NetGlow, DebugCmd::Build. 5 — срез 4 «выживание»: Msg::Vitals, Msg::UseStop,
/// Act::UseRelease, DebugCmd::Hurt и SetHp. 6 — мобы (G6): Ent::Mob, Msg::MobMoves, Act::UseEntity, DebugCmd::SpawnMob,
/// Msg::Anim kind 1 — урон. 7 — G7: Act::Bucket, DebugCmd::TickSpeed. 8 — Mekanism M3: Act::ItemMode, Msg::MekSides.
/// 9 — Mekanism M5: Msg::Transit, Msg::TransitEnd (предметы в пути по транспортёрам). 10 — G16: Act::Flight, Msg::Flight,
/// флаг хода MV_NOCLIP (полёт выживания отдельно от отладочного).
/// 11 — G17: Msg::Mode (режим игры и права), виды чата CHAT_WHISPER и CHAT_SAY.
pub(crate) const PROTO: u16 = 11;
/// Сборка: git rev (crates/voxel-core/build.rs; Docker без .git — переменная VOXEL_REV; номер неизвестен — «dev»).
pub(crate) const BUILD: &str = env!("VOXEL_BUILD");

/// Совпадают ли сборки клиента и сервера. «dev» (номер неизвестен: нет git) с любой сборкой не спорит.
pub(crate) fn build_matches(theirs: &str) -> bool {
    theirs == BUILD || theirs == "dev" || BUILD == "dev"
}

/// Отказ «устарел»: протокол или сборка клиента не наши. None — совпало. Текст читает игрок.
pub(crate) fn outdated(proto: u16, build: &str) -> Option<String> {
    if proto != PROTO {
        return Some(format!("Версия игры устарела (протокол {proto}, на сервере {PROTO}). Обновите страницу."));
    }
    if !build_matches(build) {
        return Some(format!("Версия игры устарела (у вас {build}, на сервере {BUILD}). Обновите страницу."));
    }
    None
}

/// Начало Hello — (proto, build) — из кадра, который целиком не разобрался: клиент старой сборки с другим Hello тоже
/// получает «обновите страницу», а не «пакет не разобрался». Опирается на порядок: вариант C2S::Hello — первый (0),
/// в Hello первые поля — proto и build (не менять местами и не вставлять перед ними).
pub(crate) fn hello_head(b: &[u8]) -> Option<(u16, String)> {
    let (&tag, rest) = b.split_first()?;
    if tag != 0 {
        return None;
    }
    let (head, _) = postcard::take_from_bytes::<(u16, String)>(rest).ok()?;
    Some(head)
}

// ---------------------------------------------------------------- клиент → сервер

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum C2S {
    Hello(Hello),
    /// 20 раз в секунду: действия по порядку и итог движения своего тика.
    Tick(ClientTick),
    /// Действия между тиками — сразу, без движения (локальный сервер: отладка и проверки ставят мир между тиками; сеть
    /// шлёт их в Tick).
    Acts(Vec<Act>),
    Chat { text: String },
    Pong { id: u32 },
    View { radius: u8 },
    Away { on: bool },
    Respawn,
}

/// Вход (§3.2): ник и токен (MP-1), радиус правок в кусках, проба генератора (gen::probe — хеши 4 кусков постоянного
/// сида: сверка реализаций, а не мира). Первые два поля (proto, build) на месте навсегда: по ним сервер узнаёт
/// клиента старой сборки, даже если остальное в Hello изменилось (hello_head). token — случайные 128 бит из
/// localStorage; все нули — «гость» без токена: такой ник не закрепляется и чужой (закреплённый) ник не откроет.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Hello {
    pub(crate) proto: u16,
    pub(crate) build: String,
    pub(crate) nick: String,
    pub(crate) token: [u8; 16],
    pub(crate) view: u8,
    pub(crate) probe: [u64; 4],
}

/// Тик клиента: номер, действия в порядке, в каком клиент их сделал (сначала между тиками — окно, колесо, отладка,
/// затем фаза ввода тика), итог движения и хеш предсказанного инвентаря (сервер сверяет со своим и при расхождении
/// шлёт полную копию — MP-5 заменит ревизиями).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct ClientTick {
    pub(crate) n: u32,
    pub(crate) acts: Vec<Act>,
    pub(crate) mv: Move,
    pub(crate) inv: u32,
}

/// Итог движения тика (design.md §1.3): всё, от чего зависит следующий тик игрока, — сервер повторяет тик той же
/// физикой («теневой тик») и сверяет; не совпало — поправка (MP-0: принять и посчитать; MP-1 — проверка и Teleport).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct Move {
    pub(crate) pos: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) fall: f32,
    /// Удержания (маска Hold), с которыми шёл тик игрока.
    pub(crate) keys: u16,
    /// MV_* ниже.
    pub(crate) flags: u16,
    pub(crate) jump_cd: u8,
    pub(crate) dtap: u8,
    /// Ввод «вперёд» после множителей (isWalking следующего тика).
    pub(crate) fwd_in: f32,
    /// Опорный блок.
    pub(crate) support: Option<[i32; 3]>,
}

pub(crate) const MV_GROUND: u16 = 1;
pub(crate) const MV_SPRINT: u16 = 2;
pub(crate) const MV_CROUCH: u16 = 4;
pub(crate) const MV_FLY: u16 = 8;
pub(crate) const MV_SNEAK: u16 = 16;
pub(crate) const MV_HCOLL: u16 = 32;
pub(crate) const MV_SOFT: u16 = 64;
/// Отладочный полёт сквозь стены (MV_FLY при нём тоже стоит).
pub(crate) const MV_NOCLIP: u16 = 128;

/// Действие игрока (§3.3), по порядку. seq — номер предсказания: запись в своё зеркало мира откатывается или
/// подтверждается по Ack (§4.2).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum Act {
    // Между тиками (в кадре): клики окна и колесо — очередью до тика (ревью P2 «Сеть»).
    /// Клик по окну win (0 — инвентарь игрока): slot — номер слота окна (−999 вне окна, −1 мимо), action — SlotAction;
    /// hash — хеш окна после предсказания (сервер сверяет); rev — MP-5.
    Click { win: u8, rev: u32, slot: i16, button: u8, action: u8, hash: u32 },
    QuickMoveAll { win: u8, rev: u32, slot: i16, item: u16, damage: u16, hash: u32 },
    CloseWindow { win: u8 },
    /// Выбранная ячейка хотбара (колесо, 1–9): шлётся перед действием, которому она важна, и в конце тика.
    Hotbar(u8),
    // В тике, в порядке фазы ввода (tick_use).
    OpenInventory,
    Drop { all: bool },
    Swing { hand: u8 },
    BreakStart { pos: [i32; 3], face: u8, seq: u32 },
    BreakAbort { pos: [i32; 3], seq: u32 },
    BreakDone { pos: [i32; 3], seq: u32 },
    /// ПКМ по блоку рукой hand (0 главная, 1 вторая): at — точка попадания относительно клетки (как в Minecraft),
    /// inside — луч начался внутри формы.
    Use { hand: u8, pos: [i32; 3], face: u8, at: [f32; 3], inside: bool, seq: u32 },
    UseAir { hand: u8, seq: u32 },
    /// Средняя кнопка (выбор блока): перестановка в хотбаре.
    Pick { pos: [i32; 3] },
    Attack { target: u32 },
    TeleportAck { id: u32 },
    VelocityAck { id: u32 },
    /// Только при debug = true (локальный сервер).
    Debug(DebugCmd),
    /// Отпустил ПКМ с едой в руке: поедание прервано (ASP-12 §9). Начало поедания — UseAir.
    UseRelease,
    /// Кнопка окна машины (виджет Button окна win: «сброс бака» инфузора) — Mekanism, MEK-08 §11.1.
    WinButton { win: u8, widget: u8 },
    /// ПКМ ведром рукой hand (ASP-15 §5): pos и face — цель: пустое ведро — клетка источника, которую увидел его луч;
    /// полное — кликнутый блок и грань (жидкость ставится в соседнюю клетку).
    Bucket { hand: u8, pos: [i32; 3], face: u8, seq: u32 },
    /// ПКМ по мобу рукой hand (стрижка овцы ножницами; дальше — корм, доение): номер сущности.
    UseEntity { target: u32, hand: u8 },
    /// N / Shift+N: режим предмета в главной руке (конфигуратор Mekanism) вперёд или назад.
    ItemMode { back: bool },
    /// F: полёт выживания вкл / выкл (G16, ClientPlayerEntity.sendAbilitiesUpdate). Сервер ставит, если полёт разрешён
    /// (game::rules::may_fly), — до теневого тика этого хода.
    Flight(bool),
}

/// Отладка (клавиши H, K, J; адрес ?fly, проверки): клиент выполняет сразу (предсказание), сервер — в своём порядке.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum DebugCmd {
    /// Полёт сквозь стены (H).
    Fly(bool),
    Give { item: u16, count: i32 },
    /// Набор для приёмки (K).
    Kit,
    ClearInventory,
    /// Переставить ступни (сдвиг J, телепорт проверок).
    Teleport { pos: [f64; 3] },
    /// Поставить на землю у (x, z).
    Spawn { x: f64, z: f64 },
    /// Время суток — доля от полуночи.
    Time(f64),
    DayCycle(bool),
    /// Урон вида Generic через обычный путь (окно, броня, смерть) — проверка «урон N».
    Hurt { amount: f32 },
    /// Поставить здоровье (0 < hp ≤ 20) без окна и мигания — подготовка сцен и проверок.
    SetHp { hp: f32 },
    /// Площадка у ног для приёмки (1 — «сталь»: лава под тепловой генератор, место под кабель и инфузор, набор).
    Build(u8),
    /// gamerule randomTickSpeed: случайных клеток на секцию за тик (3 по умолчанию; для приёмки роста посевов — 300).
    TickSpeed(u32),
    /// Моб вида kind (game/mob: 0 свинья, 1 корова, 2 овца, 3 курица, 4 зомби) n штук у ступней pos — сцены и проверки.
    SpawnMob { kind: u8, pos: [f64; 3], n: u8 },
}

// ---------------------------------------------------------------- сервер → клиент

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum S2C {
    Welcome(Welcome),
    Reject { code: Reject, text: String },
    /// Один кадр на тик сервера.
    Bundle(Bundle),
    Kick { text: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    Outdated,
    NickTaken,
    BadNick,
    Full,
    Banned,
    GenMismatch,
    /// Ник закреплён за другим токеном (другой браузер или очищенное хранилище).
    BadToken,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Welcome {
    /// Номер своей сущности.
    pub(crate) you: u32,
    pub(crate) seed: u32,
    pub(crate) gen_version: u32,
    pub(crate) time: u64,
    pub(crate) day: u64,
    pub(crate) cycle: bool,
    pub(crate) me: SelfState,
    pub(crate) server_name: String,
    pub(crate) online: u16,
    pub(crate) max: u16,
    /// Отладка (полёт сквозь стены H, набор K, сдвиг J, время) разрешена.
    pub(crate) debug: bool,
}

/// Свой игрок при входе.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct SelfState {
    pub(crate) pos: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) fall: f32,
    pub(crate) on_ground: bool,
    pub(crate) flying: bool,
    pub(crate) crouch: bool,
    pub(crate) selected: u8,
    /// Непустые ячейки инвентаря: (плоский номер 0…40, стек).
    pub(crate) inv: Vec<(u8, ItemStack)>,
}

/// Кадр тика сервера: номер тика (время мира) и сообщения по порядку применения.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct Bundle {
    pub(crate) tick: u64,
    pub(crate) msgs: Vec<Msg>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum Msg {
    // Мир (§3.5).
    /// Куски с правками вокруг: (cx, cz, правок).
    EditIndex(Vec<(i32, i32, u32)>),
    /// Правки куска целиком: ключи клеток (world::local_key) — разностями по возрастанию, ячейки u32. Сжатие (zip) —
    /// когда понадобится (MP-1/2).
    ChunkEdits { cx: i32, cz: i32, edits: Vec<(u16, u32)> },
    Forget { cx: i32, cz: i32 },
    /// Живые правки куска за тик: (ключ клетки, ячейка).
    Blocks { cx: i32, cz: i32, list: Vec<(u16, u32)> },
    /// Предсказания до seq включительно решены.
    Ack { seq: u32 },
    BreakStage { who: u32, pos: [i32; 3], stage: i8 },
    // Сущности (§3.6).
    Spawn { id: u32, ent: Ent },
    Despawn(Vec<u32>),
    Moves(Vec<EntMove>),
    /// Ходы других игроков: состояние на конец их тика n (интерполяция у получателя — по n, §3.6).
    PlayerMoves(Vec<PlayerMove>),
    /// Анимация сущности: 0 — взмах рукой.
    Anim { id: u32, kind: u8 },
    ItemCount { id: u32, count: u8 },
    /// Подбор: летящая копия (3 тика) от точки from; stack — стек до подбора.
    Pickup { id: u32, by: u32, stack: ItemStack, from: [f64; 3], spin: u32 },
    // Свой игрок.
    Teleport { id: u32, pos: [f64; 3], yaw: f32, pitch: f32 },
    Velocity { id: u32, v: [f64; 3] },
    /// Инвентарь полной копией (MP-0/1; ревизии — MP-5): 41 ячейка по плоским номерам; выбранная — если её сменил сервер.
    Inventory { slots: Vec<ItemStack>, selected: Option<u8> },
    // Окна (§3.7): win 0 — инвентарь игрока, 1…255 — открытые блоком.
    WinOpen { win: u8, kind: u8, pos: Option<[i32; 3]>, rev: u32, slots: Vec<ItemStack> },
    /// Слоты окна (кроме ячеек инвентаря — те идут Inventory): номер слота окна, −1 — курсор.
    WinSlots { win: u8, rev: u32, list: Vec<(i16, ItemStack)> },
    WinAck { win: u8, rev: u32 },
    /// Числа окна (печь: горение, топливо всего, плавка, плавка всего — PropertyDelegate).
    WinData { win: u8, values: Vec<i32> },
    WinClose { win: u8 },
    ChestViewers { pos: [i32; 3], n: u8 },
    // Эффекты, время, люди.
    /// Событие (abi::Ev) с адресатом на сервере; свои предсказанные себе не шлются.
    Fx { code: u16, pos: [i32; 3], arg: i32 },
    Time { time: u64, day: u64, cycle: bool },
    Chat { kind: u8, from: u32, text: String },
    ListAdd { id: u32, nick: String, skin: u8 },
    ListRemove(u32),
    ListPing(Vec<(u32, u16)>),
    Ping { id: u32 },
    /// Ответ на Away { on: false } (MP-2, §1.3): пока игрок был «вне игры», сервер тикал его с пустым вводом. Полное
    /// кинематическое состояние сервера (то же, что в ClientTick) и номер его тика — клиент встаёт в него, подтверждает
    /// TeleportAck(id) и дальше тикает как обычно. Взгляд остаётся клиентский. Сервер отвечает на любой Away { on: false }.
    Resume { id: u32, n: u32, mv: Move },
    /// Своё здоровье, воздух, броня, горение, смерть (ASP-12): при входе и при каждом изменении. Клиент не предсказывает
    /// урон (как Minecraft): сердца, крен камеры и экран смерти идут от этого сообщения.
    Vitals(crate::game::health::Vitals),
    /// Поедание закончено сервером или отказано (по seq начавшего UseAir): клиент снимает «ем», если seq тот же.
    UseStop { seq: u32 },
    /// Свечение энергосетей (MEK-02 §14.8): на сеть — наименьший кабель и свечение 0…1000; полный список при изменении.
    /// Клиент раздаёт свечение кабелям сети обходом своего зеркала.
    NetGlow(Vec<([i32; 3], u16)>),
    /// Ходы мобов этого тика (только изменившиеся; номер тика — Bundle.tick): интерполяция у клиента по тикам сервера.
    MobMoves(Vec<MobMove>),
    /// Стороны машин Mekanism рядом (MEK-09 §5; подсветка граней конфигуратором): клетка и упаковка — 6 режимов
    /// предметов по 4 бита (лицо, лево, право, зад, верх, низ) | выталкивание << 24. Только изменившиеся.
    MekSides(Vec<([i32; 3], u32)>),
    /// Предметы в пути (M5, MEK-14 §21.2 п. 11): состояние стека при появлении и при каждом событии, которого клиент сам
    /// не выведет (новый путь, частичная вставка). Обычный сдвиг клиент делает сам (world/logistics.rs View).
    Transit(Vec<crate::world::logistics::TransitMsg>),
    /// Стеки больше не видны (доставлен, выпал, ушёл из известных кусков).
    TransitEnd(Vec<u32>),
    /// Режим игры (game/rules.rs GameMode: 0 выживание, 1 творческий, 3 наблюдатель) и права (0 игрок, 1 админ, 2 владелец)
    /// — при входе и при каждом изменении (G17). Права нужны клиенту только для подсказок команд: проверяет сервер.
    Mode { mode: u8, level: u8 },
    /// Свой полёт задал сервер (PlayerAbilities, G16): возрождение, отказ в полёте. Идёт до Teleport — ходы до TeleportAck
    /// не принимаются, расхождения нет.
    Flight(bool),
}

/// Вид строки чата (`Msg::Chat.kind`, G2). Игрок: `from` — номер отправителя, `text` — только его слова (клиент
/// складывает «<Ник> текст»). Остальные виды — готовая строка: вход, выход, смерть (survival зовёт
/// `ServerSim::announce(CHAT_DEATH, …)`), системное сервера, отказ (серым, только отправителю).
pub(crate) const CHAT_PLAYER: u8 = 0;
pub(crate) const CHAT_JOIN: u8 = 1;
pub(crate) const CHAT_LEAVE: u8 = 2;
pub(crate) const CHAT_DEATH: u8 = 3;
pub(crate) const CHAT_SYSTEM: u8 = 4;
pub(crate) const CHAT_DENY: u8 = 5;
/// Личное сообщение (/msg): готовая строка «Ник шепчет вам: …» / «Вы шепчете Нику: …» (G17).
pub(crate) const CHAT_WHISPER: u8 = 6;
/// Объявление /say: «[Ник] текст» (G17).
pub(crate) const CHAT_SAY: u8 = 7;

/// Сущность при появлении у клиента.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum Ent {
    /// Предмет: prev — позиция начала тика (отрисовка с долей тика), spin — возраст вращения, offset — uniqueOffset.
    Item { stack: ItemStack, pos: [f64; 3], prev: [f64; 3], vel: [f64; 3], spin: u32, offset: f32 },
    /// Падающий блок.
    Falling { cell: u32, pos: [f64; 3], prev: [f64; 3], vel: [f64; 3] },
    /// Другой игрок: ник, скин, состояние на конец его тика n.
    Player { nick: String, skin: u8, mv: PlayerMove },
    /// Моб (game/mob: MobKind) и его состояние на этот тик.
    Mob { kind: u8, mv: MobMove },
}

/// Состояние другого игрока на конец его тика n: ступни (f32 — для отрисовки хватает), взгляд, PM_* ниже.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlayerMove {
    pub(crate) id: u32,
    pub(crate) n: u32,
    pub(crate) pos: [f32; 3],
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) flags: u8,
}

pub(crate) const PM_GROUND: u8 = 1;
pub(crate) const PM_CROUCH: u8 = 2;
pub(crate) const PM_SPRINT: u8 = 4;
pub(crate) const PM_FLY: u8 = 8;
/// Игрок мёртв (чужие клиенты не рисуют тело после смерти).
pub(crate) const PM_DEAD: u8 = 16;

/// Состояние моба на тик сервера: ступни, поворот тела, голова (абсолютный yaw), тангаж головы, MM_* ниже, вариант
/// (цвет овцы).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct MobMove {
    pub(crate) id: u32,
    pub(crate) pos: [f32; 3],
    pub(crate) body: f32,
    pub(crate) head: f32,
    pub(crate) pitch: f32,
    pub(crate) flags: u8,
    pub(crate) var: u8,
}

pub(crate) const MM_GROUND: u8 = 1;
/// Мёртв: лежит набок 20 тиков, потом Despawn.
pub(crate) const MM_DEAD: u8 = 2;
pub(crate) const MM_BABY: u8 = 4;
/// Овца острижена.
pub(crate) const MM_SHEARED: u8 = 8;
/// Зомби поднял руки (погоня).
pub(crate) const MM_ARMS: u8 = 16;
/// Овца щиплет траву (голова вниз).
pub(crate) const MM_EAT: u8 = 32;
pub(crate) const MM_FIRE: u8 = 64;
pub(crate) const MM_WATER: u8 = 128;

/// Ход сущности за тик. MP-0 — абсолютная позиция (f64); сдвиги в 1/4096 блока — MP-1 (§3.6).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct EntMove {
    pub(crate) id: u32,
    pub(crate) pos: [f64; 3],
    pub(crate) spin: u32,
}

// ---------------------------------------------------------------- кодирование

#[derive(Debug)]
pub(crate) struct ProtoError(pub(crate) String);

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "пакет не разобрался: {}", self.0)
    }
}

pub(crate) fn encode<T: Serialize>(m: &T) -> Vec<u8> {
    postcard::to_allocvec(m).expect("postcard: запись в Vec не ошибается")
}

/// Разобрать кадр: весь целиком (лишние байты в конце — ошибка, не тихий обрыв).
pub(crate) fn decode<T: for<'a> Deserialize<'a>>(b: &[u8]) -> Result<T, ProtoError> {
    let (v, rest) = postcard::take_from_bytes::<T>(b).map_err(|e| ProtoError(e.to_string()))?;
    if !rest.is_empty() {
        return Err(ProtoError(format!("{} лишних байт", rest.len())));
    }
    Ok(v)
}

/// Правки куска по возрастанию ключа → разности ключей (кадр ChunkEdits).
pub(crate) fn delta_keys(edits: &[(u16, u32)]) -> Vec<(u16, u32)> {
    let mut prev = 0;
    edits
        .iter()
        .map(|&(k, c)| {
            let d = k.wrapping_sub(prev);
            prev = k;
            (d, c)
        })
        .collect()
}
/// Разности → ключи; None — повтор или ключ вне куска.
pub(crate) fn undelta_keys(edits: &[(u16, u32)], limit: u16) -> Option<Vec<(u16, u32)>> {
    let mut k = 0u16;
    let mut out = Vec::with_capacity(edits.len());
    for (i, &(d, c)) in edits.iter().enumerate() {
        if i > 0 && d == 0 {
            return None;
        }
        k = k.checked_add(d)?;
        if k >= limit {
            return None;
        }
        out.push((k, c));
    }
    Some(out)
}

#[cfg(test)]
mod proto_tests {
    use super::*;

    fn round<T: Serialize + for<'a> Deserialize<'a> + PartialEq + std::fmt::Debug>(m: &T) -> usize {
        let b = encode(m);
        let back: T = decode(&b).unwrap();
        assert_eq!(&back, m);
        b.len()
    }

    fn mv() -> Move {
        Move {
            pos: [8.5, 101.0, -3.25],
            vel: [0.0, -0.0784000015258789, 0.1],
            yaw: 123.5,
            pitch: -12.0,
            fall: 0.0,
            keys: 1 | 512,
            flags: MV_GROUND,
            jump_cd: 0,
            dtap: 3,
            fwd_in: 1.0,
            support: Some([8, 100, -4]),
        }
    }

    #[test]
    fn round_trip_all() {
        // Каждый вид сообщения туда и обратно без потерь (числа f64 — биты).
        let s = ItemStack { item: 300, count: 64, damage: 17 };
        let acts = vec![
            Act::Click { win: 3, rev: 9, slot: -999, button: 1, action: 5, hash: 0xDEAD_BEEF },
            Act::QuickMoveAll { win: 0, rev: 0, slot: 36, item: 7, damage: 0, hash: 1 },
            Act::CloseWindow { win: 255 },
            Act::Hotbar(8),
            Act::OpenInventory,
            Act::Drop { all: true },
            Act::Swing { hand: 1 },
            Act::BreakStart { pos: [-1, 0, i32::MAX], face: 5, seq: 1 },
            Act::BreakAbort { pos: [1, 127, -1], seq: 2 },
            Act::BreakDone { pos: [i32::MIN, 64, 0], seq: u32::MAX },
            Act::Use { hand: 0, pos: [3, 101, 2], face: 1, at: [0.5, 1.0, 0.25], inside: false, seq: 7 },
            Act::UseAir { hand: 1, seq: 8 },
            Act::Bucket { hand: 0, pos: [-5, 64, 9], face: 1, seq: 9 },
            Act::Pick { pos: [0, 0, 0] },
            Act::Attack { target: 42 },
            Act::TeleportAck { id: 1 },
            Act::VelocityAck { id: 2 },
            Act::Debug(DebugCmd::Fly(true)),
            Act::Debug(DebugCmd::Give { item: 5, count: -3 }),
            Act::Debug(DebugCmd::Kit),
            Act::Debug(DebugCmd::ClearInventory),
            Act::Debug(DebugCmd::Teleport { pos: [0.1, 0.2, f64::MIN_POSITIVE] }),
            Act::Debug(DebugCmd::Spawn { x: 8.0, z: -8.0 }),
            Act::Debug(DebugCmd::Time(0.3)),
            Act::Debug(DebugCmd::DayCycle(false)),
            Act::Debug(DebugCmd::Hurt { amount: 4.5 }),
            Act::Debug(DebugCmd::SetHp { hp: 7.0 }),
            Act::Debug(DebugCmd::TickSpeed(300)),
            Act::UseRelease,
            Act::UseEntity { target: 9, hand: 1 },
            Act::Debug(DebugCmd::SpawnMob { kind: 4, pos: [1.5, 64.0, -2.5], n: 3 }),
        ];
        let hello = Hello { proto: PROTO, build: BUILD.into(), nick: "Вася_1".into(), token: [7; 16], view: 8, probe: [1, 2, 3, u64::MAX] };
        for m in [
            C2S::Hello(hello),
            C2S::Tick(ClientTick { n: 77, acts: acts.clone(), mv: mv(), inv: 0x1234_5678 }),
            C2S::Acts(acts),
            C2S::Chat { text: "привет".into() },
            C2S::Pong { id: 3 },
            C2S::View { radius: 12 },
            C2S::Away { on: true },
            C2S::Respawn,
        ] {
            round(&m);
        }
        let me = SelfState {
            pos: [1.0, 2.0, 3.0],
            vel: [0.0; 3],
            yaw: 1.0,
            pitch: 2.0,
            fall: 0.5,
            on_ground: true,
            flying: false,
            crouch: true,
            selected: 4,
            inv: vec![(0, s), (40, ItemStack::of(1, 1))],
        };
        let msgs = vec![
            Msg::EditIndex(vec![(0, 0, 25), (-3, 5, 1)]),
            Msg::ChunkEdits { cx: -1, cz: 2, edits: vec![(0, 1), (128, 0x0002_0011)] },
            Msg::Forget { cx: 9, cz: -9 },
            Msg::Blocks { cx: 0, cz: 0, list: vec![(4100, 0), (32767, u32::MAX)] },
            Msg::Ack { seq: 17 },
            Msg::BreakStage { who: 2, pos: [1, 2, 3], stage: -1 },
            Msg::Spawn { id: 5, ent: Ent::Item { stack: s, pos: [0.5, 101.125, 0.5], prev: [0.5, 101.0, 0.5], vel: [0.1, 0.2, -0.1], spin: 3, offset: 2.5 } },
            Msg::Spawn { id: 6, ent: Ent::Falling { cell: 12, pos: [3.5, 110.0, 3.5], prev: [3.5, 110.04, 3.5], vel: [0.0, -0.04, 0.0] } },
            Msg::Despawn(vec![5, 6]),
            Msg::Moves(vec![EntMove { id: 5, pos: [1.0, 2.0, 3.0], spin: 9 }]),
            Msg::PlayerMoves(vec![PlayerMove { id: 2, n: 77, pos: [8.5, 101.0, -3.25], yaw: -720.5, pitch: 12.0, flags: PM_GROUND | PM_CROUCH }]),
            Msg::Anim { id: 2, kind: 0 },
            Msg::Spawn { id: 2, ent: Ent::Player { nick: "Бот".into(), skin: 3, mv: PlayerMove { id: 2, n: 1, pos: [0.0; 3], yaw: 0.0, pitch: 0.0, flags: 0 } } },
            Msg::ItemCount { id: 5, count: 63 },
            Msg::Pickup { id: 5, by: 1, stack: s, from: [0.0; 3], spin: 4 },
            Msg::Teleport { id: 1, pos: [0.0, 64.0, 0.0], yaw: 0.0, pitch: 0.0 },
            Msg::Velocity { id: 1, v: [0.4, 0.3608, 0.0] },
            Msg::Inventory { slots: vec![ItemStack::EMPTY; 41], selected: Some(3) },
            Msg::WinOpen { win: 1, kind: 3, pos: Some([0, 101, 2]), rev: 0, slots: vec![s; 27] },
            Msg::WinSlots { win: 1, rev: 1, list: vec![(-1, s), (0, ItemStack::EMPTY)] },
            Msg::WinAck { win: 1, rev: 2 },
            Msg::WinData { win: 1, values: vec![1600, 1600, 3, 200] },
            Msg::WinClose { win: 1 },
            Msg::ChestViewers { pos: [0, 101, 2], n: 1 },
            Msg::Fx { code: 12, pos: [0, 101, 2], arg: 950 },
            Msg::Time { time: 1 << 40, day: 6000, cycle: true },
            Msg::Chat { kind: 0, from: 1, text: "hi".into() },
            Msg::ListAdd { id: 1, nick: "Вася".into(), skin: 3 },
            Msg::ListRemove(1),
            Msg::ListPing(vec![(1, 45)]),
            Msg::Ping { id: 9 },
            Msg::Resume { id: 3, n: 4100, mv: mv() },
            Msg::Vitals(crate::game::health::Vitals { hp: 13.5, air: -20, armor: 15, burning: true, dead: true, death_time: 19, tilt: -42.5, cause: 6 }),
            Msg::UseStop { seq: 12 },
            Msg::Spawn { id: 7, ent: Ent::Mob { kind: 2, mv: MobMove { id: 7, pos: [1.5, 65.0, 2.5], body: 90.0, head: 100.0, pitch: -5.0, flags: MM_GROUND | MM_SHEARED, var: 3 } } },
            Msg::MobMoves(vec![MobMove { id: 7, pos: [1.6, 65.0, 2.5], body: 91.0, head: 100.0, pitch: 0.0, flags: MM_DEAD, var: 0 }]),
            Msg::PlayerMoves(vec![PlayerMove { id: 2, n: 78, pos: [0.0; 3], yaw: 0.0, pitch: 0.0, flags: PM_GROUND | PM_DEAD }]),
            Msg::Mode { mode: 3, level: 2 },
        ];
        for m in [
            S2C::Welcome(Welcome { you: 1, seed: 1337, gen_version: 2, time: 5, day: 5, cycle: true, me, server_name: "локальный".into(), online: 1, max: 32, debug: true }),
            S2C::Reject { code: Reject::GenMismatch, text: "генератор другой".into() },
            S2C::Reject { code: Reject::BadToken, text: "чужой ник".into() },
            S2C::Bundle(Bundle { tick: 123456, msgs }),
            S2C::Kick { text: "пока".into() },
        ] {
            round(&m);
        }
    }

    #[test]
    fn hello_head_survives_a_changed_hello() {
        // Начало Hello читается у любой версии: клиент другой сборки узнаётся, даже когда остальные поля не те.
        let hello = |proto, build: &str| C2S::Hello(Hello { proto, build: build.into(), nick: "Вася".into(), token: [9; 16], view: 8, probe: [1, 2, 3, 4] });
        let b = encode(&hello(PROTO, "abc1234"));
        assert_eq!(hello_head(&b), Some((PROTO, "abc1234".into())));
        // Хвост другой формы (поля после build изменились): целиком не разбирается, голова — да.
        let mut cut = b[..b.len() - 9].to_vec();
        cut.extend_from_slice(&[0xFF; 3]);
        assert!(decode::<C2S>(&cut).is_err());
        assert_eq!(hello_head(&cut), Some((PROTO, "abc1234".into())));
        // Не Hello и мусор — None.
        assert_eq!(hello_head(&encode(&C2S::Respawn)), None);
        assert_eq!(hello_head(&[]), None);
        assert_eq!(hello_head(&[0]), None);
        // Сверка: те же сборки — можно; другой протокол или номер — «обновите страницу»; «dev» не спорит.
        assert_eq!(outdated(PROTO, BUILD), None);
        assert_eq!(outdated(PROTO, "dev"), None);
        let text = outdated(PROTO + 1, BUILD).expect("протокол другой");
        assert!(text.contains("Обновите страницу"), "{text}");
        if BUILD != "dev" {
            let text = outdated(PROTO, "ffffff0").expect("сборка другая");
            assert!(text.contains("Обновите страницу") && text.contains("ffffff0"), "{text}");
        }
    }

    #[test]
    fn bad_frames_are_errors() {
        // Обрезанное, мусор и лишний хвост — ошибка разбора, не паника.
        let b = encode(&C2S::Tick(ClientTick { n: 1, acts: vec![Act::Hotbar(2)], mv: mv(), inv: 0 }));
        for cut in 1..b.len() {
            assert!(decode::<C2S>(&b[..cut]).is_err(), "обрезано до {cut}");
        }
        let mut long = b.clone();
        long.push(0);
        assert!(decode::<C2S>(&long).is_err());
        assert!(decode::<C2S>(&[200, 1, 2, 3]).is_err());
        assert!(decode::<S2C>(&[]).is_err());
        // Ключи правок: разности туда и обратно; повтор и выход за кусок — None.
        let e = vec![(3u16, 1u32), (200, 2), (32767, 3)];
        assert_eq!(undelta_keys(&delta_keys(&e), 32768), Some(e));
        assert_eq!(undelta_keys(&[(5, 1), (0, 2)], 32768), None);
        assert_eq!(undelta_keys(&[(40000, 1)], 32768), None);
    }

    #[test]
    fn typical_sizes() {
        // Размеры типичных кадров (для отчёта: cargo test typical_sizes -- --nocapture). Пределы — с запасом к оценке
        // design.md §3.9 (~75 байт тика клиента).
        let idle = encode(&C2S::Tick(ClientTick { n: 1200, acts: vec![], mv: mv(), inv: 0x9E37_79B9 }));
        let busy = encode(&C2S::Tick(ClientTick {
            n: 1201,
            acts: vec![Act::Hotbar(3), Act::BreakDone { pos: [12, 64, -40], seq: 310 }, Act::Swing { hand: 0 }],
            mv: mv(),
            inv: 0x9E37_79B9,
        }));
        let click = encode(&Act::Click { win: 1, rev: 0, slot: 37, button: 0, action: 0, hash: 0x9E37_79B9 });
        let empty = encode(&S2C::Bundle(Bundle { tick: 72_000, msgs: vec![] }));
        let mine = encode(&S2C::Bundle(Bundle {
            tick: 72_001,
            msgs: vec![
                Msg::Blocks { cx: 0, cz: -3, list: vec![(4100, 0)] },
                Msg::Ack { seq: 310 },
                Msg::Spawn { id: 40, ent: Ent::Item { stack: ItemStack::of(4, 1), pos: [12.6, 64.4, -39.7], prev: [12.6, 64.4, -39.7], vel: [0.05, 0.2, -0.03], spin: 0, offset: 1.7 } },
            ],
        }));
        let moves = encode(&Msg::Moves((0..10).map(|i| EntMove { id: 40 + i, pos: [12.6, 64.4, -39.7], spin: 100 }).collect()));
        let inv = encode(&Msg::Inventory { slots: vec![ItemStack::of(4, 64); 41], selected: None });
        let pm = encode(&Msg::PlayerMoves(vec![PlayerMove { id: 2, n: 1200, pos: [120.5, 64.0, -310.25], yaw: 93.5, pitch: 10.0, flags: PM_GROUND }]));
        println!(
            "размеры: тик клиента без действий {} Б, с ломанием {} Б; клик окна {} Б; кадр сервера пустой {} Б, \
             ломание (правка + Ack + предмет) {} Б; 10 ходов предметов {} Б; полный инвентарь {} Б; ход игрока {} Б",
            idle.len(),
            busy.len(),
            click.len(),
            empty.len(),
            mine.len(),
            moves.len(),
            inv.len(),
            pm.len()
        );
        assert!(idle.len() <= 90 && busy.len() <= 110 && empty.len() <= 8 && click.len() <= 16);
    }
}
