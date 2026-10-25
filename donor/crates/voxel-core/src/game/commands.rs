//! Команды чата (G17, docs/areas/commands.md): таблица, разбор слов и подсказки. Чистые функции — их зовут сервер
//! (исполнение и права, game/server/admin.rs) и клиент (подсказки и Tab в чате, web/game/chat.js через `Game::chat_hint`).
//! Синтаксис — как Minecraft Java 1.21.1; права проверяет только сервер, клиент лишь не предлагает недоступное.

use crate::game::mob::MobKind;
use crate::registry::items::{idef, ItemId, ItemKind, G7_END, G7_FIRST, ITEMS_END};

/// Права: игрок, админ, владелец (`Msg::Mode.level`).
pub(crate) const LVL_PLAYER: u8 = 0;
pub(crate) const LVL_ADMIN: u8 = 1;
pub(crate) const LVL_OWNER: u8 = 2;

/// Вид аргумента — что подсказывать.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Arg {
    /// Ник в сети.
    Player,
    /// Ник или @s / @p / @a.
    Target,
    /// Ник или координата (/tp).
    PlayerOrCoord,
    Item,
    Number,
    Mode,
    Mob,
    Coord,
    /// Слово из списка.
    Word(&'static [&'static str]),
    /// Имя команды (/help).
    Command,
    /// Свободный текст до конца строки.
    Text,
}

/// Команда: имя, другие имена, кто может, синтаксис, что делает, аргументы по порядку.
pub(crate) struct Spec {
    pub(crate) name: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) level: u8,
    pub(crate) usage: &'static str,
    pub(crate) help: &'static str,
    pub(crate) args: &'static [Arg],
}

const fn spec(name: &'static str, aliases: &'static [&'static str], level: u8, usage: &'static str, help: &'static str, args: &'static [Arg]) -> Spec {
    Spec { name, aliases, level, usage, help, args }
}

use Arg::*;

/// Все команды (порядок — порядок /help).
pub(crate) const COMMANDS: &[Spec] = &[
    spec("help", &["?"], LVL_PLAYER, "/help [команда]", "список команд или синтаксис одной", &[Command]),
    spec("list", &[], LVL_PLAYER, "/list", "кто в сети", &[]),
    spec("msg", &["tell", "w"], LVL_PLAYER, "/msg <ник> <текст>", "личное сообщение", &[Player, Text]),
    spec("spawn", &[], LVL_PLAYER, "/spawn", "на Перекрёсток", &[]),
    spec("gamemode", &["gm"], LVL_ADMIN, "/gamemode <survival|creative|spectator> [ник]", "режим игры", &[Mode, Player]),
    spec("tp", &["teleport"], LVL_ADMIN, "/tp <ник> | /tp <кого> <к кому> | /tp [кого] <x> <y> <z>", "телепорт (~ — относительно)", &[PlayerOrCoord, PlayerOrCoord, PlayerOrCoord, Coord]),
    spec("give", &[], LVL_ADMIN, "/give <кто> <предмет> [кол-во]", "выдать предмет (@s — себе)", &[Target, Item, Number]),
    spec("time", &[], LVL_ADMIN, "/time set <day|noon|night|midnight|число>", "время суток", &[Word(&["set"]), Word(&["day", "noon", "night", "midnight"])]),
    spec("heal", &[], LVL_ADMIN, "/heal [ник]", "полное здоровье", &[Player]),
    spec("kill", &[], LVL_ADMIN, "/kill [ник]", "убить (вещи выпадут)", &[Player]),
    spec("clear", &[], LVL_ADMIN, "/clear [ник]", "очистить инвентарь", &[Player]),
    spec("summon", &[], LVL_ADMIN, "/summon <моб> [x y z]", "призвать моба", &[Mob, Coord, Coord, Coord]),
    spec("killall", &[], LVL_ADMIN, "/killall mobs", "убрать всех мобов", &[Word(&["mobs"])]),
    spec("kick", &[], LVL_ADMIN, "/kick <ник> [причина]", "выкинуть с сервера", &[Player, Text]),
    spec("ban", &[], LVL_ADMIN, "/ban <ник> [причина]", "бан по нику", &[Player, Text]),
    spec("unban", &["pardon"], LVL_ADMIN, "/unban <ник>", "снять бан", &[Text]),
    spec("mute", &[], LVL_ADMIN, "/mute <ник>", "запретить писать в чат", &[Player]),
    spec("unmute", &[], LVL_ADMIN, "/unmute <ник>", "разрешить писать в чат", &[Player]),
    spec("tps", &[], LVL_ADMIN, "/tps", "тиков в секунду и MSPT", &[]),
    spec("save", &["save-all"], LVL_ADMIN, "/save", "записать мир сейчас", &[]),
    spec("say", &[], LVL_ADMIN, "/say <текст>", "объявление всем", &[Text]),
    spec("rollback", &[], LVL_ADMIN, "/rollback <ник> [минуты]", "вернуть блоки, что ник ломал и ставил (по умолчанию 60 мин)", &[Player, Number]),
    spec("op", &[], LVL_OWNER, "/op <ник>", "дать права админа", &[Player]),
    spec("deop", &[], LVL_OWNER, "/deop <ник>", "снять права админа", &[Player]),
];

/// Команда по имени или другому имени (без «/», без учёта регистра).
pub(crate) fn find(word: &str) -> Option<&'static Spec> {
    let w = word.to_lowercase();
    COMMANDS.iter().find(|s| s.name == w || s.aliases.contains(&w.as_str()))
}

/// Доступна ли команда: по правам; /give — ещё и любому в творческом (только себе — проверяет сервер).
pub(crate) fn allowed(s: &Spec, level: u8, creative: bool) -> bool {
    level >= s.level || (creative && s.name == "give")
}

/// Слова команды без «/» (пробелы схлопнуты — текст чата уже чищен clean_chat).
pub(crate) fn words(text: &str) -> Vec<&str> {
    text.trim_start_matches('/').split(' ').filter(|w| !w.is_empty()).collect()
}

/// Предмет по имени Minecraft: «stone», «minecraft:stone», «mekanism:steel_ingot» (пространство имён отбрасывается).
pub(crate) fn item_by_name(s: &str) -> Option<ItemId> {
    let s = s.to_lowercase();
    let name = s.rsplit(':').next().unwrap_or("");
    if name.is_empty() || name == "air" {
        return None;
    }
    all_items().find(|&i| idef(i).mc == name)
}

/// Все настоящие предметы по возрастанию номера.
pub(crate) fn all_items() -> impl Iterator<Item = ItemId> {
    (1..ITEMS_END).chain(G7_FIRST..G7_END).filter(|&i| idef(i).kind != ItemKind::None)
}

/// Подходит ли вариант к введённому (SharedSuggestionProvider.matchesSubStr): начало имени или любого слова после «_»,
/// без учёта регистра.
pub(crate) fn matches(cand: &str, input: &str) -> bool {
    let (c, i) = (cand.to_lowercase(), input.to_lowercase());
    let i = i.rsplit(':').next().unwrap_or("");
    if c.starts_with(i) {
        return true;
    }
    c.match_indices('_').any(|(k, _)| c[k + 1..].starts_with(i))
}

/// Координата: «12», «-3.5», «~», «~2» (base — своя). None — не число.
pub(crate) fn coord(s: &str, base: f64) -> Option<f64> {
    let v = match s.strip_prefix('~') {
        Some("") => base,
        Some(r) => base + r.parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    v.is_finite().then_some(v)
}

/// Подсказка к строке ввода (текст до курсора): синтаксис текущей команды, с какого символа (не байта) начинается
/// дополняемое слово и варианты. None — это не команда (нет «/» в начале).
pub(crate) struct Hint {
    pub(crate) usage: String,
    pub(crate) start: usize,
    pub(crate) list: Vec<String>,
}

/// Вариантов в подсказке не больше.
const HINT_MAX: usize = 40;

pub(crate) fn hint(text: &str, level: u8, creative: bool, names: &[String]) -> Option<Hint> {
    let rest = text.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split(' ').collect();
    let cur = *parts.last().unwrap_or(&"");
    let start = text.chars().count() - cur.chars().count();
    if parts.len() == 1 {
        // Имя команды.
        let list: Vec<String> = COMMANDS
            .iter()
            .filter(|s| allowed(s, level, creative) && (s.name.starts_with(&cur.to_lowercase()) || s.aliases.iter().any(|a| a.starts_with(&cur.to_lowercase()) && !cur.is_empty())))
            .map(|s| s.name.to_string())
            .collect();
        let usage = match find(cur) {
            Some(s) if allowed(s, level, creative) => format!("{} — {}", s.usage, s.help),
            _ if list.is_empty() => format!("Неизвестная команда «/{cur}». Список — /help"),
            _ => String::new(),
        };
        return Some(Hint { usage, start, list });
    }
    let Some(s) = find(parts[0]).filter(|s| allowed(s, level, creative)) else {
        return Some(Hint { usage: format!("Неизвестная команда «/{}». Список — /help", parts[0]), start, list: Vec::new() });
    };
    let usage = format!("{} — {}", s.usage, s.help);
    let k = parts.len() - 2; // номер дополняемого аргумента
    // Текст до конца строки — дальше не подсказываем.
    if s.args.iter().take(k).any(|a| *a == Text) {
        return Some(Hint { usage, start, list: Vec::new() });
    }
    let kind = s.args.get(k).copied();
    let mut list: Vec<String> = match kind {
        Some(Player) => names.to_vec(),
        Some(Target) => ["@s", "@p", "@a"].iter().map(|v| v.to_string()).chain(names.iter().cloned()).collect(),
        Some(PlayerOrCoord) => names.iter().cloned().chain(std::iter::once("~".to_string())).collect(),
        Some(Coord) => vec!["~".into()],
        Some(Mode) => crate::game::rules::GameMode::ALL.iter().map(|m| m.mc().to_string()).collect(),
        Some(Mob) => MobKind::ALL.iter().map(|m| m.def().mc.to_string()).collect(),
        Some(Word(w)) => w.iter().map(|v| v.to_string()).collect(),
        Some(Command) => COMMANDS.iter().filter(|c| allowed(c, level, creative)).map(|c| c.name.to_string()).collect(),
        Some(Item) => {
            let mut v: Vec<&'static str> = all_items().map(|i| idef(i).mc).filter(|n| matches(n, cur)).collect();
            v.sort_by_key(|n| (!n.starts_with(&cur.to_lowercase()), *n));
            v.dedup();
            return Some(Hint { usage, start, list: v.into_iter().take(HINT_MAX).map(String::from).collect() });
        }
        Some(Number) | Some(Text) | None => Vec::new(),
    };
    list.retain(|v| matches(v, cur) || (cur.is_empty()));
    list.truncate(HINT_MAX);
    Some(Hint { usage, start, list })
}

/// /help для прав level: строки «/команда — что делает»; с именем — синтаксис одной.
pub(crate) fn help(arg: Option<&str>, level: u8, creative: bool) -> Result<String, String> {
    if let Some(a) = arg {
        return match find(a.trim_start_matches('/')).filter(|s| allowed(s, level, creative)) {
            Some(s) => Ok(format!("{} — {}", s.usage, s.help)),
            None => Err(format!("Нет такой команды: /{a}")),
        };
    }
    let list: Vec<&str> = COMMANDS.iter().filter(|s| allowed(s, level, creative)).map(|s| s.name).collect();
    Ok(format!("Команды: /{}. Подробнее — /help <команда>", list.join(", /")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_names_unique_and_found() {
        // Имя Minecraft — ключ /give: у каждого предмета своё (иначе панель nei выдаст не тот).
        let mut seen = std::collections::HashMap::new();
        for i in all_items() {
            let n = idef(i).mc;
            if let Some(j) = seen.insert(n, i) {
                panic!("имя «{n}» у предметов {j} и {i}");
            }
            assert_eq!(item_by_name(n), Some(i));
        }
        assert!(seen.len() > 100, "предметов {}", seen.len());
        assert_eq!(item_by_name("minecraft:STONE"), item_by_name("stone"));
        assert!(item_by_name("stone").is_some());
        assert_eq!(item_by_name("nope"), None);
        assert_eq!(item_by_name("air"), None);
        // Панель nei (G18) шлёт «/give @s <mc> <n>» по своему списку: каждое имя находит тот же предмет.
        for i in crate::game::nei::item_ids() {
            assert_eq!(item_by_name(idef(i).mc), Some(i), "nei: {}", idef(i).mc);
        }
    }

    #[test]
    fn hints() {
        let names = vec!["Аня".to_string(), "Bot".to_string()];
        // Игрок видит только свои команды.
        let h = hint("/", LVL_PLAYER, false, &names).unwrap();
        assert!(h.list.contains(&"help".into()) && !h.list.contains(&"gamemode".into()), "{:?}", h.list);
        let h = hint("/g", LVL_ADMIN, false, &names).unwrap();
        assert_eq!(h.list, vec!["gamemode".to_string(), "give".to_string()]);
        // Творческий: /give себе доступна и игроку.
        assert!(hint("/gi", LVL_PLAYER, true, &names).unwrap().list.contains(&"give".into()));
        assert!(hint("/gi", LVL_PLAYER, false, &names).unwrap().list.is_empty());
        // Аргументы: режим, цель, предмет по слову после «_», моб, ник.
        let h = hint("/gamemode c", LVL_ADMIN, false, &names).unwrap();
        assert_eq!((h.list.clone(), h.start), (vec!["creative".to_string()], 10));
        assert!(h.usage.starts_with("/gamemode"));
        let h = hint("/give @", LVL_ADMIN, false, &names).unwrap();
        assert_eq!(h.list, vec!["@s", "@p", "@a"]);
        let h = hint("/give @s planks", LVL_ADMIN, false, &names).unwrap();
        assert!(h.list.iter().any(|n| n == "oak_planks"), "{:?}", h.list);
        let h = hint("/give @s ston", LVL_ADMIN, false, &names).unwrap();
        assert_eq!(h.list.first().map(String::as_str), Some("stone"));
        assert_eq!(hint("/summon z", LVL_ADMIN, false, &names).unwrap().list, vec!["zombie"]);
        assert_eq!(hint("/tp а", LVL_ADMIN, false, &names).unwrap().list, vec!["Аня"]);
        assert!(hint("/msg Bot прив", LVL_PLAYER, false, &names).unwrap().list.is_empty());
        // Начало слова — в символах (кириллица).
        assert_eq!(hint("/msg Ан", LVL_PLAYER, false, &names).unwrap().start, 5);
        assert!(hint("привет", 0, false, &names).is_none());
        assert!(hint("/xyz a", LVL_ADMIN, false, &names).unwrap().usage.contains("Неизвестная"));
    }

    #[test]
    fn coords_and_help() {
        assert_eq!(coord("~", 5.0), Some(5.0));
        assert_eq!(coord("~-2", 5.0), Some(3.0));
        assert_eq!(coord("10.5", 5.0), Some(10.5));
        assert_eq!(coord("x", 5.0), None);
        assert_eq!(coord("NaN", 0.0), None);
        let h = help(None, LVL_PLAYER, false).unwrap();
        assert!(h.contains("/spawn") && !h.contains("/ban"), "{h}");
        assert!(help(None, LVL_OWNER, false).unwrap().contains("/op"));
        assert!(help(Some("ban"), LVL_PLAYER, false).is_err());
        assert!(help(Some("/tp"), LVL_ADMIN, false).unwrap().starts_with("/tp"));
    }
}
