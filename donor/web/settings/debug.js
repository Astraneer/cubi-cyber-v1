// Раздел «Отладка»: инструменты проверки движка — подкраска уровней плиток, границы, заморозка загрузки и отсечения
// (main.js: view.frozen), отсечение за рельефом (строка engine/occlusion.js). Формат — шапка web/settings.js.
export const TAB = { id: "debug", label: "Отладка", icon: "debug", sub: "Инструменты для проверки движка", groups: ["Плитки и отсечение"] };
export const DEFAULTS = { tint: false, borders: false, freeze: false };
export const KNOWN = {
  tint: { tab: "debug", group: "Плитки и отсечение", icon: "palette", cost: 0, label: "Подкрасить уровни плиток",
    hint: "Окрашивает плитки дали по уровню детальности (легенда ниже)." },
  borders: { tab: "debug", group: "Плитки и отсечение", icon: "grid", cost: 0, label: "Границы чанков и плиток",
    hint: "Рисует рамки кусков и плиток." },
  freeze: { tab: "debug", group: "Плитки и отсечение", icon: "snow", cost: 0, label: "Заморозить загрузку и отсечение",
    hint: "Мир перестаёт подгружаться и отсекаться — можно отлететь и посмотреть, что видит камера." },
  occlusion: { tab: "debug", group: "Плитки и отсечение", icon: "hidden", cost: -1, label: "Отсечение за рельефом (Hi-Z)",
    hint: "Не рисует плитки, спрятанные за горами. Обычно ускоряет; выключать — только для проверки." },
};
export const ROWS = [
  ["tint", "Подкрасить уровни плиток", "check"],
  ["borders", "Границы чанков и плиток", "check"],
  ["freeze", "Заморозить загрузку и отсечение", "check"],
];
