//! Машины Mekanism — блок-сущности v2 (Ф4 = C7, E15; MEK-01, MEK-03, MEK-04, MEK-12): одна сущность MekBe из
//! компонентов — слоты (предметы), батарея (энергия, Дж u64), бак (вещество или лава, мБ), тепло (тепловой генератор,
//! f64 как в моде: только + − × /, детерминированно), прогресс операции и вид «активна». Что из этого есть у машины и
//! как она себя ведёт — строка таблицы mek_def по виду (MekKind): ёмкость, расход, время, грани энергии, слоты и окно.
//! Новая машина — строка таблицы и шаг тика, а не правки по всему коду.
//!
//! Тик — фаза блок-сущностей в порядке появления (как печи, world/blockentity.rs ticking). Порядок внутри тика машины
//! (MEK-01 §2.1): отсчёт задержки вида → (выталкивание — M3) → слот энергии → доп. слот (вещество) → шаг рецепта.
//! Генератор (MEK-03 §1.2): отсчёт вида → отдать соседям на грани энергии (≤ max_out) → слот топлива → выработка.
//! Энергию соседям генератор и куб отдают сразу (кабелю — в запас сети, world/grid.rs; машине — в батарею); сети
//! раздают после всех блок-сущностей. Энергия генератора тика t у машины — в t+1 (через кабель) или сразу (вплотную
//! и машина тикает позже).
//! «Активна» — бит LIT (3) в ячейке (как у печи), с антидребезгом Mekanism (ActiveView, решение после MEK-01): вид —
//! свет, модель, звук-петля, частицы; логика машины его не читает.
//! Окно: слоты и виджеты — данные таблицы (MekDef.slots, widgets), значения — снимок view() раз в тик (он же пакет
//! Msg::WinData), пиксели и подсказки считает Rust (widget_values, tip) — JS только рисует (web/game/screen.js).

pub(crate) mod items;
pub(crate) mod recipes;
pub(crate) mod sides;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_m3;
#[cfg(test)]
mod tests_m4;

use crate::abi::Widget;
use crate::cell::{offset, Cell};
use crate::registry::blocks::MekKind;
use crate::registry::items::{
    idef, ItemId, I_BUCKET, I_ENERGY_TABLET, I_LAVA_BUCKET, I_UPGRADE_ENERGY, I_UPGRADE_GAS, I_UPGRADE_SPEED, I_WATER_BUCKET,
};
use crate::world::blockentity::{fuel_time, smelt_result};
use crate::world::stack::{Container, ItemStack};
use recipes::*;
use sides::*;

pub(crate) type Pos = [i32; 3];

/// Задержка повторного выключения вида (GeneralConfig.blockDeactivationDelay, MEK-01 §5.1).
pub(crate) const DEACTIVATION_DELAY: u8 = 60;

// Тепловой генератор (MEK-03 §2.4).
pub(crate) const HEAT_FUEL: f64 = 200.0;
pub(crate) const HEAT_LAVA_SIDE: f64 = 30.0;
pub(crate) const HEAT_BURN_MB: u32 = 10;
pub(crate) const HEAT_TANK_MB: u32 = 24_000;
pub(crate) const HEAT_C: f64 = 10.0;
pub(crate) const HEAT_WORK: f64 = 0.5;
pub(crate) const HEAT_MAX_PROD: u64 = 510;
const AIR_INV: f64 = 10_000.0;
const COND: f64 = 5.0;
const INSUL: f64 = 100.0;
/// Окружающая температура — константа 300 К (решение после MEK-03/12: климат биомов не берём).
pub(crate) const AMBIENT_K: f64 = 300.0;
/// Бак инфузора (MAX_INFUSE).
pub(crate) const INFUSE_TANK: u32 = 1000;
/// Выход и слоты куба энергии базового яруса.
pub(crate) const CUBE_RATE: u64 = 4000;
// Ветряк (MEK-03 §3.2): от 60 Дж/т (верх ≤ Y 24) линейно до 480 Дж/т (верх = последний блок мира, у нас 127).
pub(crate) const WIND_MIN: f64 = 60.0;
pub(crate) const WIND_MAX: f64 = 480.0;
pub(crate) const WIND_MIN_Y: i32 = 24;
/// Пересчёт множителя ветряка — раз в секунду.
pub(crate) const WIND_EVERY: u32 = 20;
// Планшет энергии (MEK-12 §6.1): 1 МДж, 5 000 Дж/т на вход и выход. Энергия — в поле damage стека единицами по 20 Дж
// (1 МДж = 50 000 < 65 536): стек Copy, по сети и в сохранение — как есть, клиент видит заряд без хранилища.
pub(crate) const TABLET_CAP: u64 = 1_000_000;
pub(crate) const TABLET_RATE: u64 = 5_000;
pub(crate) const TABLET_UNIT: u64 = 20;
// Улучшения (MEK-10 §5): до 8 скорости и 8 энергии; значения — таблицами целых (§5.2), а не powf.
pub(crate) const UPGRADE_MAX: u8 = 8;
/// Тиков на операцию 200 · 10^(−s/8), вниз.
pub(crate) const UP_TICKS: [u32; 9] = [200, 149, 112, 84, 63, 47, 35, 26, 20];
/// Дж/т при базе 50 по k = 2s − e (индекс k + 8): 50 · 10^(k/8), вверх.
pub(crate) const UP_J50: [u64; 25] =
    [5, 7, 9, 12, 16, 22, 29, 38, 50, 67, 89, 119, 159, 211, 282, 375, 500, 667, 890, 1186, 1582, 2109, 2812, 3750, 5000];
/// Буфер при базе 20 000 по e: 20 000 · 10^(e/8), вниз.
pub(crate) const UP_BUF20K: [u64; 9] = [20_000, 26_670, 35_565, 47_427, 63_245, 84_339, 112_468, 149_978, 200_000];
/// Установка из слота — в 21-м тике (счётчик 1…20, затем установка).
pub(crate) const UP_INSTALL_TICKS: u8 = 20;
/// Вид улучшения: 0 скорость, 1 энергия.
pub(crate) const UP_SPEED: usize = 0;
pub(crate) const UP_ENERGY: usize = 1;
/// M4: улучшение газа (только очиститель, MEK-10 §5.4; решение после MEK-04/06/10/11 — в M).
pub(crate) const UP_GAS: usize = 2;
/// Дж/т при базе 200 (очиститель) по k = 2s − e: 200 · 10^(k/8), вверх (MEK-10 §5.2).
pub(crate) const UP_J200: [u64; 25] = [
    20, 27, 36, 48, 64, 85, 113, 150, 200, 267, 356, 475, 633, 844, 1125, 1500, 2000, 2668, 3557, 4743, 6325, 8434, 11247, 14998, 20000,
];
/// Буфер при базе 80 000 (очиститель) и 160 000 (сепаратор) по e: отдельные таблицы — ⌊база · 10^(e/8)⌋ не делится
/// пропорционально.
pub(crate) const UP_BUF80K: [u64; 9] = [80_000, 106_681, 142_262, 189_709, 252_982, 337_357, 449_873, 599_915, 800_000];
pub(crate) const UP_BUF160K: [u64; 9] =
    [160_000, 213_363, 284_524, 379_419, 505_964, 674_714, 899_746, 1_199_830, 1_600_000];
/// Кислород очистителя на операцию при ровном расходе (единицы рецепта), индекс s − g + 8: round(200 · 10^((s−g)/8)).
pub(crate) const GAS_OP: [u32; 17] = [20, 27, 36, 47, 63, 84, 112, 150, 200, 267, 356, 474, 632, 843, 1125, 1500, 2000];

// M4: очиститель и сепаратор (MEK-11 §4.1, §4.2).
/// Бак кислорода очистителя (MAX_GAS).
pub(crate) const PURIFY_TANK: u32 = 210;
/// Сепаратор: 2 мБ воды → 2 мБ водорода + 1 мБ кислорода за операцию, 400 Дж (2 × 200 Дж/мБ водорода); 2^s операций
/// за тик; баки газов по 2 400 мБ, воды — 24 000.
pub(crate) const SEP_J_PER_OP: u64 = 400;
pub(crate) const SEP_GAS_CAP: u32 = 2_400;
pub(crate) const SEP_WATER_CAP: u32 = 24_000;
/// «Сброс излишка» держит ⌊2 400 · 0,85⌋ = 2 040 мБ, лишнее — до 1 024 мБ/т; выталкивание веществ — до 1 024 мБ/т на бак.
pub(crate) const KEEP_EXCESS: u32 = 2_040;
pub(crate) const CHEM_EJECT: u32 = 1_024;
/// Электрический насос (MEK-14 §11.4): бак 10 000 мБ, операция 19 тиков (+1 000 мБ из источника под собой), 100 Дж/т,
/// буфер 40 000 Дж, выход вверх до 256 мБ/т, поиск лавы — до 80 блоков.
pub(crate) const PUMP_TANK: u32 = 10_000;
pub(crate) const PUMP_OUT: u32 = 256;
pub(crate) const PUMP_RANGE: i32 = 80;
/// Режимы бака газа по кругу (кнопка окна): простой → сброс излишка → сброс.
pub(crate) const GM_IDLE: u8 = 0;
pub(crate) const GM_EXCESS: u8 = 1;
pub(crate) const GM_DUMP: u8 = 2;

/// Роль слота машины (MEK-08 §1.2: рамка по роли) — что в него можно положить.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// Вход рецепта.
    In,
    /// Выход: класть может только машина.
    Out,
    /// Энергия: редстоун (10 кДж) и предметы с энергией.
    Energy,
    /// Вещество: предмет → бак (уголь → углерод).
    Extra,
    /// Топливо теплового генератора: ведро лавы или горючее.
    Fuel,
    /// Зарядка предмета (планшет — с энергией в стеке, Ф4 позже).
    Charge,
    /// Разряд предмета в куб (редстоун).
    Discharge,
    /// Улучшения (MEK-10 §5.3: в моде — слот подокна «Улучшения»): установка через 20 тиков, вся стопка до 8.
    Upgrade,
    /// Жидкость (сепаратор, MEK-11 §1.4): ведро воды → бак +1 000 мБ, в слоте остаётся пустое ведро.
    Fluid,
}

/// Виджет окна: вид (abi Widget), прямоугольник в пикселях GUI (с рамкой 1 px), источник значения.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WidgetDef {
    pub(crate) kind: Widget,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) w: i16,
    pub(crate) h: i16,
}

/// Строка таблицы машин.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MekDef {
    pub(crate) title: &'static str,
    /// Ёмкость батареи, Дж.
    pub(crate) cap: u64,
    /// Расход за рабочий тик (машины), Дж.
    pub(crate) per_tick: u64,
    /// Тиков на операцию (машины).
    pub(crate) ticks: u32,
    /// Отдача за тик с граней энергии (генератор — 2 × наибольшая выработка, куб — выход яруса).
    pub(crate) max_out: u64,
    /// Слоты по порядку (он же порядок Shift-клика): роль и левый верх иконки в окне.
    pub(crate) slots: &'static [(Role, i16, i16)],
    pub(crate) widgets: &'static [WidgetDef],
    pub(crate) tank_cap: u32,
}

const fn wd(kind: Widget, x: i16, y: i16, w: i16, h: i16) -> WidgetDef {
    WidgetDef { kind, x, y, w, h }
}

// Слоты и виджеты окон (MEK-08 §2; координаты — левый верх иконки 16 × 16, как у ванильных окон).
const HEAT_SLOTS: [(Role, i16, i16); 2] = [(Role::Fuel, 17, 35), (Role::Charge, 143, 35)];
const HEAT_WIDGETS: [WidgetDef; 4] =
    [wd(Widget::Tank, 54, 17, 68, 40), wd(Widget::Energy, 163, 14, 8, 56), wd(Widget::Text, 54, 60, 68, 9), wd(Widget::Text, 8, 18, 40, 9)];
/// Инфузор: вещество, вход, выход, энергия.
pub(crate) const INF_EXTRA: usize = 0;
pub(crate) const INF_IN: usize = 1;
pub(crate) const INF_OUT: usize = 2;
pub(crate) const INF_ENERGY: usize = 3;
pub(crate) const INF_UPGRADE: usize = 4;
const INFUSER_SLOTS: [(Role, i16, i16); 5] =
    [(Role::Extra, 17, 35), (Role::In, 51, 43), (Role::Out, 109, 43), (Role::Energy, 143, 35), (Role::Upgrade, 143, 61)];
const INFUSER_WIDGETS: [WidgetDef; 6] = [
    wd(Widget::Tank, 6, 14, 6, 54),
    wd(Widget::Energy, 163, 14, 8, 56),
    wd(Widget::Progress, 72, 47, 32, 8),
    wd(Widget::Button, 16, 59, 21, 10),
    wd(Widget::Button, 109, 62, 21, 10),
    wd(Widget::Button, 109, 74, 21, 10),
];
const CUBE_SLOTS: [(Role, i16, i16); 2] = [(Role::Discharge, 17, 35), (Role::Charge, 143, 35)];
/// Камеры и плавильня (GuiElectricMachine, MEK-08 §2): вход, выход, энергия.
pub(crate) const CH_IN: usize = 0;
pub(crate) const CH_OUT: usize = 1;
pub(crate) const CH_ENERGY: usize = 2;
pub(crate) const CH_UPGRADE: usize = 3;
const CHAMBER_SLOTS: [(Role, i16, i16); 4] = [(Role::In, 64, 17), (Role::Out, 116, 35), (Role::Energy, 64, 53), (Role::Upgrade, 143, 61)];
/// Кнопка выталкивания предметов (в моде — в подокне сторон; у нас — в окне машины, MEK-09 §5.6).
/// Вторая кнопка — снять улучшения в слот (в моде — подокно «Улучшения»).
const CHAMBER_WIDGETS: [WidgetDef; 4] = [
    wd(Widget::Energy, 163, 15, 8, 56),
    wd(Widget::Progress, 86, 38, 28, 8),
    wd(Widget::Button, 112, 58, 21, 10),
    wd(Widget::Button, 112, 70, 21, 10),
];
const CUBE_WIDGETS: [WidgetDef; 2] = [wd(Widget::Energy, 54, 17, 68, 50), wd(Widget::Text, 54, 60, 68, 9)];
/// Ветряк (MEK-08 §2): слот зарядки справа, полоса энергии; тексты — выработка и «небо закрыто».
const WIND_SLOTS: [(Role, i16, i16); 1] = [(Role::Charge, 143, 35)];
const WIND_WIDGETS: [WidgetDef; 3] = [wd(Widget::Energy, 163, 14, 8, 56), wd(Widget::Text, 40, 24, 90, 9), wd(Widget::Text, 40, 40, 90, 9)];
/// Очиститель (GuiAdvancedElectricMachine, MEK-08 §2): вход, газ-предмет (кремень), выход, энергия, улучшения; бак
/// кислорода между входом и газ-слотом; кнопки — как у камер.
pub(crate) const PUR_IN: usize = 0;
pub(crate) const PUR_EXTRA: usize = 1;
pub(crate) const PUR_OUT: usize = 2;
pub(crate) const PUR_ENERGY: usize = 3;
const PURIFIER_SLOTS: [(Role, i16, i16); 5] =
    [(Role::In, 64, 17), (Role::Extra, 64, 53), (Role::Out, 116, 35), (Role::Energy, 39, 35), (Role::Upgrade, 143, 61)];
const PURIFIER_WIDGETS: [WidgetDef; 5] = [
    wd(Widget::Energy, 163, 15, 8, 56),
    wd(Widget::Tank, 68, 35, 8, 17),
    wd(Widget::Progress, 86, 38, 28, 8),
    wd(Widget::Button, 112, 58, 21, 10),
    wd(Widget::Button, 112, 70, 21, 10),
];
/// Сепаратор (MEK-08 §2, своя раскладка без слотов газ-выхода — они для джетпака): ведро воды, улучшения, энергия;
/// баки воды, водорода (лево), кислорода (право); кнопки режима баков газа с подписями; строка состояния.
pub(crate) const SEP_FLUID: usize = 0;
pub(crate) const SEP_ENERGY: usize = 2;
const SEPARATOR_SLOTS: [(Role, i16, i16); 3] = [(Role::Fluid, 26, 14), (Role::Upgrade, 122, 14), (Role::Energy, 143, 14)];
/// Насос (MEK-14 §11.4; своя раскладка без слотов вёдер): энергия; бак, энергия, строка состояния.
pub(crate) const PUMP_ENERGY: usize = 0;
const PUMP_SLOTS: [(Role, i16, i16); 1] = [(Role::Energy, 143, 35)];
const PUMP_WIDGETS: [WidgetDef; 3] = [wd(Widget::Tank, 6, 14, 18, 58), wd(Widget::Energy, 163, 14, 8, 56), wd(Widget::Text, 30, 30, 108, 9)];
const SEPARATOR_WIDGETS: [WidgetDef; 10] = [
    wd(Widget::Tank, 6, 14, 14, 58),
    wd(Widget::Tank, 48, 14, 16, 26),
    wd(Widget::Tank, 70, 14, 16, 26),
    wd(Widget::Energy, 163, 14, 8, 56),
    wd(Widget::Button, 26, 42, 14, 9),
    wd(Widget::Text, 43, 43, 118, 9),
    wd(Widget::Button, 26, 52, 14, 9),
    wd(Widget::Text, 43, 53, 118, 9),
    wd(Widget::Text, 26, 63, 136, 9),
    wd(Widget::Button, 122, 34, 21, 10),
];

/// Строка таблицы машин по виду (числа — MEK-01 §3.4, MEK-03 §2.4, MEK-12 §5.1).
pub(crate) fn mek_def(k: MekKind) -> &'static MekDef {
    const HEAT: MekDef = MekDef {
        title: "Тепловой генератор",
        cap: 160_000,
        per_tick: 0,
        ticks: 0,
        max_out: 2 * HEAT_MAX_PROD, // 1 020 = 2 × 510
        slots: &HEAT_SLOTS,
        widgets: &HEAT_WIDGETS,
        tank_cap: HEAT_TANK_MB,
    };
    const INFUSER: MekDef = MekDef {
        title: "Металлургический инфузор",
        cap: 20_000,
        per_tick: 50,
        ticks: 200,
        max_out: 0,
        slots: &INFUSER_SLOTS,
        widgets: &INFUSER_WIDGETS,
        tank_cap: INFUSE_TANK,
    };
    const CUBE: MekDef = MekDef {
        title: "Базовый куб энергии",
        cap: 4_000_000,
        per_tick: 0,
        ticks: 0,
        max_out: CUBE_RATE,
        slots: &CUBE_SLOTS,
        widgets: &CUBE_WIDGETS,
        tank_cap: 0,
    };
    // Камеры (MEK-06 §3): 50 Дж/т, буфер 20 000 Дж, 200 тиков — как инфузор; отличаются таблицей рецептов.
    const ENRICH: MekDef = MekDef {
        title: "Обогатительная камера",
        cap: 20_000,
        per_tick: 50,
        ticks: 200,
        max_out: 0,
        slots: &CHAMBER_SLOTS,
        widgets: &CHAMBER_WIDGETS,
        tank_cap: 0,
    };
    const SMELTER: MekDef = MekDef { title: "Плавильня с энергией", ..ENRICH };
    // Ветряк (MEK-03 §1.1, §3): буфер 200 кДж, отдача 2 × 480 = 960 Дж/т с лица и низа.
    const WIND: MekDef = MekDef {
        title: "Ветряной генератор",
        cap: 200_000,
        per_tick: 0,
        ticks: 0,
        max_out: 2 * WIND_MAX as u64,
        slots: &WIND_SLOTS,
        widgets: &WIND_WIDGETS,
        tank_cap: 0,
    };
    // M4: дробилка — камера с другой таблицей; очиститель — 200 Дж/т, буфер 80 кДж, 200 тиков, бак кислорода 210;
    // сепаратор — 400 Дж за операцию (расход не «за тик»: per_tick 0), буфер 160 кДж, бак воды 24 000.
    const CRUSHER: MekDef = MekDef { title: "Дробилка", ..ENRICH };
    const PURIFIER: MekDef = MekDef {
        title: "Очистительная камера",
        cap: 80_000,
        per_tick: 200,
        ticks: 200,
        max_out: 0,
        slots: &PURIFIER_SLOTS,
        widgets: &PURIFIER_WIDGETS,
        tank_cap: PURIFY_TANK,
    };
    const SEPARATOR: MekDef = MekDef {
        title: "Электролитический сепаратор",
        cap: 160_000,
        per_tick: 0,
        ticks: 0,
        max_out: 0,
        slots: &SEPARATOR_SLOTS,
        widgets: &SEPARATOR_WIDGETS,
        tank_cap: SEP_WATER_CAP,
    };
    const PUMP: MekDef = MekDef {
        title: "Электрический насос",
        cap: 40_000,
        per_tick: 100,
        ticks: 19,
        max_out: 0,
        slots: &PUMP_SLOTS,
        widgets: &PUMP_WIDGETS,
        tank_cap: PUMP_TANK,
    };
    match k {
        MekKind::Heat => &HEAT,
        MekKind::Pump => &PUMP,
        MekKind::Infuser => &INFUSER,
        MekKind::Cube => &CUBE,
        MekKind::Enrich => &ENRICH,
        MekKind::Smelter => &SMELTER,
        MekKind::Wind => &WIND,
        MekKind::Crusher => &CRUSHER,
        MekKind::Purifier => &PURIFIER,
        MekKind::Separator => &SEPARATOR,
    }
}

/// Грани энергии машины вида k с лицом facing на мировой грани f: (принимает, отдаёт). Тепловой — отдаёт лицом
/// (порт), инфузор и камеры — принимают всеми гранями (MEK-08 §5.4), куб — принимает всеми, кроме лица, лицом отдаёт.
pub(crate) fn energy_io(k: MekKind, facing: u8, f: u8) -> (bool, bool) {
    match k {
        MekKind::Heat => (false, f == facing),
        MekKind::Infuser | MekKind::Enrich | MekKind::Smelter | MekKind::Crusher | MekKind::Purifier | MekKind::Separator => {
            (true, false)
        }
        MekKind::Cube => (f != facing, f == facing),
        MekKind::Wind => (false, f == facing || f == crate::cell::F_DOWN),
        // Насос: энергия только сзади (контейнер на грани BACK, MEK-14 §11.4).
        MekKind::Pump => (f == dir_of(facing, R_BACK), false),
    }
}

/// Рукав передатчика вида k к машине c с её мировой грани f (MEK-14 §21.1): энергия — грани энергии; предметы — машины
/// со сторонами предметов; жидкость — сепаратор (вода), тепловой (лава), насос (верх — выход); газ — сепаратор (выходы),
/// очиститель (кислород).
pub(crate) fn transmitter_face(c: Cell, f: u8, k: crate::world::grid::Kind) -> bool {
    use crate::registry::blocks::{bdef, BeKind};
    use crate::world::grid::Kind;
    let Some(BeKind::Mek(m)) = bdef(crate::cell::block_of(c)).be else { return false };
    match k {
        Kind::Energy => {
            let (i, o) = cell_io(c, f);
            i || o
        }
        Kind::Item => !item_modes(m).is_empty(),
        Kind::Fluid => matches!(m, MekKind::Separator | MekKind::Heat) || (m == MekKind::Pump && f == crate::cell::F_UP),
        Kind::Gas => matches!(m, MekKind::Separator | MekKind::Purifier),
    }
}

/// Грани энергии ячейки c на мировой грани f: (принимает, отдаёт); не машина — (нет, нет).
pub(crate) fn cell_io(c: Cell, f: u8) -> (bool, bool) {
    use crate::registry::blocks::{bdef, facing_of, BeKind};
    match bdef(crate::cell::block_of(c)).be {
        Some(BeKind::Mek(k)) => energy_io(k, facing_of(c), f),
        _ => (false, false),
    }
}

/// Рукава кабеля в клетке p по соседям: кабель или грань машины с энергией (вход или выход) — труба.
pub(crate) fn cable_arms(w: &mut dyn MekWorld, p: Pos) -> u16 {
    use crate::world::grid::{is_cable, ARM_TUBE};
    let mut a = 0u16;
    for f in 0..6u8 {
        let c = w.cell(offset(p, f));
        let (i, o) = cell_io(c, f ^ 1);
        if is_cable(c) || i || o {
            a |= ARM_TUBE << (2 * f);
        }
    }
    a
}

/// Вид «активна» с антидребезгом Mekanism (MEK-01 §5.1): включение сразу, первое выключение сразу; выключение в течение
/// 60 тиков после прошлого откладывается, пока 60 тиков подряд не будет новых. logic — что делала машина, shown — бит.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ActiveView {
    pub(crate) logic: bool,
    pub(crate) shown: bool,
    pub(crate) delay: u8,
}

impl ActiveView {
    /// Начало тика машины: Some(вид), если вид догнал логику после задержки.
    pub(crate) fn countdown(&mut self) -> Option<bool> {
        if self.delay == 0 {
            return None;
        }
        self.delay -= 1;
        if self.delay == 0 && self.shown != self.logic {
            self.shown = self.logic;
            return Some(self.shown);
        }
        None
    }
    /// Итог тика: Some(вид), если вид сменился сейчас.
    pub(crate) fn set(&mut self, on: bool) -> Option<bool> {
        if on == self.logic {
            return None;
        }
        self.logic = on;
        if self.shown == on {
            return None;
        }
        if on {
            self.shown = true;
            return Some(true);
        }
        let now = self.delay == 0;
        self.delay = DEACTIVATION_DELAY;
        if now {
            self.shown = false;
            Some(false)
        } else {
            None
        }
    }
}

/// Бак: вещество (recipes SUB_*) и количество, мБ. Один вид за раз; пустой бак «забывает» вид.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Tank {
    pub(crate) sub: u8,
    pub(crate) amount: u32,
}

/// Операция машины под входом сейчас (рецепт любой таблицы в одном виде): key — номер рецепта в таблице машины
/// (у плавильни — номер входного предмета), вход ×in_n → выход ×out_n, вещество бака (инфузор) тратится в конце.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Op {
    pub(crate) key: u16,
    pub(crate) in_n: i32,
    pub(crate) out: ItemId,
    pub(crate) out_n: i32,
    pub(crate) amount: u32,
}

/// Итог шага рецепта (MEK-01 §17): из него «активна», флаги окна.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Idle,
    Reset,
    NoEnergy,
    NoRoom,
    WrongOutput,
    Worked,
    Finished,
    /// M4: очистителю не хватает кислорода на этот тик — пауза, прогресс держится (MEK-11 §4.1.2).
    NoGas,
    /// M4, сепаратор: нет воды; бак водорода / кислорода полон — встал (MEK-11 §4.2.2).
    NoWater,
    H2Full,
    O2Full,
}

impl Step {
    pub(crate) fn working(self) -> bool {
        matches!(self, Step::Worked | Step::Finished)
    }
    fn code(self) -> u8 {
        self as u8
    }
}

/// Что машине нужно от мира (WorldData; в тестах — заглушка).
pub(crate) trait MekWorld {
    fn cell(&mut self, p: Pos) -> Cell;
    /// Отдать до n Дж соседу клетки p за её гранью f: кабель — в запас сети, машина с входом на этой грани — в батарею.
    /// Ответ — сколько принято.
    fn push_energy(&mut self, p: Pos, f: u8, n: u64) -> u64;
    /// Положить стопку s в хранилище соседа клетки p за её гранью f (сундук, печь, машина через грань «вход»): ответ —
    /// сколько предметов принято (выталкивание, MEK-09 §6.4).
    fn push_items(&mut self, _p: Pos, _f: u8, _s: ItemStack) -> i32 {
        0
    }
    /// Отдать до n мБ вещества sub соседу клетки p за её гранью f (выталкивание веществ, MEK-09 §6.3): ответ — сколько
    /// принято. Принимает машина с баком под это вещество (очиститель — кислород всеми гранями).
    fn push_gas(&mut self, _p: Pos, _f: u8, _sub: u8, _n: u32) -> u32 {
        0
    }
    /// Убрать источник жидкости в клетке q (насос выкачал лаву): мир запишет воздух после тика машины. false — нельзя.
    fn drain(&mut self, _q: Pos) -> bool {
        false
    }
}

/// Блок-сущность машины Mekanism.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MekBe {
    pub(crate) kind: MekKind,
    pub(crate) slots: [ItemStack; 5],
    /// Батарея, Дж.
    pub(crate) energy: u64,
    pub(crate) active: ActiveView,
    /// Прогресс операции (тиков) и рецепт, который сейчас идёт (Op::key: номер в таблице машины).
    pub(crate) progress: u32,
    pub(crate) recipe: Option<u16>,
    pub(crate) tank: Tank,
    /// Запасённое тепло теплового генератора (0 — ещё не задано: при первом тике = 10 × Tокр).
    pub(crate) heat: f64,
    /// Выработано за прошлый тик (генератор) / получено за прошлый тик (куб) — окно.
    pub(crate) produced: u64,
    /// Получено в этом тике (куб: «скорость входа»).
    pub(crate) received: u64,
    /// Код последнего шага рецепта (Step) — предупреждения окна.
    pub(crate) last: u8,
    /// Шкала ядра куба 0…10 000 (не сохраняется: после загрузки «проявляется» заново).
    pub(crate) scale: u16,
    /// Счётчик тиков сущности (ветряк: пересчёт раз в 20 тиков от первого); не сохраняется.
    pub(crate) ticker: u32,
    /// Режимы граней для предметов по относительным граням (sides.rs: лицо, лево, право, зад, верх, низ).
    pub(crate) sides: [u8; 6],
    /// Выталкивание предметов включено; счётчик до следующего (не сохраняется: первое — в первом тике).
    pub(crate) eject: bool,
    pub(crate) eject_cd: u8,
    /// Установлено улучшений: скорость, энергия, газ (0…8); счётчик установки из слота (не сохраняется).
    pub(crate) upgrades: [u8; 3],
    pub(crate) install_cd: u8,
    /// M4, сепаратор: баки газов — водород (лево), кислород (право) — и их режимы (GM_*).
    pub(crate) gas: [Tank; 2],
    pub(crate) gas_mode: [u8; 2],
    /// M4, очиститель: кислорода израсходовано за текущую операцию (единицы рецепта; ровный расход, MEK-11 §4.1.2).
    pub(crate) gas_used: u32,
    /// M5, сепаратор: стороны газов по относительным граням (0 — нет, 1 — водород, 2 — кислород; конфигуратор).
    pub(crate) gas_sides: [u8; 6],
    /// M5, насос: выбранная жидкость (держится до сброса) и узлы поиска по озеру (лава; не сохраняются).
    pub(crate) pump_sub: u8,
    pub(crate) nodes: Vec<Pos>,
}

impl MekBe {
    pub(crate) fn new(kind: MekKind) -> MekBe {
        MekBe { kind, sides: default_sides(kind), gas_sides: default_gas_sides(kind), ..Default::default() }
    }
    pub(crate) fn def(&self) -> &'static MekDef {
        mek_def(self.kind)
    }
    /// Ёмкость с улучшениями энергии (MEK-10 §5.1: база · 10^(e/8), вниз; таблицы для баз 20, 80, 160 кДж).
    pub(crate) fn cap(&self) -> u64 {
        let base = self.def().cap;
        let e = self.upgrades[UP_ENERGY].min(UPGRADE_MAX) as usize;
        if e == 0 || !takes_upgrades(self.kind) {
            return base;
        }
        match base {
            20_000 => UP_BUF20K[e],
            80_000 => UP_BUF80K[e],
            160_000 => UP_BUF160K[e],
            _ => (base as u128 * UP_BUF20K[e] as u128 / 20_000) as u64,
        }
    }
    /// Операций сепаратора за тик: 2^скорость (MEK-10 §5.1).
    pub(crate) fn sep_ops(&self) -> u64 {
        1 << self.upgrades[UP_SPEED].min(UPGRADE_MAX)
    }
    /// Тиков на операцию с улучшениями скорости.
    pub(crate) fn ticks(&self) -> u32 {
        let base = self.def().ticks;
        let s = self.upgrades[UP_SPEED].min(UPGRADE_MAX) as usize;
        if s == 0 || base == 0 {
            base
        } else {
            (base as u64 * UP_TICKS[s] as u64 / 200).max(1) as u32
        }
    }
    /// Дж за рабочий тик с улучшениями: база · 10^((2s − e)/8), вверх.
    pub(crate) fn per_tick(&self) -> u64 {
        let base = self.def().per_tick;
        let [s, e, _] = self.upgrades.map(|n| n.min(UPGRADE_MAX) as i32);
        if (s == 0 && e == 0) || base == 0 {
            return base;
        }
        let k = (2 * s - e + 8) as usize;
        match base {
            50 => UP_J50[k],
            200 => UP_J200[k],
            _ => (base * UP_J50[k]).div_ceil(50),
        }
    }
    /// Установить до n улучшений вида u (сколько влезает до 8): ответ — сколько установлено. Запас энергии — не больше
    /// новой ёмкости.
    pub(crate) fn install(&mut self, u: usize, n: i32) -> i32 {
        if !supports_upgrade(self.kind, u) {
            return 0;
        }
        let k = n.min((UPGRADE_MAX - self.upgrades[u].min(UPGRADE_MAX)) as i32).max(0);
        self.upgrades[u] += k as u8;
        self.energy = self.energy.min(self.cap());
        k
    }
    /// Слот улучшений (TileComponentUpgrade.tickServer): поддерживаемое улучшение и есть место — счётчик 1…20, на 21-м
    /// тике установка всей стопки до 8; иначе счётчик 0.
    fn upgrade_tick(&mut self) {
        let Some(i) = self.def().slots.iter().position(|s| s.0 == Role::Upgrade) else { return };
        let s = self.slots[i];
        let room = upgrade_kind(s.id()).filter(|&u| supports_upgrade(self.kind, u) && self.upgrades[u] < UPGRADE_MAX);
        let Some(u) = room else {
            self.install_cd = 0;
            return;
        };
        if self.install_cd < UP_INSTALL_TICKS {
            self.install_cd += 1;
            return;
        }
        let n = self.install(u, s.count);
        self.slots[i] = if s.count > n { s.with_count(s.count - n) } else { ItemStack::EMPTY };
        self.install_cd = 0;
    }
    /// Кнопка «снять улучшения»: все улучшения первого установленного вида — в слот улучшений (если он пуст или там
    /// такие же); запас энергии — не больше новой ёмкости.
    fn unload_upgrades(&mut self) {
        let Some(i) = self.def().slots.iter().position(|s| s.0 == Role::Upgrade) else { return };
        for u in [UP_SPEED, UP_ENERGY, UP_GAS] {
            let n = self.upgrades[u] as i32;
            if n == 0 {
                continue;
            }
            let item = [I_UPGRADE_SPEED, I_UPGRADE_ENERGY, I_UPGRADE_GAS][u];
            let cur = self.slots[i];
            if !cur.is_empty() && cur.item != item {
                return;
            }
            let room = (idef(item).max_stack as i32 - cur.n()).min(n);
            if room <= 0 {
                return;
            }
            self.slots[i] = ItemStack::of(item, cur.n() + room);
            self.upgrades[u] -= room as u8;
            self.energy = self.energy.min(self.cap());
            return;
        }
    }
    pub(crate) fn free(&self) -> u64 {
        self.cap().saturating_sub(self.energy)
    }
    /// Принять энергию снаружи (кабель, генератор, куб): сколько влезет — своего предела скорости у машины нет.
    pub(crate) fn insert_energy(&mut self, n: u64) -> u64 {
        let a = n.min(self.free());
        self.energy += a;
        self.received += a;
        a
    }
    /// Сохраняемое (MEK-01 §10.1) строкой — сверка «туда и обратно» (save/tests.rs).
    #[allow(dead_code)]
    pub(crate) fn saved_key(&self) -> String {
        format!(
            "{:?} {:?} {} {} {:?} {:x} {:?} {:?} {} {:?} {:?} {:?} {} {:?} {}",
            self.kind,
            self.slots,
            self.energy,
            self.progress,
            self.tank,
            self.heat.to_bits(),
            self.active,
            self.sides,
            self.eject,
            self.upgrades,
            self.gas,
            self.gas_mode,
            self.gas_used,
            self.gas_sides,
            self.pump_sub
        )
    }
    /// Число слотов машины.
    pub(crate) fn nslots(&self) -> usize {
        self.def().slots.len()
    }
    /// Температура генератора, К.
    pub(crate) fn temp(&self) -> f64 {
        if self.heat == 0.0 {
            AMBIENT_K
        } else {
            self.heat / HEAT_C
        }
    }

    /// Фаза блок-сущностей. Ответ — новый вид «активна» для бита в клетке (как LIT печи).
    pub(crate) fn tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> Option<bool> {
        let mut shown = self.active.countdown();
        if takes_upgrades(self.kind) {
            self.upgrade_tick();
        }
        let on = match self.kind {
            MekKind::Heat => Some(self.heat_tick(p, facing, w)),
            MekKind::Infuser => Some(self.infuser_tick(p, facing, w)),
            MekKind::Enrich | MekKind::Smelter | MekKind::Crusher => Some(self.chamber_tick(p, facing, w)),
            MekKind::Purifier => Some(self.purifier_tick(p, facing, w)),
            MekKind::Separator => Some(self.separator_tick(p, facing, w)),
            MekKind::Cube => {
                self.cube_tick(p, facing, w);
                None
            }
            MekKind::Wind => Some(self.wind_tick(p, facing, w)),
            MekKind::Pump => Some(self.pump_tick(p, facing, w)),
        };
        if let Some(v) = on.and_then(|on| self.active.set(on)) {
            shown = Some(v);
        }
        shown
    }

    // ------------------------------------------------------------ тепловой генератор (MEK-03 §2.1)

    fn heat_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        // 3a. Отдать с лица ≤ 1 020 (выработанное в прошлом тике и раньше).
        let out = w.push_energy(p, facing, self.energy.min(self.def().max_out));
        self.energy -= out;
        // 3b. Слот зарядки: планшет — из буфера.
        self.charge_slot(1, u64::MAX);
        // 3c. Слот топлива → бак «лавы»: один предмет за тик; ведро лавы — целиком 1 000 мБ, в слоте остаётся ведро.
        let free = HEAT_TANK_MB - self.tank.amount;
        let s = self.slots[0];
        if !s.is_empty() && free > 0 {
            if s.item == I_LAVA_BUCKET {
                if free >= 1000 {
                    self.tank.amount += 1000;
                    self.slots[0] = ItemStack::of(I_BUCKET, 1);
                }
            } else {
                let mb = (fuel_time(s.item) / 20) as u32;
                if mb > 0 && mb <= free {
                    self.tank.amount += mb;
                    let rem = idef(s.item).remainder;
                    let f = &mut self.slots[0];
                    f.count -= 1;
                    if f.count <= 0 {
                        *f = if rem == 0 { ItemStack::EMPTY } else { ItemStack::of(rem, 1) };
                    }
                }
            }
        }
        if self.tank.amount > 0 {
            self.tank.sub = SUB_LAVA;
        }
        let prev = self.energy;
        let ta = AMBIENT_K;
        if self.heat == 0.0 {
            self.heat = HEAT_C * ta; // первая выдача: T = окружающей
        }
        // Пассив: 30 тепла за грань с лавой (6 соседей; «залит лавой» не берём — решение после MEK-03/12).
        let lava = (0..6u8).filter(|&f| crate::cell::block_of(w.cell(offset(p, f))) == crate::registry::ids::LAVA).count();
        let mut dq = HEAT_LAVA_SIDE * lava as f64;
        // Топливо горит, только пока буфер не полон.
        let burning = self.free() > 0 && self.tank.amount >= HEAT_BURN_MB;
        if burning {
            self.tank.amount -= HEAT_BURN_MB;
            dq += HEAT_FUEL;
        }
        if self.tank.amount == 0 {
            self.tank.sub = SUB_NONE;
        }
        // Карно и окружение — от температуры начала тика; тепло прибавляется в конце одним числом.
        let t = self.heat / HEAT_C;
        let lost = HEAT_WORK * (t - ta);
        dq -= lost;
        let eff = 1.0 - ta.min(t) / ta.max(t);
        let j = ((lost.abs() * eff) as u64).min(HEAT_MAX_PROD);
        self.energy += j.min(self.free());
        let per_side = |ins: f64| (t - ta) / (AIR_INV + ins + COND);
        let env = 5.0 * per_side(INSUL) + per_side(0.0);
        dq -= HEAT_C * env;
        self.heat += dq;
        self.produced = self.energy - prev;
        burning
    }

    // ------------------------------------------------------------ инфузор (MEK-04 §2.2, MEK-01 §2.2)

    fn infuser_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        let d = self.def();
        self.eject_items(p, facing, w);
        // 4b. Слот энергии: редстоун → 10 000 Дж, только если влезает целиком; планшет — до 5 000 Дж/т.
        self.energy_slot(INF_ENERGY, u64::MAX);
        // 4c. Слот вещества: 1 предмет → бак, если влезает целиком и вид совместим с баком.
        let x = self.slots[INF_EXTRA];
        if let Some((sub, mb)) = chem_of(x.id()) {
            let fits = self.tank.amount + mb <= d.tank_cap && (self.tank.amount == 0 || self.tank.sub == sub);
            if fits {
                self.tank.sub = sub;
                self.tank.amount += mb;
                take_one(&mut self.slots[INF_EXTRA]);
            }
        }
        // 4d. Рецепт.
        let step = self.op_step();
        self.last = step.code();
        step.working()
    }

    /// Камеры и плавильня (MEK-06 §3, MEK-01 §2.1): слот энергии → шаг рецепта.
    fn chamber_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        self.eject_items(p, facing, w);
        self.energy_slot(CH_ENERGY, u64::MAX);
        let step = self.op_step();
        self.last = step.code();
        step.working()
    }

    /// Очиститель (MEK-11 §4.1.2): выталкиватель → слот энергии → газ-слот (кремень → +10 мБ, если влезает целиком) →
    /// шаг рецепта (кислород каждый тик; нехватка — пауза).
    fn purifier_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        self.eject_items(p, facing, w);
        self.energy_slot(PUR_ENERGY, u64::MAX);
        let mb = oxygen_of(self.slots[PUR_EXTRA].id());
        if mb > 0 && self.insert_gas(SUB_OXYGEN, mb, true) == mb {
            take_one(&mut self.slots[PUR_EXTRA]);
        }
        let step = self.op_step();
        self.last = step.code();
        step.working()
    }

    /// Принять до n мБ вещества sub в бак (снаружи — выталкиватель соседа; dry — только если войдёт всё): ответ — сколько
    /// принято. Бак очистителя берёт только кислород; один вид за раз (MEK-11 §1.2).
    pub(crate) fn insert_gas(&mut self, sub: u8, n: u32, whole: bool) -> u32 {
        let room = self.sub_room(sub);
        if room == 0 || (whole && room < n) {
            return 0;
        }
        let a = n.min(room);
        self.tank.sub = sub;
        self.tank.amount += a;
        a
    }
    /// Свободное место бака под вещество sub снаружи (M5 — трубы и трубки): очиститель — кислород, сепаратор — вода,
    /// тепловой — лава. 0 — не принимает.
    pub(crate) fn sub_room(&self, sub: u8) -> u32 {
        let ok = matches!((self.kind, sub), (MekKind::Purifier, SUB_OXYGEN) | (MekKind::Separator, SUB_WATER) | (MekKind::Heat, SUB_LAVA));
        if !ok || (self.tank.amount > 0 && self.tank.sub != sub) {
            return 0;
        }
        self.def().tank_cap.saturating_sub(self.tank.amount)
    }
    /// Взять до max мБ через мировую грань dir (сторона PULL трубы, трубки): насос — бак через верх; сепаратор — газ
    /// стороны (не в режиме «сброс»). want — вещество сети (0 — любое). Ответ — (вещество, сколько).
    pub(crate) fn extract_sub(&mut self, facing: u8, dir: u8, want: u8, max: u32) -> (u8, u32) {
        let t = match self.kind {
            MekKind::Pump if dir == crate::cell::F_UP => &mut self.tank,
            MekKind::Separator => match self.gas_sides[rel_of(facing, dir)] {
                g @ 1..=2 if self.gas_mode[g as usize - 1] != GM_DUMP => &mut self.gas[g as usize - 1],
                _ => return (0, 0),
            },
            _ => return (0, 0),
        };
        if t.amount == 0 || (want != 0 && t.sub != want) {
            return (0, 0);
        }
        let n = t.amount.min(max);
        let s = t.sub;
        t.amount -= n;
        if t.amount == 0 {
            t.sub = SUB_NONE;
        }
        (s, n)
    }

    /// Кислород очистителя на этот тик при ровном расходе (MEK-11 §4.1.2 `constantUse`): осталось на операцию B − used,
    /// осталось тиков T − p: меньше тиков, чем газа, — ⌊(B − used)/(T − p)⌋, иначе 0. Ответ — (единиц, мБ = единиц × rate).
    fn gas_need(&self, rate: u32) -> (u32, u32) {
        let [s, _, g] = self.upgrades.map(|n| n.min(UPGRADE_MAX) as i32);
        let b = GAS_OP[(s - g + 8) as usize];
        let rem_gas = b.saturating_sub(self.gas_used);
        let rem_ticks = self.ticks().saturating_sub(self.progress).max(1);
        let m = if rem_gas < rem_ticks { 0 } else { rem_gas / rem_ticks };
        (m, m * rate)
    }

    /// Электролитический сепаратор (MEK-11 §4.2.2): выталкивание газов (каждый тик: водород — с левой грани, кислород —
    /// с правой; бак в режиме «сброс» не выталкивается) → слот энергии → ведро воды → операции → сброс газов по режимам.
    fn separator_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        // Выталкивание газов по сторонам газов (M5: конфигуратор; умолчание — водород слева, кислород справа): до 1 024
        // мБ/т на бак, грани по порядку относительных.
        for i in 0..2 {
            if self.gas_mode[i] == GM_DUMP {
                continue;
            }
            let mut left = CHEM_EJECT;
            for rel in 0..6 {
                let t = self.gas[i];
                if self.gas_sides[rel] as usize != i + 1 || t.amount == 0 || left == 0 {
                    continue;
                }
                let n = w.push_gas(p, dir_of(facing, rel), t.sub, t.amount.min(left)).min(t.amount);
                left -= n;
                self.gas[i].amount -= n;
                if self.gas[i].amount == 0 {
                    self.gas[i].sub = SUB_NONE;
                }
            }
        }
        self.energy_slot(SEP_ENERGY, u64::MAX);
        // Ведро воды (только одно в слоте) → +1 000 мБ, если влезает целиком; в слоте остаётся пустое ведро.
        let s = self.slots[SEP_FLUID];
        if s.item == I_WATER_BUCKET && s.count == 1 && SEP_WATER_CAP - self.tank.amount >= 1000 {
            self.tank = Tank { sub: SUB_WATER, amount: self.tank.amount + 1000 };
            self.slots[SEP_FLUID] = ItemStack::of(I_BUCKET, 1);
        }
        let [h2, o2] = self.gas.map(|t| t.amount);
        let both_excess = self.gas_mode == [GM_EXCESS; 2] && h2 >= KEEP_EXCESS && o2 >= KEEP_EXCESS;
        let ops = if both_excess {
            0
        } else {
            self.sep_ops()
                .min(self.tank.amount as u64 / 2)
                .min((SEP_GAS_CAP - h2) as u64 / 2)
                .min((SEP_GAS_CAP - o2) as u64)
                .min(self.energy / SEP_J_PER_OP)
        };
        let step = if ops > 0 {
            self.energy -= ops * SEP_J_PER_OP;
            self.tank.amount -= 2 * ops as u32;
            if self.tank.amount == 0 {
                self.tank.sub = SUB_NONE;
            }
            self.gas[0] = Tank { sub: SUB_HYDROGEN, amount: h2 + 2 * ops as u32 };
            self.gas[1] = Tank { sub: SUB_OXYGEN, amount: o2 + ops as u32 };
            Step::Worked
        } else if both_excess {
            Step::Idle
        } else if self.tank.amount < 2 {
            Step::NoWater
        } else if SEP_GAS_CAP - h2 < 2 {
            Step::H2Full
        } else if SEP_GAS_CAP - o2 < 1 {
            Step::O2Full
        } else {
            Step::NoEnergy
        };
        self.produced = ops * SEP_J_PER_OP;
        self.last = step.code();
        let dump = 8u32 << self.upgrades[UP_SPEED].min(UPGRADE_MAX);
        for i in 0..2 {
            let t = &mut self.gas[i];
            let n = match self.gas_mode[i] {
                GM_DUMP => t.amount.min(dump),
                GM_EXCESS if t.amount > KEEP_EXCESS => (t.amount - KEEP_EXCESS).min(CHEM_EJECT),
                _ => 0,
            };
            t.amount -= n;
            if t.amount == 0 {
                t.sub = SUB_NONE;
            }
        }
        ops > 0
    }

    /// Слот энергии: планшет — до 5 000 Дж/т (и не больше rate); редстоун — 1 предмет за тик, только если его энергия
    /// влезает целиком (MEK-01 §3.3, MEK-12 §6.2).
    fn energy_slot(&mut self, i: usize, rate: u64) {
        let s = self.slots[i];
        if is_battery(s.id()) {
            let n = stack_energy(&s).min(TABLET_RATE).min(rate).min(self.free()) / TABLET_UNIT * TABLET_UNIT;
            if n > 0 {
                self.energy += n;
                self.slots[i] = with_energy(s, stack_energy(&s) - n);
            }
            return;
        }
        let v = energy_of(s.id());
        if v > 0 && self.free() >= v {
            self.energy += v;
            take_one(&mut self.slots[i]);
        }
    }
    /// Слот зарядки (EnergyInventorySlot.drain): из буфера в планшет — до 5 000 Дж/т и не больше rate.
    fn charge_slot(&mut self, i: usize, rate: u64) {
        let s = self.slots[i];
        if !is_battery(s.id()) {
            return;
        }
        let have = stack_energy(&s);
        let n = self.energy.min(TABLET_RATE).min(rate).min(TABLET_CAP - have) / TABLET_UNIT * TABLET_UNIT;
        if n > 0 {
            self.energy -= n;
            self.slots[i] = with_energy(s, have + n);
        }
    }

    /// Выталкивание предметов (MEK-09 §6.2): раз в 11 тиков (счётчик стоит, пока выключено); по «выходным» режимам
    /// (выход, вход-выход) — их слоты выхода, по мировым граням с этим режимом в порядке D, U, N, S, W, E: на грань —
    /// одна стопка, сколько влезет.
    pub(crate) fn eject_items(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) {
        if !self.eject || !can_eject(self.kind) {
            return;
        }
        if self.eject_cd > 0 {
            self.eject_cd -= 1;
            return;
        }
        let d = self.def();
        for m in [S_OUT, S_IO] {
            let outs: Vec<usize> = self.mode_slots(m).into_iter().filter(|&i| d.slots[i].0 == Role::Out).collect();
            for dir in EJECT_ORDER {
                if self.side_at(facing, dir) != m {
                    continue;
                }
                for &i in &outs {
                    let s = self.slots[i];
                    if s.is_empty() {
                        continue;
                    }
                    let n = w.push_items(p, dir, s).min(s.count);
                    if n > 0 {
                        self.slots[i] = if s.count > n { s.with_count(s.count - n) } else { ItemStack::EMPTY };
                        break;
                    }
                }
            }
        }
        self.eject_cd = EJECT_DELAY;
    }

    // ------------------------------------------------------------ насос (MEK-14 §11.4)

    /// Слот энергии → если бак пуст или есть место на порцию: при энергии ≥ расхода — счётчик +1 (жидкость уже выбрана —
    /// расход сразу), на 19-м — операция (удалась — расход, если ещё не списан; нет — сброс выбора и узлов) → выход вверх.
    fn pump_tick(&mut self, p: Pos, _facing: u8, w: &mut dyn MekWorld) -> bool {
        self.energy_slot(PUMP_ENERGY, u64::MAX);
        let mut working = false;
        let use_ = self.per_tick();
        if self.tank.amount == 0 || self.tank.amount + 1000 <= PUMP_TANK {
            if self.energy >= use_ {
                let mut paid = false;
                if self.pump_sub != SUB_NONE {
                    self.energy -= use_;
                    paid = true;
                }
                self.progress += 1;
                working = true;
                if self.progress >= self.ticks() {
                    self.progress = 0;
                    if self.suck(p, w) {
                        if !paid {
                            self.energy = self.energy.saturating_sub(use_);
                        }
                    } else {
                        self.pump_sub = SUB_NONE;
                        self.nodes.clear();
                        working = false;
                    }
                }
                self.last = if working { Step::Worked } else { Step::Idle }.code();
            } else {
                self.last = Step::NoEnergy.code();
            }
        } else {
            self.last = Step::NoRoom.code();
        }
        if self.tank.amount > 0 {
            let n = w.push_gas(p, crate::cell::F_UP, self.tank.sub, self.tank.amount.min(PUMP_OUT)).min(self.tank.amount);
            self.tank.amount -= n;
            if self.tank.amount == 0 {
                self.tank.sub = SUB_NONE;
            }
        }
        working
    }
    /// Операция: источник прямо под насосом, иначе узлы по порядку — сам узел и его соседи в пределах 80 блоков.
    fn suck(&mut self, p: Pos, w: &mut dyn MekWorld) -> bool {
        let below = [p[0], p[1] - 1, p[2]];
        if self.take_source(below, w) {
            if !self.nodes.contains(&below) {
                self.nodes.push(below);
            }
            return true;
        }
        let mut nodes = std::mem::take(&mut self.nodes);
        nodes.sort_unstable();
        let i = 0; // голова: узел без подходящих соседей удаляется, следующий встаёт на его место
        while i < nodes.len() {
            let n = nodes[i];
            if self.take_source(n, w) {
                self.nodes = nodes;
                return true;
            }
            for f in 0..6u8 {
                let q = offset(n, f);
                let d2 = (0..3).map(|k| ((q[k] - p[k]) as i64).pow(2)).sum::<i64>();
                if d2 > (PUMP_RANGE as i64).pow(2) || nodes.contains(&q) {
                    continue;
                }
                if self.take_source(q, w) {
                    nodes.push(q);
                    self.nodes = nodes;
                    return true;
                }
            }
            nodes.remove(i);
        }
        self.nodes = nodes;
        false
    }
    /// Взять источник жидкости в клетке q (того же вида, что выбран и в баке; порция влезает): вода бесконечна — блок
    /// остаётся (правило мира «бесконечная вода» — решение после MEK-13/14), лава — убирается.
    fn take_source(&mut self, q: Pos, w: &mut dyn MekWorld) -> bool {
        use crate::game::fluids::{fluid_of, Liquid};
        let Some(fl) = fluid_of(w.cell(q)) else { return false };
        if !fl.still {
            return false;
        }
        let sub = if fl.liquid == Liquid::Water { SUB_WATER } else { SUB_LAVA };
        if (self.pump_sub != SUB_NONE && self.pump_sub != sub) || (self.tank.amount > 0 && self.tank.sub != sub) {
            return false;
        }
        if self.tank.amount + 1000 > PUMP_TANK {
            return false;
        }
        if sub == SUB_LAVA && !w.drain(q) {
            return false;
        }
        self.tank = Tank { sub, amount: self.tank.amount + 1000 };
        self.pump_sub = sub;
        true
    }

    // ------------------------------------------------------------ ветряк (MEK-03 §3.1)

    fn wind_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) -> bool {
        // 3a. Отдать с лица и низа ≤ 960 (сначала лицу, остаток — вниз).
        let mut left = self.energy.min(self.def().max_out);
        for f in [facing, crate::cell::F_DOWN] {
            let a = w.push_energy(p, f, left);
            left -= a;
            self.energy -= a;
        }
        // 3b. Слот зарядки.
        self.charge_slot(0, u64::MAX);
        // 3c. Раз в секунду (от первого тика) — выработка по высоте верха и небу; между пересчётами постоянна.
        if self.ticker % WIND_EVERY == 0 {
            let top = [p[0], p[1] + 4, p[2]];
            let open = sky_open(w, top);
            self.produced = if open { wind_power(p[1]) } else { 0 };
            self.last = !open as u8;
        }
        self.ticker = self.ticker.wrapping_add(1);
        self.energy += self.produced.min(self.free());
        self.produced > 0
    }

    /// Слоты входа и выхода рецепта.
    fn io_slots(&self) -> (usize, usize) {
        match self.kind {
            MekKind::Infuser => (INF_IN, INF_OUT),
            MekKind::Purifier => (PUR_IN, PUR_OUT),
            _ => (CH_IN, CH_OUT),
        }
    }

    /// Рецепт под входом и баком сейчас (полная проверка, с количеством входа и вещества).
    pub(crate) fn find_op(&self) -> Option<Op> {
        let input = self.slots[self.io_slots().0];
        let id = input.id();
        match self.kind {
            MekKind::Infuser => find_infuse(id, self.tank.sub, self.tank.amount).map(|i| {
                let r = INFUSE[i as usize];
                Op { key: i as u16, in_n: 1, out: r.out, out_n: 1, amount: r.amount }
            }),
            MekKind::Enrich => find_enrich(id).and_then(|i| {
                let r = ENRICH[i as usize];
                (input.n() >= r.in_n).then_some(Op { key: i as u16, in_n: r.in_n, out: r.out, out_n: r.out_n, amount: 0 })
            }),
            MekKind::Smelter => smelt_result(id).map(|out| Op { key: id, in_n: 1, out, out_n: 1, amount: 0 }),
            MekKind::Crusher => find_crush(id).and_then(|i| {
                let r = CRUSH[i as usize];
                (input.n() >= r.in_n).then_some(Op { key: i as u16, in_n: r.in_n, out: r.out, out_n: r.out_n, amount: 0 })
            }),
            // Очиститель: amount — кислород на тик (мБ); рецепт проверяет только вход — нехватка кислорода даёт паузу.
            MekKind::Purifier => find_purify(id).map(|i| {
                let r = PURIFY[i as usize];
                Op { key: i as u16, in_n: 1, out: r.out, out_n: r.out_n, amount: r.rate }
            }),
            MekKind::Heat | MekKind::Cube | MekKind::Wind | MekKind::Separator | MekKind::Pump => None,
        }
    }

    fn reset(&mut self) {
        self.progress = 0;
        self.gas_used = 0;
    }

    /// Шаг рецепта (MEK-01 §2.2, §4.1): полная проверка каждый тик (без паузы-кэша мода): кэш/вход → энергия → выход.
    /// Вещества меньше рецепта, не тот вид, вход пропал (или его меньше рецепта) — прогресс 0; нет энергии, места — пауза.
    pub(crate) fn op_step(&mut self) -> Step {
        let op = self.find_op();
        let keep = op.is_some() && op.map(|o| o.key) == self.recipe;
        if !keep {
            match op {
                Some(o) => {
                    if self.recipe.is_some() || self.progress >= self.ticks() {
                        self.reset();
                    }
                    self.recipe = Some(o.key);
                }
                None if self.recipe.is_none() => return Step::Idle,
                None => {
                    self.reset();
                    return Step::Reset;
                }
            }
        }
        let Some(r) = op else { return Step::Idle };
        // Очиститель: кислород на этот тик; пустой бак или меньше нужного — пауза (прогресс держится).
        let purify = self.kind == MekKind::Purifier;
        let (units, gas) = if purify { self.gas_need(r.amount) } else { (0, 0) };
        if purify && (self.tank.amount == 0 || self.tank.sub != SUB_OXYGEN || self.tank.amount < gas) {
            return Step::NoGas;
        }
        if self.energy < self.per_tick() {
            return Step::NoEnergy;
        }
        let (i_in, i_out) = self.io_slots();
        let out = self.slots[i_out];
        if !out.is_empty() {
            if out.item != r.out || out.damage != 0 {
                return Step::WrongOutput;
            }
            if out.count + r.out_n > idef(r.out).max_stack as i32 {
                return Step::NoRoom;
            }
        }
        self.energy -= self.per_tick();
        self.progress += 1;
        if purify {
            self.tank.amount -= gas;
            self.gas_used += units;
            if self.tank.amount == 0 {
                self.tank.sub = SUB_NONE;
            }
        }
        if self.progress >= self.ticks() {
            self.progress = 0;
            self.gas_used = 0;
            for _ in 0..r.in_n {
                take_one(&mut self.slots[i_in]);
            }
            if r.amount > 0 && self.kind == MekKind::Infuser {
                self.tank.amount -= r.amount;
                if self.tank.amount == 0 {
                    self.tank.sub = SUB_NONE;
                }
            }
            let o = &mut self.slots[i_out];
            if o.is_empty() {
                *o = ItemStack::of(r.out, r.out_n);
            } else {
                o.count += r.out_n;
            }
            return Step::Finished;
        }
        Step::Worked
    }

    /// Кнопка окна (виджет Button номер widget): у инфузора первая — «сброс» бака; кнопка выталкивания — вкл./выкл.
    pub(crate) fn press(&mut self, widget: usize) {
        if !self.def().widgets.get(widget).is_some_and(|w| w.kind == Widget::Button) {
            return;
        }
        match self.button(widget) {
            Btn::Eject => self.eject = !self.eject,
            Btn::Unload => self.unload_upgrades(),
            Btn::Dump => self.dump(),
            Btn::GasMode(t) => self.gas_mode[t] = (self.gas_mode[t] + 1) % 3,
            Btn::None => {}
        }
    }
    /// Назначение кнопки окна номер i: у камер — выталкивание, снять улучшения; у инфузора — сброс, выталкивание, снять;
    /// у сепаратора — режим бака водорода, кислорода, снять улучшения.
    fn button(&self, i: usize) -> Btn {
        let k = self.def().widgets[..i.min(self.def().widgets.len())].iter().filter(|w| w.kind == Widget::Button).count();
        use MekKind::*;
        match (self.kind, k) {
            (Infuser, 0) => Btn::Dump,
            (Infuser, 1) | (Enrich | Smelter | Crusher | Purifier, 0) => Btn::Eject,
            (Infuser, 2) | (Enrich | Smelter | Crusher | Purifier, 1) | (Separator, 2) => Btn::Unload,
            (Separator, t @ 0..=1) => Btn::GasMode(t),
            _ => Btn::None,
        }
    }
    /// Номер по порядку среди виджетов того же вида (бак 0, 1, 2; текст 0, 1, 2).
    fn nth_of(&self, i: usize) -> usize {
        let ws = self.def().widgets;
        let kind = ws[i.min(ws.len() - 1)].kind;
        ws[..i.min(ws.len())].iter().filter(|w| w.kind == kind).count()
    }
    /// Бак виджета i и его ёмкость: у сепаратора — вода, водород, кислород; у прочих — единственный бак.
    fn tank_at(&self, i: usize) -> (Tank, u32) {
        match (self.kind, self.nth_of(i)) {
            (MekKind::Separator, t @ 1..=2) => (self.gas[t - 1], SEP_GAS_CAP),
            _ => (self.tank, self.def().tank_cap),
        }
    }
    fn is_eject_button(&self, i: usize) -> bool {
        self.def().widgets.get(i).is_some_and(|w| w.kind == Widget::Button) && self.button(i) == Btn::Eject
    }
    /// «Сброс» бака инфузора (кнопка окна, MEK-04 §2.4): бак пуст сразу, текущая операция потеряна (даже если уголь
    /// в жёлтом слоте превратится в следующем тике до шага рецепта).
    pub(crate) fn dump(&mut self) {
        self.tank = Tank::default();
        self.progress = 0;
    }

    // ------------------------------------------------------------ куб энергии (MEK-12 §5.2)

    fn cube_tick(&mut self, p: Pos, facing: u8, w: &mut dyn MekWorld) {
        self.produced = std::mem::take(&mut self.received);
        // Выталкивание энергии с лица (выход) — каждый тик, ≤ 4 000.
        let out = w.push_energy(p, facing, self.energy.min(CUBE_RATE));
        self.energy -= out;
        // Слот «+» (зарядка предмета) и «−» (разряд предмета; редстоун — целиком, 1 предмет за тик) — до 4 000 Дж/т.
        self.charge_slot(1, CUBE_RATE);
        self.energy_slot(0, CUBE_RATE);
        self.scale = smooth_scale(self.scale, self.energy, self.cap());
    }

    // ------------------------------------------------------------ окно

    /// Снимок для окна (раз в тик; он же Msg::WinData): числа машины по виду.
    pub(crate) fn view(&self) -> Vec<i32> {
        let c = |v: u64| v.min(i32::MAX as u64) as i32;
        vec![
            c(self.energy),
            c(self.produced),
            self.progress as i32,
            self.tank.sub as i32,
            self.tank.amount as i32,
            (self.temp() * 100.0) as i32,
            self.last as i32,
            self.active.shown as i32,
            c(self.cap()),
            self.eject as i32,
            self.upgrades[UP_SPEED] as i32,
            self.upgrades[UP_ENERGY] as i32,
            self.gas[0].amount as i32,
            self.gas[1].amount as i32,
            self.gas_mode[0] as i32,
            self.gas_mode[1] as i32,
            self.upgrades[UP_GAS] as i32,
        ]
    }
    /// Клиент: числа окна с сервера — в свою копию сущности (подсказки и пиксели считаются из неё тем же кодом).
    pub(crate) fn apply_view(&mut self, v: &[i32]) {
        if v.len() < 8 {
            return;
        }
        self.energy = v[0].max(0) as u64;
        self.produced = v[1].max(0) as u64;
        self.progress = v[2].max(0) as u32;
        self.tank = Tank { sub: v[3] as u8, amount: v[4].max(0) as u32 };
        self.heat = v[5] as f64 / 100.0 * HEAT_C;
        self.last = v[6] as u8;
        self.active.shown = v[7] != 0;
        if let Some(&e) = v.get(9) {
            self.eject = e != 0;
        }
        if let (Some(&s), Some(&e)) = (v.get(10), v.get(11)) {
            self.upgrades[UP_SPEED] = s.clamp(0, 8) as u8;
            self.upgrades[UP_ENERGY] = e.clamp(0, 8) as u8;
        }
        if let [h, o, m0, m1, g] = v.get(12..17).unwrap_or(&[]) {
            let gas = |sub: u8, n: i32| Tank { sub: if n > 0 { sub } else { SUB_NONE }, amount: n.max(0) as u32 };
            self.gas = [gas(SUB_HYDROGEN, *h), gas(SUB_OXYGEN, *o)];
            self.gas_mode = [(*m0).clamp(0, 2) as u8, (*m1).clamp(0, 2) as u8];
            self.upgrades[UP_GAS] = (*g).clamp(0, 8) as u8;
        }
    }

    /// Значения виджетов для рисования: на виджет [пиксели заполнения, флаги: бит 0 — предупреждение, биты 8…31 — цвет].
    pub(crate) fn widget_values(&self) -> Vec<i32> {
        let d = self.def();
        let mut out = Vec::with_capacity(d.widgets.len() * 2);
        for w in d.widgets {
            let inner_h = (w.h - 2).max(0) as u64;
            let inner_w = (w.w - 2).max(0) as u64;
            let (px, flags) = match w.kind {
                Widget::Energy => {
                    let user = self.per_tick() > 0 || self.kind == MekKind::Separator;
                    let warn = self.last == Step::NoEnergy as u8 && user;
                    (fill(self.energy, self.cap(), inner_h), warn as i32)
                }
                Widget::Tank => {
                    let i = out.len() / 2;
                    let (t, cap) = self.tank_at(i);
                    let warn = match (self.kind, self.nth_of(i)) {
                        (MekKind::Purifier, _) => self.last == Step::NoGas as u8,
                        (MekKind::Separator, 0) => self.last == Step::NoWater as u8,
                        (MekKind::Separator, 1) => self.last == Step::H2Full as u8,
                        (MekKind::Separator, _) => self.last == Step::O2Full as u8,
                        _ => false,
                    };
                    let col = sub_color(t.sub) as i32;
                    (fill(t.amount as u64, cap as u64, inner_h), col << 8 | warn as i32)
                }
                Widget::Progress => {
                    let warn = matches!(self.last, x if x == Step::NoRoom as u8 || x == Step::WrongOutput as u8);
                    (if self.ticks() == 0 { 0 } else { fill(self.progress as u64, self.ticks() as u64, inner_w) }, warn as i32)
                }
                Widget::Button if self.is_eject_button(out.len() / 2) => {
                    (0, (if self.eject { mode_color(S_OUT) } else { 0x8B8B8B } as i32) << 8)
                }
                Widget::Button if self.button(out.len() / 2) == Btn::Unload => (0, 0xFFAA00 << 8),
                Widget::Button if matches!(self.button(out.len() / 2), Btn::GasMode(_)) => {
                    let Btn::GasMode(t) = self.button(out.len() / 2) else { unreachable!() };
                    (0, (gas_mode_color(self.gas_mode[t]) as i32) << 8)
                }
                Widget::Button | Widget::Text => (0, 0),
            };
            out.push(px);
            out.push(flags);
        }
        out
    }

    /// Подсказка (или текст) виджета i: наш формат чисел (MEK-08 §11.3).
    pub(crate) fn tip(&self, i: usize) -> String {
        let d = self.def();
        let Some(w) = d.widgets.get(i) else { return String::new() };
        match (self.kind, w.kind) {
            (_, Widget::Energy) => {
                let mut s = format!("{} / {}", joules(self.energy), joules(self.cap()));
                use MekKind::*;
                match self.kind {
                    Infuser | Enrich | Smelter | Crusher | Purifier | Pump if self.active.shown => {
                        s += &format!("\nИспользует: {}/т", joules(self.per_tick()))
                    }
                    Infuser | Enrich | Smelter | Crusher | Purifier | Pump => {}
                    Separator => s += &format!("\nИспользует: {}/т", joules(self.produced)),
                    MekKind::Heat => s += &format!("\nВырабатывает: {}/т\nМакс. выход: {}/т", joules(self.produced), joules(d.max_out)),
                    MekKind::Cube => s += &format!("\nПолучено: {}/т\nМакс. выход: {}/т", joules(self.produced), joules(d.max_out)),
                    MekKind::Wind => s += &format!("\nВырабатывает: {}/т\nМакс. выход: {}/т", joules(self.produced), joules(d.max_out)),
                }
                s
            }
            (_, Widget::Tank) => {
                let (t, cap) = self.tank_at(i);
                let mut s = if t.amount == 0 {
                    format!("Пусто (из {} мБ)", thousands(cap as u64))
                } else {
                    format!("{}: {} / {} мБ", sub_name(t.sub), thousands(t.amount as u64), thousands(cap as u64))
                };
                match (self.kind, self.nth_of(i)) {
                    (MekKind::Separator, 1) if self.last == Step::H2Full as u8 => {
                        s += "\nБак водорода полон — сепаратор стоит.\nВключите «Сброс» или «Сброс излишка»."
                    }
                    (MekKind::Separator, 2) if self.last == Step::O2Full as u8 => {
                        s += "\nБак кислорода полон — сепаратор стоит.\nПоставьте очиститель справа или включите сброс."
                    }
                    (MekKind::Purifier, _) if self.last == Step::NoGas as u8 => s += "\nНет кислорода — пауза",
                    _ => {}
                }
                s
            }
            (_, Widget::Button) if matches!(self.button(i), Btn::GasMode(_)) => {
                let Btn::GasMode(t) = self.button(i) else { unreachable!() };
                format!("{}: {}\nКлик — следующий режим", ["Водород", "Кислород"][t], gas_mode_name(self.gas_mode[t]))
            }
            (_, Widget::Button) if self.is_eject_button(i) => {
                format!("Выталкивание предметов: {}", if self.eject { "вкл." } else { "выкл." })
            }
            (_, Widget::Button) if self.button(i) == Btn::Unload => {
                let mut s =
                    format!("Снять улучшения в слот\nСкорость: {} / 8, энергия: {} / 8", self.upgrades[UP_SPEED], self.upgrades[UP_ENERGY]);
                if supports_upgrade(self.kind, UP_GAS) {
                    s += &format!(", газ: {} / 8", self.upgrades[UP_GAS]);
                }
                s
            }
            (_, Widget::Button) => "Сброс: опустошить бак".to_string(),
            (_, Widget::Progress) => match self.last {
                x if x == Step::NoRoom as u8 => "Нет места в выходе".to_string(),
                x if x == Step::WrongOutput as u8 => "В выходе другой предмет".to_string(),
                x if x == Step::NoEnergy as u8 => "Не хватает энергии".to_string(),
                x if x == Step::NoGas as u8 => "Нет кислорода — пауза".to_string(),
                _ => String::new(),
            },
            (MekKind::Separator, Widget::Text) => match self.nth_of(i) {
                0 => format!("H2: {}", gas_mode_name(self.gas_mode[0])),
                1 => format!("O2: {}", gas_mode_name(self.gas_mode[1])),
                _ => match self.last {
                    x if x == Step::H2Full as u8 => "Стоит: водород некуда деть!".to_string(),
                    x if x == Step::O2Full as u8 => "Стоит: кислород некуда деть!".to_string(),
                    x if x == Step::NoWater as u8 => "Нет воды: ведро — в слот".to_string(),
                    x if x == Step::NoEnergy as u8 => "Не хватает энергии".to_string(),
                    _ if self.active.shown => {
                        let ops = self.produced / SEP_J_PER_OP;
                        format!("Даёт H2 {} и O2 {} мБ/т", 2 * ops, ops)
                    }
                    _ => String::new(),
                },
            },
            (MekKind::Pump, Widget::Text) => match self.last {
                x if x == Step::NoEnergy as u8 => "Не хватает энергии".to_string(),
                x if x == Step::NoRoom as u8 => "Бак полон: выход — только вверх".to_string(),
                _ if self.pump_sub == SUB_NONE => "Нет источника под насосом".to_string(),
                _ => format!("Качает: {}", sub_name(self.pump_sub)),
            },
            (MekKind::Heat, Widget::Text) if i == 2 => format!("{}/т", joules(self.produced)),
            (MekKind::Heat, Widget::Text) => format!("{} K", self.temp() as i64),
            (MekKind::Cube, Widget::Text) => format!("{} / {}", joules(self.energy), joules(self.cap())),
            (MekKind::Wind, Widget::Text) if i == 1 => format!("Выработка: {}/т", joules(self.produced)),
            (MekKind::Wind, Widget::Text) => (if self.last != 0 { "Небо закрыто" } else { "Ветер есть" }).to_string(),
            _ => String::new(),
        }
    }
}

/// Пиксели заполнения полосы: ⌊доля × длина⌋, ненулевой запас — хотя бы 1 пиксель (MEK-08 §1.2).
pub(crate) fn fill(v: u64, cap: u64, len: u64) -> i32 {
    if cap == 0 || v == 0 {
        return 0;
    }
    ((v.min(cap) as u128 * len as u128 / cap as u128) as i32).max(1)
}

/// Сглаживание шкалы ядра куба (MekanismUtils.getScale) в целых: 10 000 = полный, 100 = порог 0.01 (MEK-03 §11).
pub(crate) fn smooth_scale(prev: u16, stored: u64, cap: u64) -> u16 {
    let target = if cap == 0 { 0 } else { (stored as u128 * 10_000 / cap as u128) as u16 };
    let diff = prev.abs_diff(target);
    if diff > 100 {
        ((9 * prev as u32 + target as u32) / 10) as u16
    } else if stored == cap && stored > 0 {
        target
    } else if stored > 0 && prev == 0 {
        target.max(1)
    } else if stored == 0 && prev < 100 {
        0
    } else {
        prev
    }
}

/// Кнопка окна машины.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    None,
    Dump,
    Eject,
    Unload,
    /// Режим бака газа сепаратора (0 — водород, 1 — кислород).
    GasMode(usize),
}

/// Принимает ли машина улучшения скорости и энергии (MEK-10 §5.4: камеры, дробилка, плавильня, инфузор, очиститель,
/// сепаратор; генераторы и куб — нет).
pub(crate) fn takes_upgrades(k: MekKind) -> bool {
    use MekKind::*;
    matches!(k, Infuser | Enrich | Smelter | Crusher | Purifier | Separator)
}
/// Принимает ли машина вида k улучшение u (газ — только очиститель).
pub(crate) fn supports_upgrade(k: MekKind, u: usize) -> bool {
    takes_upgrades(k) && (u < UP_GAS || (u == UP_GAS && k == MekKind::Purifier))
}
/// Вид улучшения предмета: Some(UP_SPEED / UP_ENERGY / UP_GAS).
pub(crate) fn upgrade_kind(item: ItemId) -> Option<usize> {
    match item {
        I_UPGRADE_SPEED => Some(UP_SPEED),
        I_UPGRADE_ENERGY => Some(UP_ENERGY),
        I_UPGRADE_GAS => Some(UP_GAS),
        _ => None,
    }
}
/// Имя режима бака газа (GM_*).
pub(crate) fn gas_mode_name(m: u8) -> &'static str {
    ["простой", "сброс излишка", "сброс"][m.min(2) as usize]
}
/// Цвет черты кнопки режима: простой — серый, сброс излишка — жёлтый, сброс — красный.
pub(crate) fn gas_mode_color(m: u8) -> u32 {
    [0x8B8B8B, 0xFFAA00, 0xC9071F][m.min(2) as usize]
}

/// Выработка ветряка с основанием на высоте y, Дж/т (MEK-03 §3.2: верх = основание + 4, зажим 24…127, линейно 60…480).
pub(crate) fn wind_power(y: i32) -> u64 {
    let max_y = crate::cell::H - 1;
    let top = (y + 4).clamp(WIND_MIN_Y, max_y);
    (WIND_MIN + (WIND_MAX - WIND_MIN) / (max_y - WIND_MIN_Y) as f64 * (top - WIND_MIN_Y) as f64) as u64
}

/// Клетка видит небо (canSeeSky: небесный свет 15) — в ней нет жидкости, над ней нет листвы, жидкости и полных
/// непрозрачных блоков; стекло, растения, мачты ветряков свет неба пропускают (тот же счёт, что у мобов: sky_light).
pub(crate) fn sky_open(w: &mut dyn MekWorld, p: Pos) -> bool {
    use crate::registry::blocks::{bdef, collision_of};
    if bdef(crate::cell::block_of(w.cell(p))).fluid != 0 {
        return false;
    }
    for y in p[1] + 1..crate::cell::H {
        let c = w.cell([p[0], y, p[2]]);
        let d = bdef(crate::cell::block_of(c));
        if d.leaves || d.fluid != 0 || (d.suffocates && !collision_of(c).is_empty()) {
            return false;
        }
    }
    true
}

/// Предмет-батарея (планшет энергии): энергия в стеке.
pub(crate) fn is_battery(item: ItemId) -> bool {
    item == I_ENERGY_TABLET
}
/// Энергия в стеке (планшет: damage × 20 Дж), Дж.
pub(crate) fn stack_energy(s: &ItemStack) -> u64 {
    if is_battery(s.id()) {
        s.damage as u64 * TABLET_UNIT
    } else {
        0
    }
}
/// Тот же стек с энергией e (кратно 20 Дж, не больше ёмкости).
pub(crate) fn with_energy(s: ItemStack, e: u64) -> ItemStack {
    ItemStack { damage: (e.min(TABLET_CAP) / TABLET_UNIT) as u16, ..s }
}

/// −1 предмет в слоте (пусто — пусто).
fn take_one(s: &mut ItemStack) {
    s.count -= 1;
    if s.count <= 0 {
        *s = ItemStack::EMPTY;
    }
}

/// Энергия нашим форматом (MEK-08 §11.3): приставка по величине, обрезание до 2 знаков, запятая, без «,0».
pub(crate) fn joules(j: u64) -> String {
    let (div, unit) = match j {
        0..=999 => (1, "Дж"),
        1_000..=999_999 => (1_000, "кДж"),
        1_000_000..=999_999_999 => (1_000_000, "МДж"),
        _ => (1_000_000_000, "ГДж"),
    };
    let whole = j / div;
    let frac = if div == 1 { 0 } else { j % div * 100 / div };
    if frac == 0 {
        format!("{whole} {unit}")
    } else if frac % 10 == 0 {
        format!("{whole},{} {unit}", frac / 10)
    } else {
        format!("{whole},{frac:02} {unit}")
    }
}
/// Число с разделителем тысяч (пробел): 24 000.
pub(crate) fn thousands(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Можно ли положить предмет в слот роли r машины вида k (Slot.canInsert; по предмету, без состояния бака — M1).
pub(crate) fn role_accepts(k: MekKind, r: Role, item: ItemId) -> bool {
    if item == 0 {
        return false;
    }
    match r {
        Role::In => match k {
            MekKind::Infuser => infusable(item),
            MekKind::Enrich => find_enrich(item).is_some(),
            MekKind::Smelter => smelt_result(item).is_some(),
            MekKind::Crusher => find_crush(item).is_some(),
            MekKind::Purifier => find_purify(item).is_some(),
            MekKind::Heat | MekKind::Cube | MekKind::Wind | MekKind::Separator | MekKind::Pump => false,
        },
        Role::Out => false,
        Role::Charge => is_battery(item),
        Role::Upgrade => upgrade_kind(item).is_some_and(|u| supports_upgrade(k, u)),
        Role::Energy | Role::Discharge => energy_of(item) > 0 || is_battery(item),
        Role::Extra if k == MekKind::Purifier => oxygen_of(item) > 0,
        Role::Extra => chem_of(item).is_some(),
        Role::Fuel => item == I_LAVA_BUCKET || fuel_time(item) > 0,
        Role::Fluid => item == I_WATER_BUCKET,
    }
}

impl Container for MekBe {
    fn size(&self) -> usize {
        self.nslots()
    }
    fn get(&self, i: usize) -> ItemStack {
        self.slots.get(i).copied().unwrap_or_default()
    }
    fn set(&mut self, i: usize, s: ItemStack) {
        if i < self.slots.len() {
            self.slots[i] = if s.is_empty() { ItemStack::EMPTY } else { s };
        }
    }
}
