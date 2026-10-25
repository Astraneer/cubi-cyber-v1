//! Правила игрока, которые зависят от режима игры (G16, G17). Одно место на клиент и сервер: клиент спрашивает их, прежде
//! чем предсказать действие, сервер — прежде чем принять. Режим (GameMode) живёт в теле игрока (`Player.mode`), меняет его
//! только сервер (`/gamemode`, game/server/admin.rs) и сообщает клиенту `Msg::Mode`.

use crate::game::player::Player;

/// Режим игры (GameMode Minecraft 1.21.1; номера — как в MC: 0, 1, 3; «приключение» (2) не делаем).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum GameMode {
    #[default]
    Survival = 0,
    Creative = 1,
    Spectator = 3,
}

impl GameMode {
    pub(crate) const ALL: [GameMode; 3] = [GameMode::Survival, GameMode::Creative, GameMode::Spectator];
    pub(crate) fn code(self) -> u8 {
        self as u8
    }
    pub(crate) fn from_code(c: u8) -> GameMode {
        match c {
            1 => GameMode::Creative,
            3 => GameMode::Spectator,
            _ => GameMode::Survival,
        }
    }
    /// Имя Minecraft (`/gamemode creative`).
    pub(crate) fn mc(self) -> &'static str {
        match self {
            GameMode::Survival => "survival",
            GameMode::Creative => "creative",
            GameMode::Spectator => "spectator",
        }
    }
    /// Для игрока: «Режим игры: творческий».
    pub(crate) fn ru(self) -> &'static str {
        match self {
            GameMode::Survival => "выживание",
            GameMode::Creative => "творческий",
            GameMode::Spectator => "наблюдатель",
        }
    }
    /// Разбор аргумента `/gamemode`: имя MC, цифра MC (0, 1, 3), короткое (s, c, sp) или русское.
    pub(crate) fn parse(s: &str) -> Option<GameMode> {
        Some(match s.to_lowercase().as_str() {
            "survival" | "s" | "0" | "выживание" => GameMode::Survival,
            "creative" | "c" | "1" | "творческий" | "креатив" => GameMode::Creative,
            "spectator" | "sp" | "3" | "наблюдатель" => GameMode::Spectator,
            _ => return None,
        })
    }
}

/// Можно ли летать (PlayerAbilities.allowFlying): клавиша F включает полёт, сервер принимает Act::Flight и ходы с MV_FLY.
/// Во всех режимах — да: в выживании тоже (запрос пользователя 2026-10-23), в творческом — как MC, наблюдатель летает
/// всегда (сквозь стены — `may_noclip`).
pub(crate) fn may_fly(mode: GameMode) -> bool {
    match mode {
        GameMode::Survival | GameMode::Creative | GameMode::Spectator => true,
    }
}

/// Полёт сквозь стены (noclip): наблюдатель — всегда, остальным — только отладочный (H) при отладке сервера.
pub(crate) fn may_noclip(mode: GameMode, debug: bool) -> bool {
    mode == GameMode::Spectator || debug
}

/// Ломает и ставит как творческий (мгновенно, дальность 5, без дропа и расхода блоков): творческий режим или отладочный
/// полёт сквозь стены (тот — только при отладке сервера: без неё сервер ход с noclip не примет).
pub(crate) fn creative(p: &Player) -> bool {
    p.mode == GameMode::Creative || (p.noclip && p.mode != GameMode::Spectator)
}

/// Наблюдатель: не ломает, не ставит, не открывает окна, не бьёт, не подбирает; невидим для других.
pub(crate) fn spectator(p: &Player) -> bool {
    p.mode == GameMode::Spectator
}

/// Неуязвим (урона и смерти нет, мобы не замечают): творческий и наблюдатель. Отладочный noclip — у сервера отдельно.
pub(crate) fn invulnerable(p: &Player) -> bool {
    p.mode != GameMode::Survival
}

/// Перевести тело в режим mode (сервер при /gamemode и входе, клиент по Msg::Mode): наблюдатель — полёт сквозь стены;
/// выход из наблюдателя — полёт остаётся (не падать из стены), сквозь стены — нет.
pub(crate) fn apply_mode(p: &mut Player, mode: GameMode) {
    let was = p.mode;
    p.mode = mode;
    if mode == GameMode::Spectator {
        p.set_flying(true);
    } else if was == GameMode::Spectator {
        p.noclip = false;
        p.flying = true;
    }
}
