// Клавиши игры: действия, назначения по умолчанию (как в Minecraft), переназначение и конфликты.
// Назначение — KeyboardEvent.code (физическая клавиша, раскладка не важна). Хранится в localStorage "voxel-keys"
// только то, что отличается от умолчания. Мышь (ЛКМ, ПКМ, СКМ, колесо) и Esc — фиксированы.
// Потребители: game/input.js (удержания и нажатия в мире), game/screen.js (окна), main.js (отладочные клавиши),
// ui/menu.js (O — настройки, F3 — профайлер, экран «Управление»).
// Ctrl, Alt, Win — «опасные»: вне захвата клавиатуры (полный экран + navigator.keyboard.lock, game/input.js) их
// сочетания забирают браузер и ОС (Ctrl+W закрывает вкладку, Alt — меню браузера). Действия на них тогда не
// работают (keys.is, keys.active), бег остаётся на двойном W и запасной клавише (sprintSafe).

const STORE = "voxel-keys";
const RISKY = /^(Control|Alt|Meta|OS)/;
/** Клавиша, сочетания с которой без захвата клавиатуры уходят браузеру или ОС. */
export const risky = (code) => !!code && RISKY.test(code);
let full = false; // захват клавиатуры действует (ставит game/input.js)

export const KEY_GROUPS = [
  ["move", "Движение"], ["act", "Действия"], ["hotbar", "Хотбар"], ["ui", "Интерфейс"], ["nei", "Рецепты (в окне)"], ["debug", "Отладка"],
];

export const ACTIONS = [
  { id: "forward", label: "Вперёд", group: "move", def: "KeyW", hint: "Двойное нажатие — бег" },
  { id: "back", label: "Назад", group: "move", def: "KeyS" },
  { id: "left", label: "Влево", group: "move", def: "KeyA" },
  { id: "right", label: "Вправо", group: "move", def: "KeyD" },
  { id: "jump", label: "Прыжок", group: "move", def: "Space", hint: "В воде — всплыть" },
  { id: "sneak", label: "Присесть", group: "move", def: "ShiftLeft", hint: "С края не упадёшь" },
  { id: "sprint", label: "Бег", group: "move", def: "ControlLeft", hint: "Вместе с «Вперёд»" },
  { id: "sprintSafe", label: "Бег — запасная клавиша", group: "move", def: "CapsLock", hint: "Работает и без полного экрана" },
  { id: "flight", label: "Полёт", group: "move", def: "KeyF", hint: "Вверх — прыжок, вниз — присесть; сел на землю — полёт выключен" },
  { id: "attack", label: "Ломать и бить", group: "act", mouse: "ЛКМ", fixed: true },
  { id: "use", label: "Ставить и использовать", group: "act", mouse: "ПКМ", fixed: true, hint: "ПКМ по верстаку — открыть" },
  { id: "pick", label: "Выбрать блок", group: "act", mouse: "СКМ", fixed: true },
  { id: "inventory", label: "Инвентарь и крафт", group: "act", def: "KeyE" },
  { id: "drop", label: "Выбросить предмет", group: "act", def: "KeyQ", hint: "С Ctrl — всю стопку" },
  { id: "mode", label: "Режим предмета", group: "act", def: "KeyN", hint: "Конфигуратор Mekanism; с Shift — назад" },
  ...Array.from({ length: 9 }, (_, i) => ({ id: `hotbar${i + 1}`, label: `Слот ${i + 1}`, group: "hotbar", def: `Digit${i + 1}` })),
  { id: "wheel", label: "Листать слоты", group: "hotbar", mouse: "Колесо", fixed: true },
  { id: "pause", label: "Пауза и меню", group: "ui", def: "Escape", fixed: true },
  { id: "playerList", label: "Список игроков", group: "ui", def: "Tab", hint: "Пока зажата" },
  { id: "chat", label: "Чат", group: "ui", def: "KeyT", hint: "Enter — отправить, Esc — закрыть" },
  { id: "command", label: "Чат с «/»", group: "ui", def: "Slash", hint: "Команды: /list, /help" },
  { id: "settings", label: "Настройки", group: "ui", def: "KeyO" },
  { id: "hud", label: "Скрыть интерфейс", group: "ui", def: "F1" },
  { id: "view", label: "Вид: от первого лица / сбоку", group: "ui", def: "KeyV", hint: "Камера за плечом, видна своя модель" },
  { id: "profiler", label: "Отладка F3 (с Shift — профайлер)", group: "ui", def: "F3" },
  { id: "bugreport", label: "Сообщить о баге", group: "ui", def: "F8", hint: "Кадр, отчёт и запись ввода — разработчику" },
  // Панель рецептов (G18, game/nei.js): только в окнах — с клавишами движения не спорят (screen: true).
  { id: "neiRecipe", label: "Как сделать", group: "nei", def: "KeyR", screen: true, hint: "Наведи на предмет в окне или панели" },
  { id: "neiUses", label: "Где используется", group: "nei", def: "KeyU", screen: true },
  { id: "neiBookmark", label: "Закладка", group: "nei", def: "KeyA", screen: true, hint: "Список закладок — слева от окна" },
  { id: "fly", label: "Полёт сквозь стены", group: "debug", def: "KeyH", hint: "Только с отладкой сервера" },
  { id: "kit", label: "Набор предметов", group: "debug", def: "KeyK" },
  { id: "shift", label: "Сдвиг на 10 блоков", group: "debug", def: "KeyJ" },
  { id: "viewUp", label: "Дальность больше", group: "debug", def: "BracketRight" },
  { id: "viewDown", label: "Дальность меньше", group: "debug", def: "BracketLeft" },
  { id: "nearUp", label: "Блоков вокруг больше", group: "debug", def: "Equal" },
  { id: "nearDown", label: "Блоков вокруг меньше", group: "debug", def: "Minus" },
  { id: "shadows", label: "Тени вкл / выкл", group: "debug", def: "KeyG" },
  { id: "dayRun", label: "Время идёт / стоит", group: "debug", def: "KeyY", hint: "Только без сервера: по сети время ведёт сервер" },
];
const BY_ID = Object.fromEntries(ACTIONS.map((a) => [a.id, a]));
// Подсказка в настройках: на Ctrl / Alt / Win — предупредить, что вне полного экрана действие молчит.
for (const a of ACTIONS) {
  if (a.fixed) continue;
  const base = a.hint || "";
  Object.defineProperty(a, "hint", { enumerable: true, get: () => (risky(map[a.id])
    ? `${base ? base + ". " : ""}${keyLabel(map[a.id])} — только в полном экране` : base) });
}

const NAMES = {
  Space: "Пробел", ShiftLeft: "Shift", ShiftRight: "Правый Shift", ControlLeft: "Ctrl", ControlRight: "Правый Ctrl",
  AltLeft: "Alt", AltRight: "Правый Alt", MetaLeft: "Win", MetaRight: "Правый Win", Escape: "Esc", Enter: "Enter",
  Tab: "Tab", CapsLock: "Caps Lock", Backspace: "Backspace", BracketLeft: "[", BracketRight: "]", Equal: "=",
  Minus: "−", Backquote: "`", Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", Backslash: "\\",
  IntlBackslash: "\\", ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→", Insert: "Insert",
  Delete: "Delete", Home: "Home", End: "End", PageUp: "Page Up", PageDown: "Page Down", ContextMenu: "Menu",
  NumpadAdd: "Num +", NumpadSubtract: "Num −", NumpadMultiply: "Num ×", NumpadDivide: "Num /",
  NumpadDecimal: "Num .", NumpadEnter: "Num Enter",
};
/** Название клавиши по code: «W», «Пробел», «Num 5». */
export function keyLabel(code) {
  if (!code) return "—";
  if (NAMES[code]) return NAMES[code];
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1];
  if ((m = /^Digit(\d)$/.exec(code))) return m[1];
  if ((m = /^Numpad(\d)$/.exec(code))) return "Num " + m[1];
  return code;
}

function load() {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem(STORE) || "{}") || {}; } catch {}
  const m = {};
  for (const a of ACTIONS) m[a.id] = !a.fixed && typeof saved[a.id] === "string" ? saved[a.id] : a.def || "";
  return m;
}
let map = load();
const subs = new Set();
function save() {
  const out = {};
  for (const a of ACTIONS) if (!a.fixed && map[a.id] !== a.def) out[a.id] = map[a.id];
  try { localStorage.setItem(STORE, JSON.stringify(out)); } catch {}
  for (const fn of subs) fn();
}

export const keys = {
  /** code назначенной клавиши ("" — не назначена). */
  code: (id) => map[id] || "",
  /** Нажата ли клавиша действия (Ctrl / Alt / Win — только под захватом клавиатуры). */
  is: (id, code) => !!code && map[id] === code && (full || !risky(code)),
  /** Номер слота хотбара 1…9 для клавиши или 0. */
  hotbarOf(code) {
    if (!code || (!full && risky(code))) return 0;
    for (let i = 1; i <= 9; i++) if (map["hotbar" + i] === code) return i;
    return 0;
  },
  /** Действие сейчас работает: назначено и не на «опасной» клавише вне захвата клавиатуры. */
  active: (id) => !!map[id] && (full || !risky(map[id])),
  risky,
  /** Захват клавиатуры действует (Ctrl, Alt, Win идут в игру). */
  get full() { return full; },
  setFull(v) { v = !!v; if (v === full) return; full = v; for (const fn of subs) fn(); },
  /** Клавиша назначена хоть на что-то (в игре у неё не должно быть браузерного действия). */
  bound: (code) => !!code && ACTIONS.some((a) => !a.fixed && map[a.id] === code),
  label: (id) => (BY_ID[id]?.mouse ? BY_ID[id].mouse : keyLabel(map[id])),
  action: (id) => BY_ID[id],
  isDefault: (id) => map[id] === (BY_ID[id]?.def || ""),
  set(id, code) {
    const a = BY_ID[id];
    if (!a || a.fixed) return;
    map[id] = code || "";
    save();
  },
  reset(id) { const a = BY_ID[id]; if (a && !a.fixed) { map[id] = a.def; save(); } },
  resetAll() { map = Object.fromEntries(ACTIONS.map((a) => [a.id, a.def || ""])); save(); },
  /** Конфликты: действие → другие действия на той же клавише. */
  conflicts() {
    const by = new Map();
    for (const a of ACTIONS) {
      const c = !a.mouse && map[a.id];
      if (!c) continue;
      if (!by.has(c)) by.set(c, []);
      by.get(c).push(a.id);
    }
    const out = new Map();
    // Клавиши окон (screen) не спорят с движением: в окне игрок не ходит.
    const clash = (a, b) => !((BY_ID[a].screen && BY_ID[b].group === "move") || (BY_ID[b].screen && BY_ID[a].group === "move"));
    for (const ids of by.values()) {
      if (ids.length < 2) continue;
      for (const id of ids) {
        const other = ids.filter((x) => x !== id && clash(id, x));
        if (other.length) out.set(id, other);
      }
    }
    return out;
  },
  /** Подписка на любые изменения назначений; возвращает отписку. */
  onChange(fn) { subs.add(fn); return () => subs.delete(fn); },
};
