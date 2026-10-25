// Раздел «Управление»: мышь и камера. Строки и умолчания регистрирует game/input.js (addSettings), подписи и подсказки —
// здесь; клавиши — ui/keys.js. Поле зрения (fov) — тоже строка input.js, но показана в «Графике» (settings/graphics.js).
export const TAB = { id: "controls", label: "Управление", icon: "controls", sub: "Мышь, камера и клавиши", groups: ["Мышь и камера"] };
export const KNOWN = {
  sens: { tab: "controls", group: "Мышь и камера", icon: "mouse", cost: 0, label: "Чувствительность мыши",
    hint: "Скорость поворота камеры. 100 % — как по умолчанию в Minecraft.", fmt: (v) => `${Math.round(v * 100)} %` },
  invertY: { tab: "controls", group: "Мышь и камера", icon: "updown", cost: 0, label: "Инверсия мыши по вертикали",
    hint: "Мышь вверх — взгляд вниз, как в авиасимуляторах." },
  bob: { tab: "controls", group: "Мышь и камера", icon: "wave", cost: 0, label: "Покачивание камеры",
    hint: "Камера покачивается в такт шагам. Выключите, если укачивает." },
  view3: { tab: "controls", group: "Мышь и камера", icon: "eye", cost: 0, label: "Вид сбоку (от третьего лица)",
    hint: "Камера за правым плечом, видна своя модель; у стены подъезжает ближе. Клавиша — V." },
};
