//! Physical-code bindings shared by winit and KeyboardEvent adapters. Conflicts
//! require an explicit swap; fixed mouse/Escape controls are never overwritten.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sneak,
    Sprint,
    SprintSafe,
    Flight,
    Inventory,
    Drop,
    ToolMode,
    Players,
    Chat,
    Command,
    Settings,
    Hud,
    View,
    Profiler,
    BugReport,
    Recipe,
    Uses,
    Bookmark,
    DebugFlight,
    Hotbar1,
    Hotbar2,
    Hotbar3,
    Hotbar4,
    Hotbar5,
    Hotbar6,
    Hotbar7,
    Hotbar8,
    Hotbar9,
}
pub const ACTION_COUNT: usize = 33;
#[derive(Clone, Copy)]
pub struct BindingDef {
    pub action: Action,
    pub label: &'static str,
    pub code: &'static str,
    pub in_window: bool,
}
macro_rules! b {
    ($action:ident,$label:literal,$code:literal) => {
        BindingDef {
            action: Action::$action,
            label: $label,
            code: $code,
            in_window: false,
        }
    };
    ($action:ident,$label:literal,$code:literal,window) => {
        BindingDef {
            action: Action::$action,
            label: $label,
            code: $code,
            in_window: true,
        }
    };
}
pub const DEFAULTS: [BindingDef; ACTION_COUNT] = [
    b!(Forward, "Вперёд", "KeyW"),
    b!(Back, "Назад", "KeyS"),
    b!(Left, "Влево", "KeyA"),
    b!(Right, "Вправо", "KeyD"),
    b!(Jump, "Прыжок", "Space"),
    b!(Sneak, "Присесть", "ShiftLeft"),
    b!(Sprint, "Бег", "ControlLeft"),
    b!(SprintSafe, "Бег — запасная клавиша", "CapsLock"),
    b!(Flight, "Полёт", "KeyF"),
    b!(Inventory, "Инвентарь", "KeyE"),
    b!(Drop, "Выбросить", "KeyQ"),
    b!(ToolMode, "Режим предмета", "KeyN"),
    b!(Players, "Список игроков", "Tab"),
    b!(Chat, "Чат", "KeyT"),
    b!(Command, "Команда", "Slash"),
    b!(Settings, "Настройки", "KeyO"),
    b!(Hud, "Скрыть интерфейс", "F1"),
    b!(View, "Вид от первого лица / за плечом", "KeyV"),
    b!(Profiler, "Отладка", "F3"),
    b!(BugReport, "Сообщить об ошибке", "F8"),
    b!(Recipe, "Как сделать", "KeyR", window),
    b!(Uses, "Где используется", "KeyU", window),
    b!(Bookmark, "Закладка", "KeyA", window),
    b!(DebugFlight, "Полёт сквозь стены", "KeyH"),
    b!(Hotbar1, "Слот 1", "Digit1"),
    b!(Hotbar2, "Слот 2", "Digit2"),
    b!(Hotbar3, "Слот 3", "Digit3"),
    b!(Hotbar4, "Слот 4", "Digit4"),
    b!(Hotbar5, "Слот 5", "Digit5"),
    b!(Hotbar6, "Слот 6", "Digit6"),
    b!(Hotbar7, "Слот 7", "Digit7"),
    b!(Hotbar8, "Слот 8", "Digit8"),
    b!(Hotbar9, "Слот 9", "Digit9"),
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingError {
    Invalid,
    Fixed,
    Conflict(Action),
    Format,
}
#[derive(Clone, Debug)]
pub struct Bindings {
    codes: Vec<String>,
}
impl Default for Bindings {
    fn default() -> Self {
        Self {
            codes: DEFAULTS.iter().map(|d| d.code.to_owned()).collect(),
        }
    }
}
impl Bindings {
    pub fn code(&self, action: Action) -> &str {
        &self.codes[action as usize]
    }
    pub fn action(&self, code: &str, in_window: bool) -> Option<Action> {
        DEFAULTS
            .iter()
            .find(|def| def.in_window == in_window && self.code(def.action) == code)
            .map(|def| def.action)
    }
    pub fn rebind(&mut self, action: Action, code: &str) -> Result<(), BindingError> {
        if code == "Escape" || code.starts_with("Mouse") {
            return Err(BindingError::Fixed);
        }
        if !valid_code(code) {
            return Err(BindingError::Invalid);
        }
        if let Some(conflict) = DEFAULTS.iter().find(|def| {
            def.action != action
                && def.in_window == DEFAULTS[action as usize].in_window
                && self.code(def.action) == code
        }) {
            return Err(BindingError::Conflict(conflict.action));
        }
        self.codes[action as usize] = code.to_owned();
        Ok(())
    }
    pub fn swap(&mut self, a: Action, b: Action) -> Result<(), BindingError> {
        if DEFAULTS[a as usize].in_window != DEFAULTS[b as usize].in_window {
            return Err(BindingError::Invalid);
        }
        self.codes.swap(a as usize, b as usize);
        Ok(())
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = b"KEY1".to_vec();
        bytes.push(ACTION_COUNT as u8);
        for code in &self.codes {
            bytes.push(code.len() as u8);
            bytes.extend_from_slice(code.as_bytes());
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, BindingError> {
        if bytes.len() > 4096
            || bytes.get(..5) != Some(&[b'K', b'E', b'Y', b'1', ACTION_COUNT as u8])
        {
            return Err(BindingError::Format);
        }
        let mut offset = 5;
        let mut codes = Vec::with_capacity(ACTION_COUNT);
        for _ in 0..ACTION_COUNT {
            let length = usize::from(*bytes.get(offset).ok_or(BindingError::Format)?);
            offset += 1;
            let code = std::str::from_utf8(
                bytes
                    .get(offset..offset + length)
                    .ok_or(BindingError::Format)?,
            )
            .map_err(|_| BindingError::Format)?;
            if !valid_code(code) || code == "Escape" || code.starts_with("Mouse") {
                return Err(BindingError::Format);
            }
            codes.push(code.to_owned());
            offset += length;
        }
        if offset != bytes.len() {
            return Err(BindingError::Format);
        }
        for a in 0..ACTION_COUNT {
            for b in a + 1..ACTION_COUNT {
                if DEFAULTS[a].in_window == DEFAULTS[b].in_window && codes[a] == codes[b] {
                    return Err(BindingError::Conflict(DEFAULTS[b].action));
                }
            }
        }
        Ok(Self { codes })
    }
}
fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 32 && code.bytes().all(|c| c.is_ascii_alphanumeric())
}
