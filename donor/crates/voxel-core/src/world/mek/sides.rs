//! Стороны машин и выталкивание (MEK-09 §5–6): режимы граней для предметов — относительно лица (повернул машину —
//! настройка повернулась вместе с ней), выталкивание предметов раз в 11 тиков в соседние хранилища (сундук, печь,
//! машина через её грань «вход»). Энергия по сторонам — постоянная таблица вида (energy_io; настройка энергии — позже).
//! Конфигуратор (game/interact.rs) правит режим грани, поворачивает машину, высыпает слоты, снимает ключом.

use super::{MekBe, Role};
use crate::cell::{F_DOWN, F_EAST, F_NORTH, F_SOUTH, F_UP, F_WEST};
use crate::registry::blocks::MekKind;

/// Режимы граней для предметов (DataType): ничего, вход, выход, вход-выход, энергия, доп.
pub(crate) const S_NONE: u8 = 0;
pub(crate) const S_IN: u8 = 1;
pub(crate) const S_OUT: u8 = 2;
pub(crate) const S_IO: u8 = 3;
pub(crate) const S_ENERGY: u8 = 4;
pub(crate) const S_EXTRA: u8 = 5;

/// Относительные грани (RelativeSide): лицо, лево, право, зад, верх, низ.
pub(crate) const R_FRONT: usize = 0;
pub(crate) const R_LEFT: usize = 1;
pub(crate) const R_RIGHT: usize = 2;
pub(crate) const R_BACK: usize = 3;
pub(crate) const R_TOP: usize = 4;
pub(crate) const R_BOTTOM: usize = 5;

/// Мировая грань относительной rel у машины с лицом facing (MEK-09 §5.2; лево и право — для игрока, смотрящего в лицо).
pub(crate) fn dir_of(facing: u8, rel: usize) -> u8 {
    let t: [u8; 6] = match facing {
        F_NORTH => [F_NORTH, F_EAST, F_WEST, F_SOUTH, F_UP, F_DOWN],
        F_SOUTH => [F_SOUTH, F_WEST, F_EAST, F_NORTH, F_UP, F_DOWN],
        F_WEST => [F_WEST, F_NORTH, F_SOUTH, F_EAST, F_UP, F_DOWN],
        F_EAST => [F_EAST, F_SOUTH, F_NORTH, F_WEST, F_UP, F_DOWN],
        F_UP => [F_UP, F_EAST, F_WEST, F_DOWN, F_SOUTH, F_NORTH],
        _ => [F_DOWN, F_EAST, F_WEST, F_UP, F_NORTH, F_SOUTH],
    };
    t[rel.min(5)]
}
/// Относительная грань мировой dir у машины с лицом facing.
pub(crate) fn rel_of(facing: u8, dir: u8) -> usize {
    (0..6).find(|&r| dir_of(facing, r) == dir).unwrap_or(R_FRONT)
}

/// Режимы предметов вида k по кругу (без «ничего»); пусто — у машины нет сторон предметов (генераторы).
pub(crate) fn item_modes(k: MekKind) -> &'static [u8] {
    match k {
        MekKind::Enrich | MekKind::Smelter | MekKind::Crusher => &[S_IN, S_OUT, S_IO, S_ENERGY],
        MekKind::Infuser | MekKind::Purifier => &[S_IN, S_OUT, S_IO, S_ENERGY, S_EXTRA],
        // Сепаратор: без слотов газ-выхода (джетпак — позже) — вход (ведро воды) и энергия.
        MekKind::Separator => &[S_IN, S_ENERGY],
        MekKind::Cube => &[S_IN, S_OUT, S_IO],
        MekKind::Heat | MekKind::Wind | MekKind::Pump => &[],
    }
}
/// Можно ли выталкивать предметы (камеры, дробилка, инфузор, очиститель; куб и сепаратор — нет: у сепаратора нет выхода
/// предметов, его газы выталкиваются всегда — MekBe::separator_tick).
pub(crate) fn can_eject(k: MekKind) -> bool {
    matches!(k, MekKind::Enrich | MekKind::Smelter | MekKind::Infuser | MekKind::Crusher | MekKind::Purifier)
}

/// Умолчания (MEK-09 §5.4): камеры — все вход, право — выход, зад — энергия; инфузор — ещё низ — доп.; куб — все вход,
/// лицо — выход.
pub(crate) fn default_sides(k: MekKind) -> [u8; 6] {
    let mut s = [S_IN; 6];
    match k {
        MekKind::Enrich | MekKind::Smelter | MekKind::Infuser | MekKind::Crusher | MekKind::Purifier => {
            s[R_RIGHT] = S_OUT;
            s[R_BACK] = S_ENERGY;
            if matches!(k, MekKind::Infuser | MekKind::Purifier) {
                s[R_BOTTOM] = S_EXTRA;
            }
        }
        // Сепаратор (MEK-08 §5.4): лицо — вход, зад — энергия; лево и право — выходы газа (их стороны газов — в тике).
        MekKind::Separator => {
            s = [S_NONE; 6];
            s[R_FRONT] = S_IN;
            s[R_BACK] = S_ENERGY;
        }
        MekKind::Cube => s[R_FRONT] = S_OUT,
        MekKind::Heat | MekKind::Wind | MekKind::Pump => s = [S_NONE; 6],
    }
    s
}

/// Стороны газов (M5, MEK-08 §5.4): у сепаратора лево — водород (выход 1), право — кислород (выход 2); у прочих нет.
pub(crate) fn default_gas_sides(k: MekKind) -> [u8; 6] {
    let mut s = [0u8; 6];
    if k == MekKind::Separator {
        s[R_LEFT] = 1;
        s[R_RIGHT] = 2;
    }
    s
}
/// Следующая сторона газа сепаратора по кругу: ничего → выход 1 (водород) → выход 2 (кислород) → ничего.
pub(crate) fn cycle_gas(m: u8) -> u8 {
    (m + 1) % 3
}

/// Следующий (back — предыдущий) режим по кругу «ничего → режимы вида → ничего» (IIncrementalEnum).
pub(crate) fn cycle_mode(k: MekKind, m: u8, back: bool) -> u8 {
    let mut all = vec![S_NONE];
    all.extend_from_slice(item_modes(k));
    if all.len() == 1 {
        return S_NONE;
    }
    let i = all.iter().position(|&x| x == m).unwrap_or(0);
    let n = all.len();
    all[if back { (i + n - 1) % n } else { (i + 1) % n }]
}

/// Роли слотов за режимом m (что за гранью): вход — слоты входа (у куба — «+», зарядка), выход — слоты выхода (у куба —
/// «−»), вход-выход — оба, энергия — слот энергии, доп. — слот вещества.
pub(crate) fn mode_roles(k: MekKind, m: u8) -> &'static [Role] {
    match (k, m) {
        (MekKind::Cube, S_IN) => &[Role::Charge],
        (MekKind::Cube, S_OUT) => &[Role::Discharge],
        (MekKind::Cube, S_IO) => &[Role::Charge, Role::Discharge],
        (MekKind::Separator, S_IN) => &[Role::Fluid],
        (_, S_IN) => &[Role::In],
        (_, S_OUT) => &[Role::Out],
        (_, S_IO) => &[Role::In, Role::Out],
        (_, S_ENERGY) => &[Role::Energy],
        (_, S_EXTRA) => &[Role::Extra],
        _ => &[],
    }
}

/// Имя режима и цвет (EnumColor) — подсказки и подсветка граней конфигуратором.
pub(crate) fn mode_name(m: u8) -> &'static str {
    ["ничего", "вход", "выход", "вход-выход", "энергия", "доп."][m.min(5) as usize]
}
/// Цвет режима 0xRRGGBB (DataType: вход — тёмно-красный, выход — тёмно-синий, вход-выход — пурпурный, энергия — зелёный,
/// доп. — жёлтый; «ничего» — серый).
pub(crate) fn mode_color(m: u8) -> u32 {
    [0x555555, 0xAA0000, 0x0000AA, 0xAA00AA, 0x55FF55, 0xFFFF55][m.min(5) as usize]
}

/// Период выталкивания предметов: после срабатывания счётчик 10, срабатывание на 0 — раз в 11 тиков (§6.2).
pub(crate) const EJECT_DELAY: u8 = 10;

impl MekBe {
    /// Режим предметов на мировой грани dir.
    pub(crate) fn side_at(&self, facing: u8, dir: u8) -> u8 {
        self.sides[rel_of(facing, dir)]
    }
    /// Слоты (номера) за режимом m.
    pub(crate) fn mode_slots(&self, m: u8) -> Vec<usize> {
        let roles = mode_roles(self.kind, m);
        self.def().slots.iter().enumerate().filter(|(_, s)| roles.contains(&s.0)).map(|(i, _)| i).collect()
    }
}

/// Мировые грани в порядке выталкивания (EnumSet<Direction>): низ, верх, север, юг, запад, восток.
pub(crate) const EJECT_ORDER: [u8; 6] = [F_DOWN, F_UP, F_NORTH, F_SOUTH, F_WEST, F_EAST];

/// Упаковка сторон для клиента (Msg::MekSides): 6 режимов по 4 бита (относительные грани) | выталкивание << 24.
pub(crate) fn pack_sides(m: &MekBe) -> u32 {
    let mut v = 0u32;
    for (r, &s) in m.sides.iter().enumerate() {
        v |= (s as u32 & 15) << (4 * r);
    }
    v | (m.eject as u32) << 24
}
/// Распаковка у клиента — в зеркало сущности.
pub(crate) fn unpack_sides(m: &mut MekBe, v: u32) {
    for r in 0..6 {
        m.sides[r] = ((v >> (4 * r)) & 15) as u8;
    }
    m.eject = v >> 24 & 1 != 0;
}
