//! Значения хранилища, версия 1 (design.md §2.5). Значение = номер версии (varint u16) + тело postcard.
//!
//! Тела — свои структуры *V1, а не структуры игры: внутренности игры меняются свободно, формат — только новой версией
//! (V2 рядом с V1 и перевод V1 → V2 при чтении; старые значения читаются всегда). Номера блоков и предметов — как
//! есть: они только дописываются (mc/decisions.md), поэтому таблицы перевода не нужно.
//!
//! Почему postcard + serde, а не ручная запись (~300 строк, как предлагало ревью): тот же формат берёт протокол MP-0
//! (design.md §3.1) — одна сериализация для диска и сети; целые — varint (ключ клетки 2–3 байта, номер блока 1–2,
//! счётчики 1–5); формат postcard 1.0 зафиксирован; порченые байты дают ошибку разбора, а не панику; чистый Rust —
//! собирается и в wasm32, и нативно. Цена — derive (serde_derive) в сборке; в WASM без вызовов из api код не попадает.
//! f32/f64 пишутся как есть (4/8 байт) — биты позиций и скоростей не теряются, детерминизм после загрузки сохраняется.

use serde::{Deserialize, Serialize};

/// Раскладка ключей хранилища (meta, chunk/x,z, player/ник, journal) — в meta.format.
pub(crate) const FORMAT: u16 = 1;
pub(crate) const META_V: u16 = 1;
pub(crate) const CHUNK_V: u16 = 1;
/// 2 — MP-0: номер сущности игрока (общий счётчик с предметами: после перезапуска номера и фаза «раз в 4 тика» у
/// новых предметов те же, что без перезапуска). 3 — MP-2: хеш токена (ник закреплён за браузером). Версии 1 и 2
/// читаются (номер — новый при входе, токена нет — ник закрепится за первым, кто войдёт с токеном).
pub(crate) const PLAYER_V: u16 = 4;
pub(crate) const JOURNAL_V: u16 = 1;

/// Стек предметов: номер предмета, число, износ.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct StackV1 {
    pub(crate) item: u16,
    pub(crate) count: i32,
    pub(crate) damage: u16,
}

/// meta: мир целиком.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct MetaV1 {
    /// Раскладка ключей (FORMAT).
    pub(crate) format: u16,
    pub(crate) seed: u32,
    /// Версия генератора (gen::GEN_VERSION): мир с другой не грузится.
    pub(crate) gen_version: u32,
    /// Время мира в тиках, время суток 0…23999, идут ли сутки (Sim::time, day, day_cycle).
    pub(crate) time: u64,
    pub(crate) day: u64,
    pub(crate) day_cycle: bool,
    /// Состояния ГСЧ splitmix64: дроп и сущности, случайные тики, эффекты (у сервера эффектов не будет — 0).
    pub(crate) rng: u64,
    pub(crate) tick_rng: u64,
    pub(crate) fx_rng: u64,
    /// Счётчики: номер следующей сущности (общий для игроков, предметов, падающих блоков — MP-0; у предмета он же —
    /// фаза «раз в 4 тика»), последнего запланированного тика, падающих блоков (с MP-0 — тот же номер следующей
    /// сущности, поле оставлено ради версии 1), следующей блок-сущности.
    pub(crate) next_item: u32,
    pub(crate) tick_seq: u64,
    pub(crate) falling_seq: u64,
    pub(crate) be_seq: u64,
    /// Сколько правок сделано за жизнь мира (World::edit_count).
    pub(crate) edit_count: u32,
}

/// chunk/<cx>,<cz>: всё, что лежит в куске. Клетка куска — ключ (lz·16 + lx)·128 + y (world/mod.rs local_key).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct ChunkV1 {
    /// Правки по возрастанию ключа: (разность с прошлым ключом, первый — от 0; ячейка блок | состояние << 16).
    /// Разности столбца — по 1 байту.
    pub(crate) edits: Vec<(u16, u32)>,
    pub(crate) bes: Vec<BeV1>,
    /// Предметы на земле, точка которых в куске.
    pub(crate) items: Vec<ItemV1>,
    pub(crate) falling: Vec<FallingV1>,
    /// Запланированные тики клеток куска.
    pub(crate) ticks: Vec<TickV1>,
}

impl ChunkV1 {
    pub(crate) fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.bes.is_empty() && self.items.is_empty() && self.falling.is_empty() && self.ticks.is_empty()
    }
}

/// Блок-сущность: клетка куска, номер появления (порядок тика печей), содержимое.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct BeV1 {
    pub(crate) key: u16,
    pub(crate) seq: u64,
    pub(crate) data: BeDataV1,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) enum BeDataV1 {
    /// Вход, топливо, выход; горит ещё, на сколько было топливо, прогресс, время рецепта (AbstractFurnaceBlockEntity).
    Furnace { slots: [StackV1; 3], burn: i32, fuel_total: i32, cook: i32, cook_total: i32 },
    /// Непустые ячейки (номер, стек). Смотрящих и крышку не храним: окна после загрузки закрыты.
    Chest { slots: Vec<(u8, StackV1)> },
    /// Машина Mekanism (MEK-01 §10.1): вид (MekKind), непустые слоты, энергия, прогресс, бак (вещество, мБ), тепло
    /// (f64 — биты как есть), «активна» (логика, вид, задержка). Шкала ядра куба и «выработано за тик» не храним.
    Mek { kind: u8, slots: Vec<(u8, StackV1)>, energy: u64, progress: u32, tank: (u8, u32), heat: f64, active: (bool, bool, u8) },
    /// Машина Mekanism с M3 (новые записи — только так; старые Mek читаются): то же + стороны предметов (6 режимов по
    /// относительным граням), выталкивание, улучшения (скорость, энергия).
    MekM3 {
        kind: u8,
        slots: Vec<(u8, StackV1)>,
        energy: u64,
        progress: u32,
        tank: (u8, u32),
        heat: f64,
        active: (bool, bool, u8),
        sides: [u8; 6],
        eject: bool,
        upgrades: [u8; 2],
    },
    /// Машина с M4 (новые записи — только так): то же + улучшения газа, баки газов сепаратора (вещество, мБ) и их режимы,
    /// израсходованный кислород операции очистителя.
    MekM4 {
        kind: u8,
        slots: Vec<(u8, StackV1)>,
        energy: u64,
        progress: u32,
        tank: (u8, u32),
        heat: f64,
        active: (bool, bool, u8),
        sides: [u8; 6],
        eject: bool,
        upgrades: [u8; 3],
        gas: [(u8, u32); 2],
        gas_mode: [u8; 2],
        gas_used: u32,
    },
}

/// mek_items: машины Mekanism в предметах (world/mek/items.rs) — следующий номер и записи «номер, энергия, непустые
/// слоты, бак (вещество, мБ)».
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct MekItemsV1 {
    pub(crate) next: u16,
    pub(crate) list: Vec<(u16, u64, Vec<(u8, StackV1)>, (u8, u32))>,
}

/// Предмет на земле (ItemEntity): всё, что влияет на следующие тики, и сдвиг вращения для вида.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct ItemV1 {
    pub(crate) id: u32,
    pub(crate) stack: StackV1,
    pub(crate) pos: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) age: u32,
    pub(crate) item_age: i32,
    pub(crate) spin_age: u32,
    pub(crate) pickup_delay: i16,
    pub(crate) thrown: bool,
    pub(crate) offset: f32,
}

/// Падающий блок (FallingBlockEntity).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct FallingV1 {
    /// Номер сущности (порядок тика).
    pub(crate) seq: u64,
    pub(crate) cell: u32,
    pub(crate) pos: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) age: u32,
}

/// Запланированный тик: клетка куска, время срабатывания (время мира), номер (порядок при равном времени), блок.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct TickV1 {
    pub(crate) key: u16,
    pub(crate) time: u64,
    pub(crate) seq: u64,
    pub(crate) block: u16,
}

/// player/<ник>: игрок. Здоровья пока нет (ASP-12) — появится в PlayerV2.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct PlayerV1 {
    /// Ник как его ввели (в ключе — строчными).
    pub(crate) nick: String,
    /// Ступни, скорость, взгляд (градусы Minecraft), на земле, fallDistance, полёт, присед.
    pub(crate) pos: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) on_ground: bool,
    pub(crate) fall: f32,
    pub(crate) flying: bool,
    pub(crate) crouch: bool,
    /// Выбранная ячейка хотбара 0…8.
    pub(crate) selected: u8,
    /// Непустые ячейки инвентаря (0–35 основные, 36–39 броня ноги…голова, 40 вторая рука).
    pub(crate) inv: Vec<(u8, StackV1)>,
    /// Окно при записи было открыто: курсор и сетка крафта. При загрузке ложатся в инвентарь (как при закрытии окна),
    /// не влезло — выпадает вперёд.
    pub(crate) carried: Vec<StackV1>,
}

/// player/<ник>, версия 2: то же и номер сущности игрока.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct PlayerV2 {
    pub(crate) id: u32,
    pub(crate) p: PlayerV1,
}

/// player/<ник>, версия 3: то же и хеш токена (save/token.rs). None — ник никем не закреплён (запись версий 1–2 или игрок
/// входил «гостем» без токена).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct PlayerV3 {
    pub(crate) id: u32,
    pub(crate) token: Option<[u8; 32]>,
    pub(crate) p: PlayerV1,
}

/// player/<ник>, версия 4 (G17): то же и режим игры (game/rules.rs GameMode: 0 выживание, 1 творческий, 3 наблюдатель).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct PlayerV4 {
    pub(crate) id: u32,
    pub(crate) token: Option<[u8; 32]>,
    pub(crate) mode: u8,
    pub(crate) p: PlayerV1,
}

/// Значение: версия + тело.
pub(crate) fn encode<T: Serialize>(ver: u16, body: &T) -> Vec<u8> {
    postcard::to_allocvec(&(ver, body)).expect("postcard: запись в Vec не ошибается")
}

/// Версия значения и тело после неё.
pub(crate) fn version(bytes: &[u8]) -> Result<(u16, &[u8]), postcard::Error> {
    postcard::take_from_bytes::<u16>(bytes)
}

/// Тело целиком (хвост после тела — ошибка: значит, читаем не ту версию).
pub(crate) fn body<'a, T: Deserialize<'a>>(rest: &'a [u8]) -> Result<T, String> {
    match postcard::take_from_bytes::<T>(rest) {
        Ok((v, [])) => Ok(v),
        Ok((_, tail)) => Err(format!("лишние {} байт после тела", tail.len())),
        Err(e) => Err(e.to_string()),
    }
}

/// logistics (M5): режимы сторон передатчиков (2 бита на сторону), цвета транспортёров, вещество в сетях труб и трубок
/// (любой член, вещество, мБ), предметы в пути, стороны газов сепараторов в мире и в предметах (номер записи mek_items).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct LogisticsV1 {
    pub(crate) modes: Vec<([i32; 3], u16)>,
    pub(crate) colors: Vec<([i32; 3], u8)>,
    pub(crate) subs: Vec<([i32; 3], u8, u64)>,
    pub(crate) stacks: Vec<TransitV1>,
    pub(crate) gas_sides: Vec<([i32; 3], [u8; 6])>,
    pub(crate) item_gas_sides: Vec<(u16, [u8; 6])>,
}

/// Стек в пути: клетка, откуда вошёл, прогресс 0…100, предмет, цвет, цель (0 — к цели, 1 — домой, 2 — простой), дом,
/// направление простоя. Путь не пишется — считается заново в первом тике (как мод).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct TransitV1 {
    pub(crate) at: [i32; 3],
    pub(crate) from: [i32; 3],
    pub(crate) progress: u16,
    pub(crate) stack: StackV1,
    pub(crate) color: u8,
    pub(crate) goal: u8,
    pub(crate) home: Option<[i32; 3]>,
    pub(crate) idle_dir: Option<u8>,
}
