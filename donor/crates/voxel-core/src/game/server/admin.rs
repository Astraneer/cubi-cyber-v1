//! Роли, команды чата и режимы игры на сервере (G17, docs/areas/commands.md). Таблица команд и подсказки —
//! game/commands.rs (общие с клиентом), правила режимов — game/rules.rs. Права проверяет только сервер.
//!
//! Роли (владелец, админы), баны и запрет чата — `Roles`, текстом в файле `roles.txt` папки мира: читает и пишет net.rs
//! (Server: правка руками подхватывается без перезапуска), здесь — только в памяти и флаг `roles_dirty`. Журнал
//! админ-команд — строки `admin_log` (net.rs дописывает в `admin.log` со временем UTC). Выкинуть игрока — `kicks`
//! (соединения у net.rs). Журнал правок игроков для /rollback — `journal` (в памяти процесса, не на диске).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::ServerSim;
use crate::cell::Cell;
use crate::game::commands::{self, coord, item_by_name, words, LVL_ADMIN, LVL_OWNER, LVL_PLAYER};
use crate::game::give_into;
use crate::game::health::{DamageKind, Health};
use crate::game::mob::MobKind;
use crate::game::rules::{self, GameMode};
use crate::game::sim::set_time_of_day;
use crate::proto::{Msg, CHAT_DENY, CHAT_SAY, CHAT_SYSTEM, CHAT_WHISPER};
use crate::registry::items::idef;
use crate::world::stack::ItemStack;

/// Имя файла ролей и журнала в папке мира.
pub(crate) const ROLES_FILE: &str = "roles.txt";
pub(crate) const ADMIN_LOG: &str = "admin.log";
/// Корзина команд: 20 подряд, дальше 10 в секунду (панель предметов кликает быстро; сообщения чата — своя корзина).
const CMD_BURST: f32 = 20.0;
const CMD_PER_TICK: f32 = 0.5;
/// Журнал правок игроков: не старше суток (тиков) и не больше записей.
const JOURNAL_TICKS: u64 = 24 * 60 * 60 * 20;
const JOURNAL_MAX: usize = 400_000;
/// /rollback без минут — за час.
const ROLLBACK_MINUTES: u64 = 60;
/// /give: не больше за раз (MC — 100 стеков).
const GIVE_MAX: i32 = 6400;

/// Роли и наказания по нику (строчными).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Roles {
    pub(crate) owners: BTreeSet<String>,
    pub(crate) admins: BTreeSet<String>,
    /// Ник → причина.
    pub(crate) bans: BTreeMap<String, String>,
    pub(crate) mutes: BTreeSet<String>,
}

impl Roles {
    /// Разбор файла: строки «owner Ник», «admin Ник», «ban Ник [причина]», «mute Ник»; «#» — комментарий; прочее
    /// пропускается.
    pub(crate) fn parse(text: &str) -> Roles {
        let mut r = Roles::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.splitn(3, char::is_whitespace);
            let (Some(kind), Some(nick)) = (it.next(), it.next()) else { continue };
            let nick = nick.trim().to_lowercase();
            if nick.is_empty() {
                continue;
            }
            match kind.to_lowercase().as_str() {
                "owner" => {
                    r.owners.insert(nick);
                }
                "admin" | "op" => {
                    r.admins.insert(nick);
                }
                "ban" => {
                    r.bans.insert(nick, it.next().unwrap_or("").trim().to_string());
                }
                "mute" => {
                    r.mutes.insert(nick);
                }
                _ => {}
            }
        }
        r
    }

    pub(crate) fn to_text(&self) -> String {
        let mut s = String::from(
            "# Роли сервера (G17, docs/areas/commands.md). Строки: «owner Ник», «admin Ник», «ban Ник причина», «mute Ник».\n\
             # Ник без учёта регистра; права действуют, только если ник закреплён за браузером (токеном). Правка руками\n\
             # подхватывается сервером за несколько секунд; /op, /deop, /ban, /mute переписывают файл.\n",
        );
        for n in &self.owners {
            s += &format!("owner {n}\n");
        }
        for n in &self.admins {
            s += &format!("admin {n}\n");
        }
        for (n, why) in &self.bans {
            s += &if why.is_empty() { format!("ban {n}\n") } else { format!("ban {n} {why}\n") };
        }
        for n in &self.mutes {
            s += &format!("mute {n}\n");
        }
        s
    }

    /// Права ника по файлу (без проверки токена).
    pub(crate) fn level(&self, nick: &str) -> u8 {
        let k = nick.to_lowercase();
        if self.owners.contains(&k) {
            LVL_OWNER
        } else if self.admins.contains(&k) {
            LVL_ADMIN
        } else {
            LVL_PLAYER
        }
    }
}

/// Корзина команд игрока (время — тики сервера).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CmdBucket {
    tokens: f32,
    at: u64,
}

impl CmdBucket {
    pub(crate) fn new(now: u64) -> CmdBucket {
        CmdBucket { tokens: CMD_BURST, at: now }
    }
    fn take(&mut self, now: u64) -> bool {
        self.tokens = (self.tokens + now.saturating_sub(self.at) as f32 * CMD_PER_TICK).min(CMD_BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Правка игрока в журнале: тик, кто (номер сущности), клетка, было, стало.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EditRec {
    pub(crate) time: u64,
    pub(crate) who: u32,
    pub(crate) pos: [i32; 3],
    pub(crate) old: Cell,
    pub(crate) new: Cell,
}

/// Состояние админки сервера (поле ServerSim::admin).
#[derive(Default)]
pub(crate) struct Admin {
    /// Все — админы (локальный сервер браузера `?local=1`, сцены): проверять команды без файла ролей.
    pub(crate) open: bool,
    pub(crate) roles: Roles,
    /// Роли поменялись командой — net.rs перепишет файл.
    pub(crate) roles_dirty: bool,
    /// Строки журнала админ-команд (без времени — его ставит net.rs).
    pub(crate) log: Vec<String>,
    /// Выкинуть: (номер игрока, текст игроку) — соединения закрывает net.rs.
    pub(crate) kicks: Vec<(u32, String)>,
    /// /save: записать мир (net.rs).
    pub(crate) save_req: bool,
    /// MSPT (ставит net.rs) и время последних тиков (мс) для /tps.
    pub(crate) mspt: f32,
    pub(crate) tick_ms: VecDeque<u64>,
    pub(crate) journal: VecDeque<EditRec>,
}

/// Ответ команды: строка и вид (системное или отказ).
type Reply = Result<String, String>;

impl ServerSim {
    /// Права игрока i: по файлу ролей — только с закреплённым ником (токен); локальный сервер браузера (`admin.open`) —
    /// всем не меньше админа.
    pub(crate) fn level_of(&self, i: usize) -> u8 {
        self.level_for(&self.players[i])
    }
    pub(crate) fn level_for(&self, p: &super::ServerPlayer) -> u8 {
        let file = if p.token.is_some() { self.admin.roles.level(&p.nick) } else { LVL_PLAYER };
        if self.admin.open {
            file.max(LVL_ADMIN)
        } else {
            file
        }
    }

    /// Творческое ломание и дальность у игрока i: творческий режим или отладочный полёт сквозь стены при отладке.
    pub(crate) fn creative_of(&self, i: usize) -> bool {
        let p = &self.players[i].player;
        p.mode == GameMode::Creative || (self.debug && p.noclip && p.mode != GameMode::Spectator)
    }

    /// Сменить режим игрока i: тело (rules::apply_mode), окно и ломание; клиенту — Msg::Mode и Teleport на то же место
    /// (ходы старого режима до TeleportAck не в счёт).
    pub(crate) fn set_mode(&mut self, i: usize, mode: GameMode) {
        self.stop_breaking(i);
        if mode == GameMode::Spectator {
            self.close_screen(i, false);
        }
        let level = self.level_of(i);
        let p = &mut self.players[i];
        p.eating = None;
        rules::apply_mode(&mut p.player, mode);
        p.sent_mode = Some((mode.code(), level));
        p.own.push(Msg::Mode { mode: mode.code(), level });
        let pos = p.player.pos;
        p.player.vel = [0.0; 3];
        p.next_tp = p.next_tp.wrapping_add(1).max(1);
        p.teleport = Some(p.next_tp);
        p.own.push(Msg::Teleport { id: p.next_tp, pos, yaw: p.player.yaw, pitch: p.player.pitch });
        p.inv_force = true;
    }

    /// После тика: правки игроков — в журнал (/rollback), старое — прочь; время тика для /tps.
    pub(crate) fn admin_tick(&mut self) {
        let t = self.sim.time;
        for (who, pos, old, new) in std::mem::take(&mut self.sim.by_player) {
            self.admin.journal.push_back(EditRec { time: t, who, pos, old, new });
        }
        let j = &mut self.admin.journal;
        while j.len() > JOURNAL_MAX || j.front().is_some_and(|r| r.time + JOURNAL_TICKS < t) {
            j.pop_front();
        }
        let tm = &mut self.admin.tick_ms;
        tm.push_back(self.now_ms);
        if tm.len() > 101 {
            tm.pop_front();
        }
    }

    /// Строка, начатая «/» (из chat_in): частота — корзина команд; дальше — права и исполнение; ответ — только
    /// спросившему; админ-команды — в журнал.
    pub(crate) fn command_in(&mut self, i: usize, text: &str) {
        let id = self.players[i].id;
        if !self.players[i].cmd.take(self.ticks) {
            return self.tell(id, CHAT_DENY, "Слишком много команд подряд — подождите".into());
        }
        let w = words(text);
        let Some(&name) = w.first() else {
            return self.tell(id, CHAT_DENY, "Неизвестная команда «/». Список команд — /help".into());
        };
        let Some(spec) = commands::find(name) else {
            return self.tell(id, CHAT_DENY, format!("Неизвестная команда «/{}». Список команд — /help", name.to_lowercase()));
        };
        let level = self.level_of(i);
        let creative = self.players[i].player.mode == GameMode::Creative;
        let r: Reply = if !commands::allowed(spec, level, creative) {
            Err(format!("Нет прав на /{}", spec.name))
        } else {
            self.run(i, spec.name, &w[1..], level)
        };
        if spec.level >= LVL_ADMIN {
            let res = match &r {
                Ok(s) => format!("ок: {s}"),
                Err(s) => format!("отказ: {s}"),
            };
            let line = format!("{}: {} → {}", self.players[i].nick, text, res);
            self.admin.log.push(line);
        }
        match r {
            Ok(s) if !s.is_empty() => self.tell(id, CHAT_SYSTEM, s),
            Ok(_) => {}
            Err(s) => self.tell(id, CHAT_DENY, s),
        }
    }

    /// Игрок по нику (в сети), без учёта регистра.
    fn online(&self, nick: &str) -> Result<usize, String> {
        let k = nick.to_lowercase();
        self.players.iter().position(|p| p.nick.to_lowercase() == k).ok_or_else(|| format!("Игрока «{nick}» нет в сети"))
    }

    /// Цели: @s и @p — сам, @a — все в сети, иначе ник.
    fn targets(&self, i: usize, w: &str) -> Result<Vec<usize>, String> {
        match w {
            "@s" | "@p" => Ok(vec![i]),
            "@a" => Ok((0..self.players.len()).collect()),
            _ => self.online(w).map(|t| vec![t]),
        }
    }

    /// Необязательный ник (по умолчанию — сам).
    fn target_or_self(&self, i: usize, w: Option<&&str>) -> Result<usize, String> {
        match w {
            Some(n) => self.targets(i, n).and_then(|v| if v.len() == 1 { Ok(v[0]) } else { Err("Нужен один игрок".into()) }),
            None => Ok(i),
        }
    }

    /// Может ли игрок с правами level наказывать ник: владелец — любого, кроме владельцев; админ — только игроков.
    fn outranks(&self, level: u8, nick: &str) -> bool {
        let t = self.admin.roles.level(nick);
        t < LVL_OWNER && (level == LVL_OWNER || t < level)
    }

    fn run(&mut self, i: usize, name: &str, a: &[&str], level: u8) -> Reply {
        let me = self.players[i].nick.clone();
        let id = self.players[i].id;
        match name {
            "help" => {
                let creative = self.players[i].player.mode == GameMode::Creative;
                commands::help(a.first().copied(), level, creative)
            }
            "list" => {
                let names: Vec<String> = self.players.iter().map(|p| p.nick.clone()).collect();
                Ok(format!("В сети ({}): {}", names.len(), names.join(", ")))
            }
            "msg" => {
                let (Some(to), true) = (a.first(), a.len() > 1) else { return Err("/msg <ник> <текст>".into()) };
                if self.admin.roles.mutes.contains(&me.to_lowercase()) {
                    return Err("Вам запрещено писать в чат".into());
                }
                let t = self.online(to)?;
                let text = a[1..].join(" ");
                let (tid, tn) = (self.players[t].id, self.players[t].nick.clone());
                self.tell(tid, CHAT_WHISPER, format!("{me} шепчет вам: {text}"));
                self.tell(id, CHAT_WHISPER, format!("Вы шепчете {tn}: {text}"));
                Ok(String::new())
            }
            "spawn" => {
                if !self.players[i].hp.alive() {
                    return Err("Сначала возродитесь".into());
                }
                if self.players[i].pvp_tag > 0 {
                    return Err("В бою нельзя — подождите 10 с после удара".into());
                }
                self.respawn_at_crossroads(i);
                Ok("Перекрёсток".into())
            }
            "gamemode" => {
                let mode = a.first().and_then(|m| GameMode::parse(m)).ok_or("/gamemode <survival|creative|spectator> [ник]")?;
                let t = self.target_or_self(i, a.get(1))?;
                self.set_mode(t, mode);
                let tn = self.players[t].nick.clone();
                if t != i {
                    let tid = self.players[t].id;
                    self.tell(tid, CHAT_SYSTEM, format!("Ваш режим игры: {}", mode.ru()));
                }
                Ok(if t == i { format!("Ваш режим игры: {}", mode.ru()) } else { format!("Режим игры {tn}: {}", mode.ru()) })
            }
            "tp" => self.cmd_tp(i, a),
            "give" => self.cmd_give(i, a, level),
            "time" => {
                let v = match (a.first().map(|s| s.to_lowercase()).as_deref(), a.get(1)) {
                    (Some("set"), Some(v)) => *v,
                    _ => return Err("/time set <day|noon|night|midnight|число>".into()),
                };
                let t: u64 = match v.to_lowercase().as_str() {
                    "day" => 1000,
                    "noon" => 6000,
                    "night" => 13000,
                    "midnight" => 18000,
                    n => n.parse::<u64>().map_err(|_| format!("Не время: «{n}»"))? % 24000,
                };
                // Тик MC 0 — 6:00; доля суток от полуночи.
                self.sim.day = set_time_of_day(((t + 6000) % 24000) as f64 / 24000.0);
                self.time_dirty = true;
                Ok(format!("Время: {t}"))
            }
            "heal" => {
                let t = self.target_or_self(i, a.first())?;
                let p = &mut self.players[t];
                if !p.hp.alive() {
                    return Err(format!("{} мёртв", p.nick));
                }
                p.hp.hp = p.hp.max;
                p.hp.air = Health::new().air;
                p.hp.fire = 0;
                Ok(format!("{} вылечен", p.nick))
            }
            "kill" => {
                let t = self.target_or_self(i, a.first())?;
                if !self.players[t].hp.alive() {
                    return Err(format!("{} уже мёртв", self.players[t].nick));
                }
                if t != i && !self.outranks(level, &self.players[t].nick.clone()) {
                    return Err("Нельзя: у игрока права не ниже ваших".into());
                }
                self.players[t].hp.hp = 0.0;
                self.die(t, DamageKind::Generic, None);
                Ok(format!("{} убит", self.players[t].nick))
            }
            "clear" => {
                let t = self.target_or_self(i, a.first())?;
                self.close_screen(t, false);
                let p = &mut self.players[t];
                let sel = p.inv.selected;
                p.inv = crate::game::inventory::Inventory::new();
                p.inv.selected = sel;
                p.inv_force = true;
                Ok(format!("Инвентарь {} очищен", p.nick))
            }
            "summon" => {
                let kind = a.first().and_then(|m| MobKind::by_name(&m.to_lowercase().replace("minecraft:", ""))).ok_or("/summon <pig|cow|sheep|chicken|zombie> [x y z]")?;
                let base = self.players[i].player.pos;
                let pos = if a.len() >= 4 {
                    let c: Option<Vec<f64>> = (0..3).map(|k| coord(a[1 + k], base[k])).collect();
                    let c = c.ok_or("Координаты — числа или ~")?;
                    [c[0], c[1], c[2]]
                } else {
                    base
                };
                self.debug_spawn_mob(kind as u8, pos, 1);
                Ok(format!("Призван: {}", kind.def().mc))
            }
            "killall" => {
                if a.first().map(|s| s.to_lowercase()).as_deref() != Some("mobs") {
                    return Err("/killall mobs".into());
                }
                let mut n = 0;
                for m in self.sim.mobs.list.iter_mut().filter(|m| !m.removed) {
                    m.removed = true;
                    n += 1;
                }
                Ok(format!("Убрано мобов: {n}"))
            }
            "kick" => {
                let t = self.online(a.first().ok_or("/kick <ник> [причина]")?)?;
                let tn = self.players[t].nick.clone();
                if t == i || !self.outranks(level, &tn) {
                    return Err("Нельзя: у игрока права не ниже ваших".into());
                }
                let why = a[1..].join(" ");
                let text = if why.is_empty() { "Вас выкинули с сервера".to_string() } else { format!("Вас выкинули с сервера: {why}") };
                self.admin.kicks.push((self.players[t].id, text));
                Ok(format!("{tn} выкинут"))
            }
            "ban" => {
                let n = a.first().ok_or("/ban <ник> [причина]")?.to_string();
                if n.to_lowercase() == me.to_lowercase() || !self.outranks(level, &n) {
                    return Err("Нельзя: у игрока права не ниже ваших".into());
                }
                let why = a[1..].join(" ");
                self.admin.roles.bans.insert(n.to_lowercase(), why.clone());
                self.admin.roles_dirty = true;
                if let Ok(t) = self.online(&n) {
                    let text = if why.is_empty() { "Вы забанены на этом сервере".to_string() } else { format!("Вы забанены на этом сервере: {why}") };
                    self.admin.kicks.push((self.players[t].id, text));
                }
                Ok(format!("{n} забанен"))
            }
            "unban" => {
                let n = a.first().ok_or("/unban <ник>")?.to_lowercase();
                if self.admin.roles.bans.remove(&n).is_none() {
                    return Err(format!("«{n}» не забанен"));
                }
                self.admin.roles_dirty = true;
                Ok(format!("{n} разбанен"))
            }
            "mute" | "unmute" => {
                let n = a.first().ok_or("/mute <ник>")?.to_string();
                if !self.outranks(level, &n) {
                    return Err("Нельзя: у игрока права не ниже ваших".into());
                }
                let k = n.to_lowercase();
                let on = name == "mute";
                let changed = if on { self.admin.roles.mutes.insert(k) } else { self.admin.roles.mutes.remove(&k) };
                self.admin.roles_dirty |= changed;
                Ok(if on { format!("{n} не может писать в чат") } else { format!("{n} снова может писать в чат") })
            }
            "tps" => {
                let tm = &self.admin.tick_ms;
                let span = tm.back().zip(tm.front()).map_or(0, |(b, f)| b.saturating_sub(*f));
                if tm.len() < 2 || span == 0 {
                    return Ok(format!("TPS: — (нет часов), MSPT {:.2}", self.admin.mspt));
                }
                let tps = (tm.len() - 1) as f64 * 1000.0 / span as f64;
                Ok(format!("TPS {:.1}, MSPT {:.2}, игроков {}", tps.min(20.0), self.admin.mspt, self.players.len()))
            }
            "save" => {
                self.admin.save_req = true;
                Ok("Мир записывается".into())
            }
            "say" => {
                if a.is_empty() {
                    return Err("/say <текст>".into());
                }
                self.announce(CHAT_SAY, id, format!("[{me}] {}", a.join(" ")));
                Ok(String::new())
            }
            "rollback" => {
                let n = a.first().ok_or("/rollback <ник> [минуты]")?.to_string();
                let minutes = match a.get(1) {
                    Some(m) => m.parse::<u64>().ok().filter(|&m| m > 0).ok_or("Минуты — целое больше 0")?,
                    None => ROLLBACK_MINUTES,
                };
                let (back, skipped) = self.rollback(&n, minutes)?;
                Ok(format!("Откат {n} за {minutes} мин: возвращено блоков {back}, пропущено (изменены позже) {skipped}"))
            }
            "op" | "deop" => {
                let n = a.first().ok_or("/op <ник>")?.to_lowercase();
                if self.admin.roles.owners.contains(&n) {
                    return Err("Это владелец".into());
                }
                let on = name == "op";
                let changed = if on { self.admin.roles.admins.insert(n.clone()) } else { self.admin.roles.admins.remove(&n) };
                self.admin.roles_dirty |= changed;
                if let Ok(t) = self.online(&n) {
                    let tid = self.players[t].id;
                    self.tell(tid, CHAT_SYSTEM, if on { "Вам выданы права админа".into() } else { "Права админа сняты".into() });
                }
                Ok(if on { format!("{n} — админ") } else { format!("{n} больше не админ") })
            }
            _ => Err(format!("Команда /{name} ещё не сделана")),
        }
    }

    /// /tp <ник> · /tp <кого> <к кому> · /tp [кого] <x> <y> <z> (~ — относительно того, кого переносят).
    fn cmd_tp(&mut self, i: usize, a: &[&str]) -> Reply {
        let usage = || "/tp <ник> | /tp <кого> <к кому> | /tp [кого] <x> <y> <z>".to_string();
        let (who, to) = match a.len() {
            1 => (i, self.players[self.online(a[0])?].player.pos),
            2 => {
                let w = self.online(a[0])?;
                (w, self.players[self.online(a[1])?].player.pos)
            }
            3 | 4 => {
                let w = if a.len() == 4 { self.online(a[0])? } else { i };
                let base = self.players[w].player.pos;
                let c = &a[a.len() - 3..];
                let p: Option<Vec<f64>> = (0..3).map(|k| coord(c[k], base[k])).collect();
                let p = p.ok_or("Координаты — числа или ~")?;
                (w, [p[0], p[1], p[2]])
            }
            _ => return Err(usage()),
        };
        if to[0].abs() >= 3.0e7 || to[2].abs() >= 3.0e7 || !(-64.0..=320.0).contains(&to[1]) {
            return Err("Координаты вне мира".into());
        }
        let yaw = self.players[who].player.yaw;
        let flying = self.players[who].player.flying;
        self.relocate(who, to, yaw);
        // relocate ставит на землю; летящий остаётся в полёте (иначе упадёт с высоты).
        if flying {
            self.players[who].player.on_ground = false;
        }
        Ok(format!("{} → {:.1} {:.1} {:.1}", self.players[who].nick, to[0], to[1], to[2]))
    }

    /// /give <кто> <предмет> [кол-во]: не админ (в творческом) — только себе. Не влезло — у ног.
    fn cmd_give(&mut self, i: usize, a: &[&str], level: u8) -> Reply {
        let (Some(who), Some(name)) = (a.first(), a.get(1)) else { return Err("/give <кто> <предмет> [кол-во]".into()) };
        let item = item_by_name(name).ok_or_else(|| format!("Нет такого предмета: «{name}»"))?;
        let count = match a.get(2) {
            Some(c) => c.parse::<i32>().ok().filter(|c| (1..=GIVE_MAX).contains(c)).ok_or_else(|| format!("Количество — от 1 до {GIVE_MAX}"))?,
            None => 1,
        };
        let ts = self.targets(i, who)?;
        if level < LVL_ADMIN && ts.iter().any(|&t| t != i) {
            return Err("В творческом /give — только себе (@s)".into());
        }
        let max = (idef(item).max_stack as i32).max(1);
        let mut got = Vec::new();
        for t in ts {
            let left = give_into(&mut self.players[t].inv, item, count);
            let mut rest = left;
            while rest > 0 {
                let n = rest.min(max);
                rest -= n;
                let p = &self.players[t].player;
                let s = &mut self.sim;
                s.items.throw(p.pos, p.eye_height(), p.yaw, p.pitch, ItemStack::of(item, n), false, true, &mut s.rng, &mut s.ids);
            }
            got.push(self.players[t].nick.clone());
        }
        Ok(format!("Выдано: {count} × {} → {}", idef(item).name, got.join(", ")))
    }

    /// Откат правок ника за minutes минут: от новых к старым каждая клетка, где сейчас то, что поставил игрок, получает
    /// прежнее (по правилам игры, с обновлением соседей). Изменённые после него — пропуск. Ответ — (вернули, пропущено).
    pub(crate) fn rollback(&mut self, nick: &str, minutes: u64) -> Result<(usize, usize), String> {
        let k = nick.to_lowercase();
        let who = self
            .players
            .iter()
            .chain(self.parked.iter())
            .find(|p| p.nick.to_lowercase() == k)
            .map(|p| p.id)
            .ok_or_else(|| format!("Игрока «{nick}» сервер не знает"))?;
        let since = self.sim.time.saturating_sub(minutes.saturating_mul(1200));
        let d = self.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let (mut back, mut skipped) = (0, 0);
        let mut keep = VecDeque::with_capacity(self.admin.journal.len());
        let recs: Vec<EditRec> = self.admin.journal.drain(..).collect();
        for r in recs.iter().rev() {
            if r.who != who || r.time < since {
                keep.push_front(*r);
                continue;
            }
            if w.get_cell(r.pos[0], r.pos[1], r.pos[2]) == r.new {
                self.sim.set_world(&mut w, r.pos, r.old, &mut self.events);
                back += 1;
            } else {
                skipped += 1;
            }
        }
        self.admin.journal = keep;
        Ok((back, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_file_round_trip() {
        let text = "# комментарий\nowner Oleg\nadmin Вася\n\nban Griefer гриф на спавне\nmute Spam\nчто-то странное\nban\n";
        let r = Roles::parse(text);
        assert!(r.owners.contains("oleg") && r.admins.contains("вася"));
        assert_eq!(r.bans.get("griefer").map(String::as_str), Some("гриф на спавне"));
        assert!(r.mutes.contains("spam"));
        assert_eq!(Roles::parse(&r.to_text()), r);
        assert_eq!(r.level("OLEG"), LVL_OWNER);
        assert_eq!(r.level("ВАСЯ"), LVL_ADMIN);
        assert_eq!(r.level("кто-то"), LVL_PLAYER);
    }
}
