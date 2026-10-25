// Раздел «Звук»: общая громкость и по категориям Minecraft (блоки, игроки, существа, окружение, интерфейс). Читает их
// web/audio/engine.js из S каждый кадр. Формат строк — шапка web/settings.js. Громкость линейная, как в Minecraft: 100 % — как задумано.
const pct = (v) => `${Math.round(v * 100)} %`;

export const TAB = { id: "sound", label: "Звук", icon: "sound", sub: "Громкость мира и интерфейса", groups: ["Громкость", "Категории"] };
export const DEFAULTS = { volMaster: 1, volBlocks: 1, volPlayers: 1, volCreatures: 1, volAmbient: 1, volUi: 1 };
export const KNOWN = {
  volMaster: { tab: "sound", group: "Громкость", icon: "note", cost: 0, label: "Общая громкость", fmt: pct,
    hint: "Громкость всех звуков игры. Ноль — полная тишина." },
  volBlocks: { tab: "sound", group: "Категории", icon: "cube", cost: 0, label: "Блоки", fmt: pct,
    hint: "Ломание, ставление и удары по блокам, сундуки, печь и машины." },
  volPlayers: { tab: "sound", group: "Категории", icon: "hand", cost: 0, label: "Игроки", fmt: pct,
    hint: "Шаги, падение, урон, подбор предметов, плавание, еда." },
  volCreatures: { tab: "sound", group: "Категории", icon: "apple", cost: 0, label: "Существа", fmt: pct,
    hint: "Животные и мобы (звуки появятся вместе с ними)." },
  volAmbient: { tab: "sound", group: "Категории", icon: "mist", cost: 0, label: "Окружающая среда", fmt: pct,
    hint: "Вода, звуки под водой и фон мира." },
  volUi: { tab: "sound", group: "Категории", icon: "sign", cost: 0, label: "Интерфейс", fmt: pct,
    hint: "Щелчки кнопок меню. Слышны и на паузе." },
};
export const ROWS = [
  ["volMaster", "Общая громкость", "range", 0, 1, 0.05],
  ["volBlocks", "Блоки", "range", 0, 1, 0.05],
  ["volPlayers", "Игроки", "range", 0, 1, 0.05],
  ["volCreatures", "Существа", "range", 0, 1, 0.05],
  ["volAmbient", "Окружающая среда", "range", 0, 1, 0.05],
  ["volUi", "Интерфейс", "range", 0, 1, 0.05],
];
