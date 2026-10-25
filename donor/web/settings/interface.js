// Раздел «Интерфейс»: меню (ui/menu.js: масштаб, размытие фона, анимации, подсказки, экономия кадров в меню), отладочный
// текст в углу, статистика (телеметрия). Формат — шапка web/settings.js.
import { nearest } from "./format.js";

const UI_SCALES = [0.85, 1, 1.15, 1.3];

export const TAB = { id: "interface", label: "Интерфейс", icon: "interface", sub: "Меню, подсказки и экран", groups: ["Меню", "Экран", "Статистика"] };
export const DEFAULTS = { uiScale: 1, uiBlur: true, uiAnim: true, uiHints: true, uiSaver: true, debugText: true };
export const KNOWN = {
  uiScale: { tab: "interface", group: "Меню", icon: "zoom", cost: 0, label: "Размер меню",
    hint: "Масштаб меню и настроек. Подстраивается под экран сам, здесь — поправка." },
  uiBlur: { tab: "interface", group: "Меню", icon: "mist", cost: 1, label: "Размытие мира за меню",
    hint: "Мир за меню слегка размывается. Немного нагружает видеокарту, пока меню открыто." },
  uiAnim: { tab: "interface", group: "Меню", icon: "sparkles", cost: 0, label: "Анимации меню",
    hint: "Плавное появление окон и переключателей." },
  uiHints: { tab: "interface", group: "Меню", icon: "info", cost: 0, label: "Подсказки к настройкам",
    hint: "Пояснения к настройкам во всплывающем окне — по «?» и при наведении на строку." },
  uiSaver: { tab: "interface", group: "Меню", icon: "battery", cost: -1, label: "Экономия в меню",
    hint: "Пока открыто меню, мир замирает: под меню — его размытый снимок, новые кадры не рисуются (видеокарта отдыхает, ноутбук не греется). Правка настройки ненадолго оживляет мир — для предпросмотра." },
  debugText: { tab: "interface", group: "Экран", icon: "text", cost: 0, label: "Отладочный текст в углу",
    hint: "Отладка поверх игры: F3 переключает краткий и подробный экран (FPS, дальность, плитки), Shift+F3 открывает профайлер." },
  // Статистика (строка телеметрии ниже; раздел — здесь, подсказка и цена — у строки)
  telemetry: { tab: "interface", group: "Статистика", pin: true },
};
export const ROWS = [
  ["uiScale", "Размер меню", "range", 0, UI_SCALES.length - 1, 1, (i) => UI_SCALES[i],
    (v) => UI_SCALES.indexOf(nearest(UI_SCALES, v)), (v) => `${Math.round(v * 100)} %`],
  ["uiBlur", "Размытие", "check"], ["uiAnim", "Анимации", "check"], ["uiHints", "Подсказки", "check"],
  ["uiSaver", "Экономия", "check"], ["debugText", "Отладочный текст", "check"],
];
// Телеметрия (engine/telemetry.js): сводка производительности на свой сайт раз в 10 с; адресом ?telemetry=0 — выкл.
// Регистрируется вызовом раздела «Отладка» (как было), раздел и группа — из KNOWN (pin).
export const TELEMETRY_ROWS = [["telemetry", "Отправлять статистику производительности", "check", { group: "Статистика", icon: "chart", cost: 0,
  hint: "Раз в 10 секунд игра отправляет на свой сайт сводку производительности: кадры в секунду, время кадра видеокарты и процессора, провалы и их причины, настройки графики, видеокарту и экран. Без имени и личных данных, IP не сохраняется. По ней видно, держатся ли у вас ровные 60 к/с и что мешает. На скорость не влияет." }]];
export const TELEMETRY_DEFAULTS = { telemetry: true };
