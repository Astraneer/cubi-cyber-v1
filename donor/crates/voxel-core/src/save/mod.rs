//! Сохранение мира (Ф2, design.md §2.5): ключ-значение через типаж Storage (storage.rs). Это формат и хранилище
//! будущего сервера: игра только сетевая — одиночной игры нет, мир живёт на сервере; IndexedDB не делаем.
//!
//! Ключи (значения — format.rs):
//!   meta             — версия раскладки, сид, GEN_VERSION, время и сутки, три ГСЧ, счётчики номеров, число правок;
//!   chunk/<cx>,<cz>  — правки куска (совпавшие с генератором удалены ещё при записи в мир — world/mod.rs), блок-сущности
//!                      (печь, сундук), предметы на земле, падающие блоки, запланированные тики клеток куска;
//!   player/<ник>     — номер сущности, хеш токена (ник закреплён за браузером), позиция, скорость, взгляд, полёт, присед,
//!                      инвентарь, выбранная ячейка; ник в ключе — строчными;
//!   grid             — запасы энергосетей Mekanism: по записи на сеть «член сети, запас» (сеть собирается из кабелей);
//!   mek_items        — машины Mekanism в предметах: номер записи (damage стека машины) → энергия, слоты, бак;
//!   journal          — служебный: пачка записи целиком, пока она пишется (ниже).
//! Не сохраняется производное и вид: кэш областей и индекс моделей (собираются из правок), смотрящие и крышки
//! сундуков, рука, HUD, ломание, ввод, летящие копии подбора, открытое окно (его курсор и сетка — в записи игрока),
//! граф сетей Mekanism (собирается заново по кабелям в правках; хранятся только запасы — ключ grid).
//!
//! Порядок, от которого зависит детерминизм, восстанавливают номера: предметы и падающие — по номеру сущности (общий
//! счётчик, у игрока — свой номер в записи), печи — по номеру появления, тики — по (время, номер). Поэтому после загрузки
//! мир тикает так же, как без сохранения (save/tests.rs: отпечаток состояния и продолжение тиков).
//!
//! Пишет и грузит сервер (game/server.rs: ServerSim): мир и все игроки (вошедшие и ждущие); загруженные игроки ждут
//! входа в ServerSim::parked (load_all — все записи player/*; load — одна, для проверок).
//!
//! Запись идёт пачкой (Batch): пачка целиком — значением «journal» (у FileStorage атомарно: tmp + rename), затем ключи по
//! одному, затем journal удаляется. Оборвалось посреди — загрузка дописывает journal: мир всегда целиком прежний или
//! целиком новый, предмет не задвоится между двумя кусками. Пачку собирает поток симуляции (collect_*), писать её можно
//! в другом потоке (Batch::write) — так задумано для сервера (design.md §2.4).
//!
//! save_dirty пишет только изменённое: куски с новыми правками или тронутыми блок-сущностями (WorldData::take_unsaved),
//! куски с предметами, падающими и тиками сейчас или при прошлой записи (сущности уходят в соседние куски), игрока и
//! meta; значение, совпавшее с уже записанным (по хешу), не пишется.

pub(crate) mod format;
pub(crate) mod storage;
pub(crate) mod token;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod writer;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;

use serde::{Deserialize, Serialize};

use crate::cell::{CS, H};
use crate::game::entity::{FallingBlock, ItemEntity};
use crate::game::inventory::Inventory;
use crate::game::server::{ServerPlayer, ServerSim};
use crate::game::{GameRng, Ids};
use crate::gen::GEN_VERSION;
use crate::registry::blocks::MekKind;
use crate::world::blockentity::{BlockEntity, Chest, Furnace};
use crate::world::mek::items::{MekItem, MekItems};
use crate::world::mek::{ActiveView, MekBe, Tank};
use crate::world::stack::ItemStack;
use crate::world::{chunk_of, local_key, local_pos, World, CHUNK_CELLS};
use format::*;
pub(crate) use storage::Storage;

pub(crate) const META: &str = "meta";
pub(crate) const JOURNAL: &str = "journal";
pub(crate) const CHUNK_PREFIX: &str = "chunk/";
pub(crate) const PLAYER_PREFIX: &str = "player/";
/// Запасы энергосетей Mekanism: [(член сети, запас)] (world/grid.rs saved / restore).
pub(crate) const GRID: &str = "grid";
pub(crate) const GRID_V: u16 = 1;
/// Машины Mekanism в предметах: номер (damage стека) → энергия, слоты, бак (world/mek/items.rs).
pub(crate) const MEK_ITEMS: &str = "mek_items";
pub(crate) const MEK_ITEMS_V: u16 = 1;
/// M3: стороны, выталкивание и улучшения машин в предметах — отдельным ключом (старые записи mek_items читаются как есть).
pub(crate) const MEK_ITEMS3: &str = "mek_items3";
pub(crate) const MEK_ITEMS3_V: u16 = 1;
type MekItems3V1 = Vec<(u16, [u8; 6], bool, [u8; 2])>;
/// M4: улучшения газа, баки газов и их режимы машин в предметах — своим ключом (старые записи читаются как есть).
pub(crate) const MEK_ITEMS4: &str = "mek_items4";
type MekItems4V1 = Vec<(u16, u8, [(u8, u32); 2], [u8; 2])>;
/// M5 «Логистика»: режимы сторон и цвета передатчиков, вещество в трубах и трубках, предметы в пути, стороны газов
/// сепараторов (в мире и в предметах) — одной записью (world/grid.rs, world/logistics.rs). Нет записи — старый мир.
pub(crate) const LOGISTICS: &str = "logistics";
pub(crate) const LOGISTICS_V: u16 = 1;

/// Ключ куска: «chunk/-3,5».
pub(crate) fn chunk_key(c: (i32, i32)) -> String {
    format!("{CHUNK_PREFIX}{},{}", c.0, c.1)
}
/// Кусок по ключу «chunk/<cx>,<cz>».
pub(crate) fn parse_chunk_key(key: &str) -> Option<(i32, i32)> {
    let (x, z) = key.strip_prefix(CHUNK_PREFIX)?.split_once(',')?;
    let c = (x.parse().ok()?, z.parse().ok()?);
    (chunk_key(c) == key).then_some(c)
}
/// Ключ игрока: «player/<ник строчными>». Ник — 1…32 символа без «/» и управляющих (правила ников — у входа, MP-1).
pub(crate) fn player_key(nick: &str) -> Result<String, SaveError> {
    let n = nick.chars().count();
    if n == 0 || n > 32 || nick.contains('/') || nick.chars().any(char::is_control) {
        return Err(SaveError::BadKey(nick.to_string()));
    }
    let key = format!("{PLAYER_PREFIX}{}", nick.to_lowercase());
    storage::check_key(&key).map_err(|_| SaveError::BadKey(nick.to_string()))?;
    Ok(key)
}

#[derive(Debug)]
pub(crate) enum SaveError {
    Io(io::Error),
    /// В хранилище нет meta — мира нет.
    NoWorld,
    /// Значение не разобралось.
    Corrupt { key: String, msg: String },
    /// Версия значения или раскладки новее, чем знает эта сборка.
    Version { key: String, ver: u16 },
    /// Мир другого генератора.
    GenVersion { saved: u32, ours: u32 },
    BadKey(String),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "хранилище: {e}"),
            SaveError::NoWorld => write!(f, "в хранилище нет мира (нет meta)"),
            SaveError::Corrupt { key, msg } => write!(f, "«{key}» не читается: {msg}"),
            SaveError::Version { key, ver } => write!(f, "«{key}»: версия {ver} новее этой сборки"),
            SaveError::GenVersion { saved, ours } => write!(f, "мир генератора {saved}, а у сборки {ours} — нужен новый мир"),
            SaveError::BadKey(k) => write!(f, "негодный ключ «{k}»"),
        }
    }
}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        SaveError::Io(e)
    }
}

/// Пачка записи: ключ → новое значение или удаление.
#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct Batch {
    pub(crate) ops: Vec<(String, Option<Vec<u8>>)>,
}

impl Batch {
    /// Записать атомарно (см. шапку). Одно значение — сразу, оно атомарно и без журнала.
    pub(crate) fn write(&self, st: &mut dyn Storage) -> io::Result<()> {
        if self.ops.len() > 1 {
            st.put(JOURNAL, &encode(JOURNAL_V, self))?;
        }
        self.apply(st)?;
        if self.ops.len() > 1 {
            st.delete(JOURNAL)?;
        }
        Ok(())
    }
    fn apply(&self, st: &mut dyn Storage) -> io::Result<()> {
        for (k, v) in &self.ops {
            match v {
                Some(b) => st.put(k, b)?,
                None => st.delete(k)?,
            }
        }
        Ok(())
    }
    /// Сколько значений пишется, сколько удаляется, байт.
    pub(crate) fn report(&self) -> SaveReport {
        let mut r = SaveReport::default();
        for (_, v) in &self.ops {
            match v {
                Some(b) => {
                    r.puts += 1;
                    r.bytes += b.len();
                }
                None => r.deletes += 1,
            }
        }
        r
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub(crate) struct SaveReport {
    pub(crate) puts: usize,
    pub(crate) deletes: usize,
    pub(crate) bytes: usize,
}

/// FNV-1a: отличить новое значение от уже записанного.
fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
}

/// Что лежит в хранилище (по хешам) и какие куски были «живыми» при прошлой записи.
#[derive(Default)]
pub(crate) struct Saver {
    /// Ключ → хеш последнего записанного значения.
    written: HashMap<String, u64>,
    /// Куски с предметами, падающими или тиками при прошлой записи.
    lively: HashSet<(i32, i32)>,
    /// Прошлая запись не удалась: следующая save_dirty пишет всё (save_all).
    full: bool,
    /// Записи игроков, которые load_all не смог прочитать (порча, версия новее): пропущены, файлы не тронуты.
    pub(crate) unreadable: Vec<String>,
}

/// Содержимое куска, кроме правок и блок-сущностей, собранное по всему миру.
#[derive(Default)]
struct Lively {
    items: Vec<ItemV1>,
    falling: Vec<FallingV1>,
    ticks: Vec<TickV1>,
}

impl Saver {
    pub(crate) fn new() -> Saver {
        Saver::default()
    }

    /// Полная запись (SIGTERM, первая запись в новую папку): все куски мира, игроки, meta; куски хранилища, которых в
    /// мире больше нет (правки вернулись к генератору), удаляются.
    pub(crate) fn save_all(&mut self, g: &ServerSim, st: &mut dyn Storage) -> Result<SaveReport, SaveError> {
        let existing = st.list(CHUNK_PREFIX)?;
        let b = self.collect(g, Some(&existing))?;
        self.finish(b, st)
    }

    /// Автосохранение: только изменённое (см. шапку).
    pub(crate) fn save_dirty(&mut self, g: &ServerSim, st: &mut dyn Storage) -> Result<SaveReport, SaveError> {
        if self.full {
            return self.save_all(g, st);
        }
        let b = self.collect_dirty(g)?;
        self.finish(b, st)
    }

    /// Пачка автосохранения (для записи в другом потоке). Хеши записанного обновляются сразу: не удалась запись —
    /// вызвать failed().
    pub(crate) fn collect_dirty(&mut self, g: &ServerSim) -> Result<Batch, SaveError> {
        self.collect(g, None)
    }

    /// Пачка полной записи (для записи в другом потоке): куски хранилища, которых в мире больше нет, удаляются. st —
    /// чтение (list); писать в него параллельно с этим вызовом нельзя — вызывающий сначала дожидается потока записи.
    pub(crate) fn collect_full(&mut self, g: &ServerSim, st: &dyn Storage) -> Result<Batch, SaveError> {
        let existing = st.list(CHUNK_PREFIX)?;
        self.collect(g, Some(&existing))
    }

    /// Пачка из записей игроков nicks (вход закрепил токен, выход унёс инвентарь): не ждать автосохранения. Нет такого
    /// игрока — пропуск. Значение, совпавшее с записанным, не пишется.
    pub(crate) fn collect_players(&mut self, g: &ServerSim, nicks: &[String]) -> Result<Batch, SaveError> {
        let mut ops = Vec::new();
        for nick in nicks {
            let key = player_key(nick)?;
            if let Some(p) = g.players.iter().chain(g.parked.iter()).find(|p| player_key(&p.nick).is_ok_and(|k| k == key)) {
                self.put(&mut ops, key, encode(PLAYER_V, &player_v4(p)), false);
            }
        }
        Ok(Batch { ops })
    }

    /// Мир ещё не лежит в хранилище: загрузки не было (новая папка).
    pub(crate) fn is_fresh(&self) -> bool {
        !self.written.contains_key(META)
    }

    /// Нужна ли полная запись (прошлая не удалась); флаг снимается — дальше его выставит failed(), если и эта не удастся.
    pub(crate) fn take_full(&mut self) -> bool {
        std::mem::take(&mut self.full)
    }

    /// Запись пачки не удалась: что лежит в хранилище, неизвестно — следующая save_dirty пишет всё.
    pub(crate) fn failed(&mut self) {
        self.full = true;
        self.written.clear();
    }

    fn finish(&mut self, b: Batch, st: &mut dyn Storage) -> Result<SaveReport, SaveError> {
        match b.write(st) {
            Ok(()) => {
                self.full = false;
                Ok(b.report())
            }
            Err(e) => {
                self.failed();
                Err(e.into())
            }
        }
    }

    /// Положить в пачку значение ключа: all — всегда, иначе — если отличается от записанного.
    fn put(&mut self, ops: &mut Vec<(String, Option<Vec<u8>>)>, key: String, bytes: Vec<u8>, all: bool) {
        let h = hash(&bytes);
        if all || self.written.get(&key) != Some(&h) {
            self.written.insert(key.clone(), h);
            ops.push((key, Some(bytes)));
        }
    }

    /// Собрать пачку. existing — куски, что лежат в хранилище (полная запись), None — автосохранение.
    fn collect(&mut self, sv: &ServerSim, existing: Option<&[String]>) -> Result<Batch, SaveError> {
        let all = existing.is_some();
        let g = sv;
        let mut w = g.sim.world.d.borrow_mut();
        let unsaved = w.take_unsaved();
        // Предметы, падающие и тики — по кускам.
        let mut lively: HashMap<(i32, i32), Lively> = HashMap::new();
        for e in g.sim.items.list.iter().filter(|e| !e.removed) {
            lively.entry(chunk_at(e.pos)).or_default().items.push(item_v1(e));
        }
        for f in g.sim.falling.iter().filter(|f| !f.removed) {
            lively.entry(chunk_at(f.pos)).or_default().falling.push(falling_v1(f));
        }
        let tick_chunks: Vec<(i32, i32)> = g.sim.ticks.chunks().collect();
        for c in tick_chunks {
            let ticks: Vec<TickV1> = g
                .sim
                .ticks
                .chunk_entries(c.0, c.1)
                .into_iter()
                .filter(|e| e.2[1] >= 0 && e.2[1] < H)
                .map(|(time, seq, p, block)| TickV1 { key: local_key(p[0].rem_euclid(CS), p[1], p[2].rem_euclid(CS)), time, seq, block })
                .collect();
            if !ticks.is_empty() {
                lively.entry(c).or_default().ticks = ticks;
            }
        }
        // Блок-сущности — по кускам.
        let mut bes: HashMap<(i32, i32), Vec<BeV1>> = HashMap::new();
        for (p, seq, be) in w.be().iter() {
            if p[1] < 0 || p[1] >= H {
                continue;
            }
            let key = local_key(p[0].rem_euclid(CS), p[1], p[2].rem_euclid(CS));
            bes.entry(chunk_of(p[0], p[2])).or_default().push(BeV1 { key, seq, data: be_v1(be) });
        }
        // Какие куски писать (по порядку — пачка детерминирована).
        let mut cand: BTreeSet<(i32, i32)> = unsaved.into_iter().collect();
        cand.extend(lively.keys().copied());
        cand.extend(self.lively.iter().copied());
        let mut stored: HashSet<String> = HashSet::new();
        if let Some(ex) = existing {
            cand.extend(w.edited_chunks());
            cand.extend(bes.keys().copied());
            for k in ex {
                // Чужие ключи под chunk/ не трогаем (загрузка на них остановится — пусть смотрит человек).
                if let Some(c) = parse_chunk_key(k) {
                    cand.insert(c);
                    stored.insert(k.clone());
                }
            }
        }
        let mut ops = Vec::new();
        for c in cand {
            let key = chunk_key(c);
            let mut rec = ChunkV1 { edits: delta(&w.chunk_edits(c.0, c.1)), ..Default::default() };
            if let Some(mut b) = bes.remove(&c) {
                b.sort_by_key(|b| b.key);
                rec.bes = b;
            }
            if let Some(l) = lively.get(&c) {
                rec.items = l.items.clone();
                rec.falling = l.falling.clone();
                rec.ticks = l.ticks.clone();
            }
            if rec.is_empty() {
                if self.written.remove(&key).is_some() || stored.contains(&key) {
                    ops.push((key, None));
                }
            } else {
                self.put(&mut ops, key, encode(CHUNK_V, &rec), all);
            }
        }
        self.lively = lively.into_keys().collect();
        drop(w);
        for p in sv.players.iter().chain(sv.parked.iter()) {
            self.put(&mut ops, player_key(&p.nick)?, encode(PLAYER_V, &player_v4(p)), all);
        }
        self.put(&mut ops, META.to_string(), encode(META_V, &meta_v1(sv)), all);
        // Энергосети (решение после MEK-02): одна запись на сеть — «любой член, запас»; граф собирается при загрузке.
        let grid = sv.sim.world.d.borrow().grid.saved();
        self.put(&mut ops, GRID.to_string(), encode(GRID_V, &grid), all);
        self.put(&mut ops, MEK_ITEMS.to_string(), encode(MEK_ITEMS_V, &mek_items_v1(&sv.sim.mek_items)), all);
        let m3: MekItems3V1 =
            sv.sim.mek_items.saved().1.into_iter().map(|(k, d)| (k, d.sides, d.eject, [d.upgrades[0], d.upgrades[1]])).collect();
        self.put(&mut ops, MEK_ITEMS3.to_string(), encode(MEK_ITEMS3_V, &m3), all);
        let m4: MekItems4V1 = sv
            .sim
            .mek_items
            .saved()
            .1
            .into_iter()
            .map(|(k, d)| (k, d.upgrades[2], d.gas.map(|t| (t.sub, t.amount)), d.gas_mode))
            .collect();
        self.put(&mut ops, MEK_ITEMS4.to_string(), encode(MEK_ITEMS3_V, &m4), all);
        let lg = logistics_v1(sv);
        self.put(&mut ops, LOGISTICS.to_string(), encode(LOGISTICS_V, &lg), all);
        Ok(Batch { ops })
    }

    /// Загрузить мир и запись игрока nick (ждёт входа в parked; нет записи — при входе встанет у точки появления).
    /// Недописанная пачка (journal) сначала дописывается.
    pub(crate) fn load(st: &mut dyn Storage, nick: &str) -> Result<(ServerSim, Saver), SaveError> {
        Saver::load_with(st, Players::One(nick))
    }

    /// Загрузить мир и все записи player/* (сервер): игроки ждут входа в parked, вошедший берёт свою запись.
    pub(crate) fn load_all(st: &mut dyn Storage) -> Result<(ServerSim, Saver), SaveError> {
        Saver::load_with(st, Players::All)
    }

    fn load_with(st: &mut dyn Storage, which: Players) -> Result<(ServerSim, Saver), SaveError> {
        if let Some(j) = st.get(JOURNAL)? {
            let b: Batch = decode(JOURNAL, &j, JOURNAL_V)?;
            b.apply(st)?;
            st.delete(JOURNAL)?;
        }
        let mut saver = Saver::new();
        let mb = st.get(META)?.ok_or(SaveError::NoWorld)?;
        let meta: MetaV1 = decode(META, &mb, META_V)?;
        if meta.format > FORMAT {
            return Err(SaveError::Version { key: META.into(), ver: meta.format });
        }
        if meta.gen_version != GEN_VERSION {
            return Err(SaveError::GenVersion { saved: meta.gen_version, ours: GEN_VERSION });
        }
        saver.written.insert(META.into(), hash(&mb));
        let world = World::new(meta.seed);
        let mut g = ServerSim::new(world.share());
        let mut items = Vec::new();
        let mut falling = Vec::new();
        {
            let mut w = world.d.borrow_mut();
            for key in st.list(CHUNK_PREFIX)? {
                let c = parse_chunk_key(&key).ok_or_else(|| SaveError::BadKey(key.clone()))?;
                let Some(bytes) = st.get(&key)? else { continue };
                let rec: ChunkV1 = decode(&key, &bytes, CHUNK_V)?;
                let corrupt = |msg: &str| SaveError::Corrupt { key: key.clone(), msg: msg.into() };
                let edits = undelta(&rec.edits).ok_or_else(|| corrupt("ключи правок"))?;
                let mut list = Vec::with_capacity(rec.bes.len());
                for b in &rec.bes {
                    list.push((cell_pos(c, b.key).ok_or_else(|| corrupt("ключ блок-сущности"))?, b.seq, be_from(&b.data)));
                }
                w.load_chunk(c.0, c.1, &edits, list);
                for t in &rec.ticks {
                    let p = cell_pos(c, t.key).ok_or_else(|| corrupt("ключ тика"))?;
                    g.sim.ticks.restore(t.time, t.seq, p, t.block);
                }
                items.extend(rec.items.iter().map(item_from));
                falling.extend(rec.falling.iter().map(falling_from));
                if !(rec.items.is_empty() && rec.falling.is_empty() && rec.ticks.is_empty()) {
                    saver.lively.insert(c);
                }
                saver.written.insert(key, hash(&bytes));
            }
            let n = w.edits_len() as u32;
            w.set_edit_count(meta.edit_count.max(n));
            w.be_mut().set_next_seq(meta.be_seq);
            w.take_unsaved(); // загруженное и так лежит в хранилище
            // Сети — заново из кабелей в правках, затем запасы по записям «член сети, запас».
            w.enable_grid();
            w.commit_grid();
            if let Some(gb) = st.get(GRID)? {
                let list: Vec<([i32; 3], u64)> = decode(GRID, &gb, GRID_V)?;
                w.grid.restore(&list);
                saver.written.insert(GRID.into(), hash(&gb));
            }
        }
        items.sort_by_key(|e: &ItemEntity| e.id);
        falling.sort_by_key(|f: &FallingBlock| f.id);
        let s = &mut g.sim;
        if let Some(b) = st.get(MEK_ITEMS)? {
            let v: MekItemsV1 = decode(MEK_ITEMS, &b, MEK_ITEMS_V)?;
            mek_items_from(&mut s.mek_items, v);
            saver.written.insert(MEK_ITEMS.into(), hash(&b));
        }
        if let Some(b) = st.get(MEK_ITEMS3)? {
            let v: MekItems3V1 = decode(MEK_ITEMS3, &b, MEK_ITEMS3_V)?;
            for (k, sides, eject, upgrades) in v {
                if let Some(d) = s.mek_items.get_mut(k) {
                    d.sides = sides;
                    d.eject = eject;
                    d.upgrades = [upgrades[0], upgrades[1], 0];
                }
            }
            saver.written.insert(MEK_ITEMS3.into(), hash(&b));
        }
        if let Some(b) = st.get(MEK_ITEMS4)? {
            let v: MekItems4V1 = decode(MEK_ITEMS4, &b, MEK_ITEMS3_V)?;
            for (k, gas_up, gas, gas_mode) in v {
                if let Some(d) = s.mek_items.get_mut(k) {
                    d.upgrades[2] = gas_up;
                    d.gas = gas.map(|(sub, amount)| Tank { sub, amount });
                    d.gas_mode = gas_mode;
                }
            }
            saver.written.insert(MEK_ITEMS4.into(), hash(&b));
        }

        let next = items.iter().map(|e| e.id).chain(falling.iter().map(|f| f.id)).map(|id| id.wrapping_add(1)).fold(meta.next_item, u32::max);
        s.ids = Ids(next.max(meta.falling_seq.min(u32::MAX as u64) as u32));
        s.items.list = items;
        s.falling = falling;
        s.ticks.set_seq(meta.tick_seq);
        s.time = meta.time;
        s.day = meta.day % crate::game::sim::DAY_TICKS;
        s.day_cycle = meta.day_cycle;
        s.rng = GameRng(meta.rng);
        s.tick_rng = GameRng(meta.tick_rng);
        g.fx_rng = GameRng(meta.fx_rng);
        if let Some(b) = st.get(LOGISTICS)? {
            let v: LogisticsV1 = decode(LOGISTICS, &b, LOGISTICS_V)?;
            logistics_from(&mut g, v);
            saver.written.insert(LOGISTICS.into(), hash(&b));
        }
        let keys = match which {
            Players::One(nick) => vec![player_key(nick)?],
            Players::All => st.list(PLAYER_PREFIX)?,
        };
        for pkey in keys {
            let Some(b) = st.get(&pkey)? else { continue };
            let v = match (read_player(&pkey, &b), which) {
                (Ok(v), _) => v,
                // Одна порченая запись не кладёт сервер: пропуск, причина — в Saver::unreadable (сервер пишет в журнал).
                (Err(e), Players::All) => {
                    saver.unreadable.push(e.to_string());
                    continue;
                }
                (Err(e), Players::One(_)) => return Err(e),
            };
            let mut p = ServerPlayer::new(v.id, &v.p.nick);
            p.token = v.token;
            apply_player(&mut g, &mut p, &v.p);
            crate::game::rules::apply_mode(&mut p.player, crate::game::rules::GameMode::from_code(v.mode));
            g.parked.push(p);
            saver.written.insert(pkey, hash(&b));
        }
        Ok((g, saver))
    }
}

/// Какие записи игроков грузить вместе с миром.
#[derive(Clone, Copy)]
enum Players<'a> {
    One(&'a str),
    All,
}

/// Запись игрока любой известной версии → версия 4 (старые читаются всегда; режим у них — выживание).
fn read_player(key: &str, bytes: &[u8]) -> Result<PlayerV4, SaveError> {
    let corrupt = |msg: String| SaveError::Corrupt { key: key.to_string(), msg };
    let (ver, rest) = version(bytes).map_err(|e| corrupt(e.to_string()))?;
    Ok(match ver {
        1 => PlayerV4 { id: 0, token: None, mode: 0, p: body(rest).map_err(corrupt)? },
        2 => {
            let v: PlayerV2 = body(rest).map_err(corrupt)?;
            PlayerV4 { id: v.id, token: None, mode: 0, p: v.p }
        }
        3 => {
            let v: PlayerV3 = body(rest).map_err(corrupt)?;
            PlayerV4 { id: v.id, token: v.token, mode: 0, p: v.p }
        }
        PLAYER_V => body(rest).map_err(corrupt)?,
        _ => return Err(SaveError::Version { key: key.to_string(), ver }),
    })
}

/// Разобрать значение: версия не новее want, тело — версии want (старых версий пока нет).
fn decode<T: for<'a> Deserialize<'a>>(key: &str, bytes: &[u8], want: u16) -> Result<T, SaveError> {
    let corrupt = |msg: String| SaveError::Corrupt { key: key.to_string(), msg };
    let (ver, rest) = version(bytes).map_err(|e| corrupt(e.to_string()))?;
    if ver != want {
        return Err(SaveError::Version { key: key.to_string(), ver });
    }
    body(rest).map_err(corrupt)
}

fn chunk_at(p: [f64; 3]) -> (i32, i32) {
    chunk_of(p[0].floor() as i32, p[2].floor() as i32)
}

/// Клетка мира по ключу клетки куска c.
fn cell_pos(c: (i32, i32), key: u16) -> Option<[i32; 3]> {
    (key < CHUNK_CELLS).then(|| {
        let (lx, y, lz) = local_pos(key);
        [c.0 * CS + lx, y, c.1 * CS + lz]
    })
}

/// Правки по возрастанию ключа → разности.
fn delta(edits: &[(u16, u32)]) -> Vec<(u16, u32)> {
    let mut prev = 0;
    edits
        .iter()
        .map(|&(k, c)| {
            let d = k - prev;
            prev = k;
            (d, c)
        })
        .collect()
}
/// Разности → ключи; None — ключ вне куска или повтор.
fn undelta(edits: &[(u16, u32)]) -> Option<Vec<(u16, u32)>> {
    let mut k = 0u16;
    let mut out = Vec::with_capacity(edits.len());
    for (i, &(d, c)) in edits.iter().enumerate() {
        if i > 0 && d == 0 {
            return None;
        }
        k = k.checked_add(d)?;
        if k >= CHUNK_CELLS {
            return None;
        }
        out.push((k, c));
    }
    Some(out)
}

fn stack_v1(s: ItemStack) -> StackV1 {
    StackV1 { item: s.item, count: s.count, damage: s.damage }
}
fn stack_from(s: StackV1) -> ItemStack {
    let st = ItemStack { item: s.item, count: s.count, damage: s.damage };
    if st.is_empty() {
        ItemStack::EMPTY
    } else {
        st
    }
}

fn mek_items_v1(m: &MekItems) -> MekItemsV1 {
    let (next, list) = m.saved();
    MekItemsV1 {
        next,
        list: list
            .into_iter()
            .map(|(k, d)| {
                let slots = d.slots.iter().enumerate().filter(|(_, s)| !s.is_empty()).map(|(i, s)| (i as u8, stack_v1(*s))).collect();
                (k, d.energy, slots, (d.tank.sub, d.tank.amount))
            })
            .collect(),
    }
}
fn mek_items_from(m: &mut MekItems, v: MekItemsV1) {
    let list = v
        .list
        .into_iter()
        .map(|(k, energy, slots, tank)| {
            let mut d = MekItem { energy, tank: Tank { sub: tank.0, amount: tank.1 }, ..Default::default() };
            for (i, s) in slots {
                if let Some(slot) = d.slots.get_mut(i as usize) {
                    *slot = stack_from(s);
                }
            }
            (k, d)
        })
        .collect();
    m.restore(v.next, list);
}

fn be_v1(be: &BlockEntity) -> BeDataV1 {
    match be {
        BlockEntity::Furnace(f) => BeDataV1::Furnace {
            slots: f.slots.map(stack_v1),
            burn: f.burn,
            fuel_total: f.fuel_total,
            cook: f.cook,
            cook_total: f.cook_total,
        },
        BlockEntity::Chest(c) => BeDataV1::Chest {
            slots: c.slots.iter().enumerate().filter(|(_, s)| !s.is_empty()).map(|(i, s)| (i as u8, stack_v1(*s))).collect(),
        },
        BlockEntity::Mek(m) => BeDataV1::MekM4 {
            kind: m.kind as u8,
            slots: m.slots.iter().enumerate().filter(|(_, s)| !s.is_empty()).map(|(i, s)| (i as u8, stack_v1(*s))).collect(),
            energy: m.energy,
            progress: m.progress,
            tank: (m.tank.sub, m.tank.amount),
            heat: m.heat,
            active: (m.active.logic, m.active.shown, m.active.delay),
            sides: m.sides,
            eject: m.eject,
            upgrades: m.upgrades,
            gas: m.gas.map(|t| (t.sub, t.amount)),
            gas_mode: m.gas_mode,
            gas_used: m.gas_used,
        },
    }
}
fn be_from(d: &BeDataV1) -> BlockEntity {
    match d {
        BeDataV1::Furnace { slots, burn, fuel_total, cook, cook_total } => BlockEntity::Furnace(Furnace {
            slots: slots.map(stack_from),
            burn: *burn,
            fuel_total: *fuel_total,
            cook: *cook,
            cook_total: *cook_total,
        }),
        BeDataV1::Chest { slots } => {
            let mut c = Chest::default();
            for &(i, s) in slots {
                if let Some(slot) = c.slots.get_mut(i as usize) {
                    *slot = stack_from(s);
                }
            }
            BlockEntity::Chest(c)
        }
        BeDataV1::Mek { kind, slots, energy, progress, tank, heat, active } => {
            mek_from(*kind, slots, *energy, *progress, *tank, *heat, *active, None)
        }
        BeDataV1::MekM3 { kind, slots, energy, progress, tank, heat, active, sides, eject, upgrades } => {
            mek_from(*kind, slots, *energy, *progress, *tank, *heat, *active, Some((*sides, *eject, [upgrades[0], upgrades[1], 0])))
        }
        BeDataV1::MekM4 { kind, slots, energy, progress, tank, heat, active, sides, eject, upgrades, gas, gas_mode, gas_used } => {
            let mut be = mek_from(*kind, slots, *energy, *progress, *tank, *heat, *active, Some((*sides, *eject, *upgrades)));
            if let BlockEntity::Mek(m) = &mut be {
                m.gas = gas.map(|(sub, amount)| Tank { sub, amount });
                m.gas_mode = *gas_mode;
                m.gas_used = *gas_used;
            }
            be
        }
    }
}

/// Машина из записи (Mek — до M3, MekM3 — со сторонами, выталкиванием и улучшениями).
#[allow(clippy::too_many_arguments)]
fn mek_from(
    kind: u8,
    slots: &[(u8, StackV1)],
    energy: u64,
    progress: u32,
    tank: (u8, u32),
    heat: f64,
    active: (bool, bool, u8),
    m3: Option<([u8; 6], bool, [u8; 3])>,
) -> BlockEntity {
    let k = match kind {
        1 => MekKind::Infuser,
        2 => MekKind::Cube,
        3 => MekKind::Enrich,
        4 => MekKind::Smelter,
        5 => MekKind::Wind,
        6 => MekKind::Crusher,
        7 => MekKind::Purifier,
        8 => MekKind::Separator,
        9 => MekKind::Pump,
        _ => MekKind::Heat,
    };
    let mut m = MekBe::new(k);
    for &(i, s) in slots {
        if let Some(slot) = m.slots.get_mut(i as usize) {
            *slot = stack_from(s);
        }
    }
    if let Some((sides, eject, upgrades)) = m3 {
        m.sides = sides;
        m.eject = eject;
        m.upgrades = upgrades.map(|n| n.min(crate::world::mek::UPGRADE_MAX));
    }
    m.energy = energy.min(m.cap());
    m.progress = progress; // 0 < p < нужно — проверит первый шаг рецепта (MEK-01 §10.2)
    m.tank = Tank { sub: tank.0, amount: tank.1 };
    m.heat = heat;
    m.active = ActiveView { logic: active.0, shown: active.1, delay: active.2 };
    BlockEntity::Mek(Box::new(m))
}

/// Запись логистики M5 из сервера.
fn logistics_v1(sv: &ServerSim) -> LogisticsV1 {
    use crate::world::logistics::Goal;
    let w = sv.sim.world.d.borrow();
    let goal = |g: Goal| match g {
        Goal::Dest => 0u8,
        Goal::Home => 1,
        Goal::Idle => 2,
    };
    let gas_sides = w
        .be()
        .iter()
        .filter_map(|(p, _, be)| match be {
            BlockEntity::Mek(m) if m.gas_sides != crate::world::mek::sides::default_gas_sides(m.kind) => Some((p, m.gas_sides)),
            _ => None,
        })
        .collect();
    LogisticsV1 {
        modes: w.grid.modes.iter().map(|(&p, &m)| (p, m)).collect(),
        colors: w.grid.colors.iter().map(|(&p, &c)| (p, c)).collect(),
        subs: w.grid.saved_subs(),
        stacks: w
            .logistics
            .saved()
            .into_iter()
            .map(|(at, from, progress, s, color, g, home, idle_dir)| TransitV1 { at, from, progress, stack: stack_v1(s), color, goal: goal(g), home, idle_dir })
            .collect(),
        gas_sides,
        item_gas_sides: sv.sim.mek_items.saved().1.into_iter().filter(|(_, d)| d.gas_sides != [0; 6]).map(|(k, d)| (k, d.gas_sides)).collect(),
    }
}
/// Запись логистики M5 в загруженный сервер (после сведения сетей): режимы и цвета — в реестр, вещество — в сети,
/// стеки — без пути (посчитается в первом тике).
fn logistics_from(g: &mut ServerSim, v: LogisticsV1) {
    use crate::world::logistics::Goal;
    let mut w = g.sim.world.d.borrow_mut();
    for (p, m) in v.modes {
        w.grid.modes.insert(p, m);
    }
    for (p, c) in v.colors {
        w.grid.colors.insert(p, c);
    }
    w.grid.restore_subs(&v.subs);
    for (p, s) in v.gas_sides {
        if let Some(m) = w.be_mut().mek_mut(p) {
            m.gas_sides = s;
        }
    }
    let goal = |g: u8| match g {
        1 => Goal::Home,
        2 => Goal::Idle,
        _ => Goal::Dest,
    };
    let list = v.stacks.into_iter().map(|t| (t.at, t.from, t.progress, stack_from(t.stack), t.color, goal(t.goal), t.home, t.idle_dir)).collect();
    w.logistics.restore(list);
    drop(w);
    for (k, s) in v.item_gas_sides {
        if let Some(d) = g.sim.mek_items.get_mut(k) {
            d.gas_sides = s;
        }
    }
}

fn item_v1(e: &ItemEntity) -> ItemV1 {
    ItemV1 {
        id: e.id,
        stack: stack_v1(e.stack),
        pos: e.pos,
        vel: e.vel,
        on_ground: e.on_ground,
        age: e.age,
        item_age: e.item_age,
        spin_age: e.spin_age,
        pickup_delay: e.pickup_delay,
        thrown: e.thrown,
        offset: e.offset,
    }
}
/// Предмет из хранилища: вода и «внутри блока» досчитываются в начале его тика, prev = pos.
fn item_from(v: &ItemV1) -> ItemEntity {
    ItemEntity {
        id: v.id,
        stack: stack_from(v.stack),
        pos: v.pos,
        prev: v.pos,
        vel: v.vel,
        on_ground: v.on_ground,
        no_clip: false,
        age: v.age,
        item_age: v.item_age,
        spin_age: v.spin_age,
        pickup_delay: v.pickup_delay,
        thrown: v.thrown,
        offset: v.offset,
        in_water: false,
        fluid_h: 0.0,
        removed: false,
    }
}

fn falling_v1(f: &FallingBlock) -> FallingV1 {
    FallingV1 { seq: f.id as u64, cell: f.cell, pos: f.pos, vel: f.vel, on_ground: f.on_ground, age: f.age }
}
fn falling_from(v: &FallingV1) -> FallingBlock {
    FallingBlock { id: v.seq as u32, cell: v.cell, pos: v.pos, prev: v.pos, vel: v.vel, on_ground: v.on_ground, age: v.age, removed: false }
}

fn meta_v1(g: &ServerSim) -> MetaV1 {
    let s = &g.sim;
    let w = s.world.d.borrow();
    MetaV1 {
        format: FORMAT,
        seed: w.gen().seed,
        gen_version: GEN_VERSION,
        time: s.time,
        day: s.day,
        day_cycle: s.day_cycle,
        rng: s.rng.0,
        tick_rng: s.tick_rng.0,
        fx_rng: g.fx_rng.0,
        next_item: s.ids.0,
        tick_seq: s.ticks.seq(),
        falling_seq: s.ids.0 as u64,
        be_seq: w.be().next_seq(),
        edit_count: w.edit_count(),
    }
}

fn player_v4(sp: &ServerPlayer) -> PlayerV4 {
    let p = &sp.player;
    let inv = &sp.inv;
    let carried = sp.screen.as_ref().map_or(Vec::new(), |m| {
        std::iter::once(m.cursor).chain(m.grid.cells.iter().copied()).filter(|s| !s.is_empty()).map(stack_v1).collect()
    });
    let v = PlayerV1 {
        nick: sp.nick.clone(),
        pos: p.pos,
        vel: p.vel,
        yaw: p.yaw,
        pitch: p.pitch,
        on_ground: p.on_ground,
        fall: p.fall,
        flying: p.flying,
        crouch: p.crouch,
        selected: inv.selected as u8,
        inv: (0..41).filter_map(|i| Some(inv.get(i)).filter(|s| !s.is_empty()).map(|s| (i as u8, stack_v1(s)))).collect(),
        carried,
    };
    PlayerV4 { id: sp.id, token: sp.token, mode: sp.player.mode.code(), p: v }
}

/// Игрок из записи: стоит, где вышел, без интерполяции; курсор и сетка открытого окна — в инвентарь (не влезло — вперёд).
fn apply_player(g: &mut ServerSim, sp: &mut ServerPlayer, v: &PlayerV1) {
    let p = &mut sp.player;
    p.teleport(v.pos);
    p.vel = v.vel;
    p.yaw = v.yaw;
    p.pitch = v.pitch.clamp(-90.0, 90.0);
    p.on_ground = v.on_ground;
    p.fall = v.fall;
    p.flying = v.flying;
    p.crouch = v.crouch;
    p.cam_y = p.eye_height();
    p.last_cam_y = p.cam_y;
    let mut inv = Inventory::new();
    for &(i, s) in &v.inv {
        if (i as usize) < 41 {
            *inv.at(i as usize) = stack_from(s);
        }
    }
    inv.selected = (v.selected as usize).min(8);
    for &s in &v.carried {
        let left = inv.offer(stack_from(s));
        if !left.is_empty() {
            let p = &sp.player;
            let s = &mut g.sim;
            s.items.throw(p.pos, p.eye_height(), p.yaw, p.pitch, left, false, true, &mut s.rng, &mut s.ids);
        }
    }
    inv.bob = [0; 41];
    sp.inv = inv;
}
