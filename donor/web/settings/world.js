// Раздел «Мир»: время суток (в Rust — game.set_time_of_day, сутки идут тиками мира). Формат — шапка web/settings.js.
const hhmm = (v) => {
  const m = Math.round(v * 1440) % 1440;
  return `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
};
const dayWord = (v) => {
  const h = v * 24;
  return h < 5 ? "ночь" : h < 7 ? "рассвет" : h < 11 ? "утро" : h < 16 ? "день" : h < 17.5 ? "вечер" : h < 19 ? "закат" : "ночь";
};

export const TAB = { id: "world", label: "Мир", icon: "world", sub: "Время суток", groups: ["Время суток"] };
export const DEFAULTS = { time: 0.42, dayRun: false };
export const KNOWN = {
  time: { tab: "world", group: "Время суток", icon: "clock", cost: 0, label: "Время суток",
    hint: "Положение солнца: посмотрите мир на рассвете, в полдень или ночью.", fmt: (v) => `${hhmm(v)} · ${dayWord(v)}` },
  dayRun: { tab: "world", group: "Время суток", icon: "sun", cost: 0, label: "Время идёт",
    hint: "Сутки проходят за 4 минуты. Клавиша Y. В сетевой игре время ведёт сервер." },
};
export const ROWS = [
  ["time", "Время суток", "range", 0, 1, 0.005],
  ["dayRun", "Время идёт", "check"],
];
