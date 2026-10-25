//! Перекрёсток (mp/spawn.md): общая площадь появления у (0, 0). Генератор строит её сам — одинаково на сервере и у
//! клиентов (WASM): место ищется по сиду (find), рельеф под площадью выравнивается в Gen::column (shape), блоки
//! кладутся в область куска после деревьев (stamp), дальние плитки видят площадь и Маяк (lod). Защита — цилиндр
//! R_ZONE по всей высоте (protects): её спрашивают и сервер (Auth), и клиент (предсказание отказа).
//!
//! План (север — −Z, восток — +X; r — расстояние от центра Маяка, блоки):
//!   0–3     Маяк 7×7: стены — камень, углы — еловые брёвна, пояса песчаника через 6, окна, арки колокола, смотровая
//!           площадка +26, корона, крыша +31; вокруг — паркур винтом над бассейном (+1…+26)
//!   4–7     бассейн глубиной 2 со ступенью 1 у бортика (вода — источники, не течёт: в зоне ничего не тикает),
//!           4 водопада с водосливов +10,
//!           мостики по осям
//!   8       бортик
//!   9–18    площадь: роза ветров (лучи сторон света — песчаник, север — железо, диагонали — булыжник), отметки часов
//!   18      фонари на концах диагоналей розы
//!   19      кольцо
//!   20–28   сады на клумбах +1 (ели, берёзы, дубы и луг, перголы со скамьями, Врата странствий), дороги по осям,
//!           изгородь; витрина Mekanism (G13) — галерея у западной дороги: три линии бок о бок (ветер, сталь,
//!           удвоение) лицом к площади, дорога проходит под общей крышей
//!   28      ворота на осях, факелы на навершиях
//!   29–31   кольцевая дорожка — видимая граница зоны, столбы с факелами через 30°
//!   34–58   терраса: переход к рельефу ступенями
//!
//! Павильон витрины (G13) — генератор кладёт пол, столбы, заднюю стену и крышу; машины, кабели и сундук ставит сервер
//! при старте (game/showcase.rs): это блок-сущности, генератор их не ставит. Рельеф (shape) G13 не меняет — правки
//! игроков за зоной стоят на прежней земле, сброс мира не нужен; внутри зоны правок игроков нет.

use super::noise::{hash, Rng};
use super::region::Region;
use super::{birch, oak, spruce, Biome, Column, Gen};
use crate::cell::{BlockId, F_EAST, F_NORTH, F_SOUTH, F_WEST, H, SEA};
use crate::registry::ids::*;
use crate::registry::items::{ItemId, I_COAL, I_IRON_INGOT, I_RAW_IRON, I_STEEL_INGOT};

/// Радиус защиты (цилиндр по всей высоте мира).
pub(crate) const R_ZONE: i32 = 32;
/// Радиус ровной площадки и конца перехода к рельефу.
const R_FLAT: f32 = 34.0;
const R_BLEND: f32 = 58.0;
/// Ближе — деревьев генератора нет (крона дерева на 46 — не ближе 40).
const R_NO_TREES: i32 = 46;
/// Внутри — блоки площади (над полом всё убрано); дальше — естественная трава.
const R_BUILT: f32 = 31.5;
/// Высота Маяка над полом: смотровая площадка, крыша.
pub(crate) const DECK: i32 = 26;
pub(crate) const ROOF: i32 = 31;
/// Паркур: площадок (подъём на 1 каждая, последняя — вровень со смотровой), радиус винта, шаг по дуге.
pub(crate) const PARKOUR_N: i32 = DECK;
const PARKOUR_R: f32 = 5.5;
const PARKOUR_STEP: f32 = 2.6;
/// Водопады: клетки (dx, dz) под водосливами на высоте +10 — по одному на грань со сдвигом «вертушкой».
const FALLS: [(i32, i32); 4] = [(4, -2), (2, 4), (-4, 2), (-2, -4)];
const SPOUT: i32 = 10;
/// Врата странствий: середина проёма от центра (северо-восточный сад); проём 5 × 7 вдоль X.
const GATE_DX: i32 = 17;
const GATE_DZ: i32 = -17;

/// Линия витрины Mekanism (G13).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hall {
    /// Ветряк → кабель → обогатительная камера → плавильня → сундук (руда в слитки).
    Wind,
    /// Тепловой генератор (уголь) → кабель → инфузор → инфузор → плавильня → сундук (сталь).
    Steel,
    /// Сундук → транспортёры → очиститель; куб энергии → кабель → сепаратор → очиститель → дробилка → камера →
    /// плавильня → сундук (удвоение M4, вход — логистикой M5).
    Double,
}

impl Hall {
    /// Первая клетка ряда (u): у ветра мачта в −5, у удвоения сундук сырья в −7, у стали генератор в −4.
    pub(crate) const fn u_min(self) -> i32 {
        match self {
            Hall::Wind => -5,
            Hall::Steel => -4,
            Hall::Double => -7,
        }
    }
}

/// Павильон витрины: центр (dx, dz) от центра площади и сторона, куда смотрят лица машин (к зрителю). Локальные
/// клетки: u — вдоль ряда машин (вправо от лица, туда толкают вещи), v — к зрителю. Пол u_min…3, v −2…2: в u −4, v −1 —
/// источник энергии (ветряк — в u −5, над ним открыто небо), кабель v −1, машины v 0, задняя стена v −2 (u −2…2) с
/// доской, столбы по углам u ±3, v ±2, крыша +5 над u −3…3; у удвоения перед машинами (v +1) — транспортёры от сундука.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pavilion {
    pub(crate) hall: Hall,
    pub(crate) dx: i32,
    pub(crate) dz: i32,
    pub(crate) face: u8,
}

const fn pav(hall: Hall, dx: i32, dz: i32, face: u8) -> Pavilion {
    Pavilion { hall, dx, dz, face }
}

/// Галерея витрины (решение пользователя: «рядом друг с другом»): три линии в ряд вдоль западного края площади (машины
/// на x −19, задняя стена на −21), лицом на восток, к Маяку; с юга на север — удвоение (z 3…13), западная дорога под
/// общей крышей (BRIDGE), сталь (z −9…−2), ветер (z −19…−11).
pub(crate) const PAVILIONS: [Pavilion; 3] =
    [pav(Hall::Double, -19, 6, F_EAST), pav(Hall::Steel, -19, -6, F_EAST), pav(Hall::Wind, -19, -16, F_EAST)];
/// Крыша над западной дорогой между сталью и удвоением: (dz от, dz до) при x галереи.
const BRIDGE: (i32, i32) = (-2, 2);

/// Единичный вектор (x, z) стороны света.
pub(crate) const fn face_vec(f: u8) -> (i32, i32) {
    match f {
        F_NORTH => (0, -1),
        F_SOUTH => (0, 1),
        F_WEST => (-1, 0),
        _ => (1, 0),
    }
}

impl Pavilion {
    /// Смещение клетки (u, v) от центра площади.
    pub(crate) const fn off(&self, u: i32, v: i32) -> (i32, i32) {
        let (fx, fz) = face_vec(self.face);
        let (rx, rz) = (fz, -fx); // «право» машины (mek/sides.rs dir_of: лицо на юг — право на восток)
        (self.dx + u * rx + v * fx, self.dz + u * rz + v * fz)
    }
    /// Сторона света «права» машин — туда идут вещи и смотрит лицо источника энергии.
    pub(crate) const fn right(&self) -> u8 {
        match self.face {
            F_SOUTH => F_EAST,
            F_NORTH => F_WEST,
            F_WEST => F_SOUTH,
            _ => F_NORTH,
        }
    }
    /// Прямоугольник пола (dx0, dz0, dx1, dz1) с запасом m.
    pub(crate) fn bbox(&self, m: i32) -> (i32, i32, i32, i32) {
        let (a, b) = (self.off(self.hall.u_min(), -2), self.off(3, 2));
        (a.0.min(b.0) - m, a.1.min(b.1) - m, a.0.max(b.0) + m, a.1.max(b.1) + m)
    }
    fn covers(&self, dx: i32, dz: i32, m: i32) -> bool {
        let (x0, z0, x1, z1) = self.bbox(m);
        (x0..=x1).contains(&dx) && (z0..=z1).contains(&dz)
    }
}

/// Клетка (dx, dz) под павильоном витрины или рядом (запас m).
fn near_pavilion(dx: i32, dz: i32, m: i32) -> bool {
    PAVILIONS.iter().any(|p| p.covers(dx, dz, m))
}
/// Клетка рядом с Вратами странствий (рама и проём с запасом m).
fn near_gate(dx: i32, dz: i32, m: i32) -> bool {
    (dx - GATE_DX).abs() <= 3 + m && (dz - GATE_DZ).abs() <= m
}

/// Доска (render/boards.js): середина (мировые координаты), сторона, куда смотрит лицевая грань ('N', 'S', 'E', 'W'),
/// размер в блоках и что на ней (G15: иконки предметов вместо простыни текста).
pub(crate) struct Board {
    pub(crate) at: [f32; 3],
    pub(crate) side: char,
    pub(crate) w: f32,
    pub(crate) h: f32,
    pub(crate) sheet: Sheet,
}

/// Содержимое доски; рисует render/boards.js иконками инвентаря.
pub(crate) enum Sheet {
    /// «Что это»: заголовок, цепочка линии (предмет или машина и число штук; между ними — стрелки), энергия и помощники
    /// (иконка и подпись), одна строка «зачем».
    Show { title: &'static str, chain: &'static [(ItemId, u8)], extra: &'static [(ItemId, &'static str)], why: &'static str },
    /// Крафт: рецепты верстака (имя рецепта game/crafting.rs, имя машины над ним) — сетка 3 × 3 → результат.
    Craft { recipes: &'static [(&'static str, &'static str)] },
    /// Кнопка неба на грани блока (sky_button): день (true) или ночь.
    Sky(bool),
    /// Подпись над кнопками неба.
    SkySign,
}

/// Стенд неба (G15): три клетки в ряд лицом на юг (к точке появления) — x от центра SKY_X…SKY_X + 2, z SKY_Z; на высоте
/// пола + 2 — кнопка дня (запад), бревно, кнопка ночи (восток), под ними доски, над ними — доски с подписью.
pub(crate) const SKY_X: i32 = -12;
pub(crate) const SKY_Z: i32 = 3;
/// Время, которое кнопки показывают нажавшему (доля суток от полуночи): день — утро ближе к полудню, ночь — полночь.
pub(crate) const SKY_DAY: f64 = 0.45;
pub(crate) const SKY_NIGHT: f64 = 0.0;

// Только в cargo test: площадь строится, если тест её попросил (site_on). Старые проверки механик стоят у (0, 0) на
// высоте 100 — там теперь Маяк и защита; им площадь не нужна. Сборки (WASM, сервер, бот) строят её всегда.
#[cfg(test)]
thread_local! {
    pub(crate) static SITE_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// cargo test: строить Перекрёсток в мирах, созданных дальше в этом потоке.
#[cfg(test)]
pub(crate) fn site_on() {
    SITE_ON.with(|c| c.set(true));
}

pub(crate) struct SpawnSite {
    /// Площадь есть (в сборках — всегда; в cargo test — по site_on).
    active: bool,
    /// Центр Маяка (клетка) и высота пола площади (верхний блок).
    pub(crate) x: i32,
    pub(crate) z: i32,
    pub(crate) y0: i32,
    /// Площадки паркура (dx, высота над полом, dz) по порядку.
    parkour: Vec<(i32, i32, i32)>,
}

impl SpawnSite {
    /// Место по сиду (spawn.md §2): спираль с шагом 16 от (0, 0) до 512 — первое, где суша без рек и озёр в радиусе 58,
    /// не пики, разброс высот в радиусе 24 не больше 12, медиана в [SEA + 4, 88]. Не нашлось — у (0, 0).
    /// raw — столбец генератора до выравнивания.
    pub(crate) fn find(g: &Gen) -> SpawnSite {
        #[cfg(test)]
        if !SITE_ON.with(|c| c.get()) {
            return SpawnSite::placeholder();
        }
        for ring in 0..=32i32 {
            for dz in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs().max(dz.abs()) != ring {
                        continue;
                    }
                    if let Some(y0) = Self::fits(g, dx * 16, dz * 16) {
                        return SpawnSite::new(dx * 16, dz * 16, y0);
                    }
                }
            }
        }
        let h = g.column_raw(0, 0).h;
        SpawnSite::new(0, 0, h.clamp(SEA + 4, 88))
    }

    fn fits(g: &Gen, x: i32, z: i32) -> Option<i32> {
        let mut hs = Vec::with_capacity(64);
        // Кольца 0, 8, 16, 24 — для высоты; 40, 58 — только «не вода».
        for (r, n) in [(0, 1), (8, 8), (16, 12), (24, 16), (40, 20), (58, 24)] {
            for k in 0..n {
                let a = k as f32 * std::f32::consts::TAU / n as f32;
                let (px, pz) = (x + (r as f32 * libm::cosf(a)).round() as i32, z + (r as f32 * libm::sinf(a)).round() as i32);
                let c = g.column_raw(px, pz);
                if c.h <= SEA + 1 || matches!(c.biome, Biome::Ocean | Biome::River | Biome::Beach | Biome::Peaks) {
                    return None;
                }
                if r <= 24 {
                    hs.push(c.h);
                }
            }
        }
        let (lo, hi) = (*hs.iter().min()?, *hs.iter().max()?);
        hs.sort_unstable();
        let med = hs[hs.len() / 2];
        (hi - lo <= 12 && (SEA + 4..=88).contains(&med)).then_some(med)
    }

    /// Заглушка на время поиска места (Gen::new): поиск смотрит только column_raw.
    pub(crate) fn placeholder() -> SpawnSite {
        SpawnSite { active: false, x: 0, z: 0, y0: SEA, parkour: Vec::new() }
    }

    fn new(x: i32, z: i32, y0: i32) -> SpawnSite {
        // Винт паркура: от восточного мостика по часовой (если смотреть сверху, север вверху), площадка — клетка под
        // точкой винта. Первые две — мимо мостиков (над мостиком площадка ниже +3 мешала бы пройти).
        let mut parkour = Vec::with_capacity(PARKOUR_N as usize);
        let a0 = 22f32.to_radians();
        let da = PARKOUR_STEP / PARKOUR_R;
        for k in 1..=PARKOUR_N {
            let a = a0 + (k - 1) as f32 * da;
            let (dx, dz) = ((PARKOUR_R * libm::cosf(a)).round() as i32, (PARKOUR_R * libm::sinf(a)).round() as i32);
            parkour.push((dx, k, dz));
        }
        SpawnSite { active: true, x, z, y0, parkour }
    }

    /// Площадь есть (в cargo test — только по site_on).
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    /// Защищена ли клетка (x, z): цилиндр радиуса R_ZONE вокруг центра, по всей высоте.
    #[inline]
    pub(crate) fn protects(&self, x: i32, z: i32) -> bool {
        let (dx, dz) = ((x - self.x) as i64, (z - self.z) as i64);
        self.active && dx * dx + dz * dz <= (R_ZONE * R_ZONE) as i64
    }

    /// Деревьев генератора здесь нет.
    #[inline]
    pub(crate) fn no_trees(&self, x: i32, z: i32) -> bool {
        let (dx, dz) = (x - self.x, z - self.z);
        self.active && dx * dx + dz * dz < R_NO_TREES * R_NO_TREES
    }

    /// Кнопка неба в клетке p (стенд SKY_X, SKY_Z): Some(true) — день, Some(false) — ночь.
    pub(crate) fn sky_button(&self, p: [i32; 3]) -> Option<bool> {
        if !self.active || p[1] != self.y0 + 2 || p[2] != self.z + SKY_Z {
            return None;
        }
        match p[0] - self.x - SKY_X {
            0 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// Стенд неба: доски внизу, кнопки (белая шерсть — день, чёрная — ночь) по сторонам бревна, доски с подписью сверху,
    /// факел.
    fn sky_stand(&self, reg: &mut Region) {
        let (x0, z, y0) = (self.x + SKY_X, self.z + SKY_Z, self.y0);
        for i in 0..3 {
            set(reg, x0 + i, y0 + 1, z, SPRUCE_PLANKS);
            set(reg, x0 + i, y0 + 2, z, [WHITE_WOOL, SPRUCE_LOG, BLACK_WOOL][i as usize]);
            set(reg, x0 + i, y0 + 3, z, SPRUCE_PLANKS);
        }
        set(reg, x0 + 1, y0 + 4, z, TORCH);
    }

    /// Клетка ступней в проёме Врат странствий (5 × 7 клеток в ряд).
    pub(crate) fn in_gate(&self, p: [i32; 3]) -> bool {
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        self.active && p[2] == gz && (p[0] - gx).abs() <= 2 && p[1] > self.y0 && p[1] <= self.y0 + 7
    }

    /// Проём Врат странствий: [x0, y0, z0, x1, y1, z1] (концы — граница, не клетка), для вуали (render/beam.js).
    pub(crate) fn gate_box(&self) -> [i32; 6] {
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        [gx - 2, self.y0 + 1, gz, gx + 3, self.y0 + 8, gz + 1]
    }

    /// Площадки паркура: (dx, высота над полом, dz).
    pub(crate) fn parkour(&self) -> &[(i32, i32, i32)] {
        &self.parkour
    }

    /// Выравнивание рельефа (в Gen::column): до R_FLAT — ровно y0, до R_BLEND — плавно к естественной высоте с
    /// округлением до целых (склон полками). Одно сравнение квадрата расстояния для остального мира.
    #[inline]
    pub(crate) fn shape(&self, x: i32, z: i32, c: &mut Column) {
        let (dx, dz) = ((x - self.x) as f32, (z - self.z) as f32);
        let d2 = dx * dx + dz * dz;
        if d2 >= R_BLEND * R_BLEND || !self.active {
            return;
        }
        let d = d2.sqrt();
        let t = super::sm((d - R_FLAT) / (R_BLEND - R_FLAT));
        c.h = (self.y0 as f32 + (c.h - self.y0) as f32 * t).round() as i32;
    }

    /// Задевает ли площадь область.
    pub(crate) fn touches(&self, reg: &Region) -> bool {
        self.active && reg.touches(self.x, self.z, R_FLAT as i32 + 1)
    }

    /// Блоки площади в области (после рельефа, руд, деревьев и травы генератора).
    pub(crate) fn stamp(&self, seed: u32, reg: &mut Region) {
        let (cx, cz, y0) = (self.x, self.z, self.y0);
        let (ax, bx, az, bz) = reg.span(cx, cz, -(R_FLAT as i32), R_FLAT as i32);
        for dz in az..=bz {
            for dx in ax..=bx {
                let r = ((dx * dx + dz * dz) as f32).sqrt();
                if r > R_BUILT {
                    continue;
                }
                let (x, z) = (cx + dx, cz + dz);
                for y in y0 + 1..H {
                    set(reg, x, y, z, AIR);
                }
                let floor = self.floor(dx, dz, r);
                set(reg, x, y0, z, floor);
                let bed = is_bed(dx, dz, r);
                if bed {
                    // Клумба сада на блок выше дорожек (перепад высоты; войти — прыжком, как на кочку в лесу).
                    set(reg, x, y0, z, DIRT);
                    set(reg, x, y0 + 1, z, GRASS);
                }
                if floor == WATER {
                    // Бассейн: середина глубиной 2, у бортика — ступень глубиной 1 (плавания нет: выбраться можно прыжком).
                    if r <= 6.5 {
                        set(reg, x, y0 - 1, z, WATER);
                        set(reg, x, y0 - 2, z, SAND);
                    } else {
                        set(reg, x, y0 - 1, z, SAND);
                    }
                }
                if bed {
                    // Цветы и трава садов: луг на юго-востоке гуще.
                    let roll = hash(seed, 0x5A17, x, z) as f32 / 4294967296.0;
                    let meadow = dx > 0 && dz > 0;
                    let fl = if meadow { 0.35 } else { 0.05 };
                    let plant = if roll < fl {
                        if hash(seed, 0x5A18, x.div_euclid(3), z.div_euclid(3)) & 1 == 0 { DANDELION } else { POPPY }
                    } else if roll < fl + 0.25 {
                        if dx < 0 && dz < 0 { FERN } else { TALL_GRASS }
                    } else {
                        AIR
                    };
                    set(reg, x, y0 + 2, z, plant);
                }
            }
        }
        self.hedges(reg);
        self.trees(seed, reg);
        self.pergolas(reg);
        for p in &PAVILIONS {
            self.pavilion(reg, p);
        }
        self.bridge(reg);
        self.gates(reg);
        self.posts(reg);
        self.lamps(reg);
        self.sky_stand(reg);
        self.tower(reg);
        self.parkour_blocks(reg);
    }

    /// Павильон витрины (см. Pavilion): пол, столбы, задняя стена, крыша, факелы по углам крыши; над полом до неба —
    /// воздух (кроны садов не закрывают ветряку небо). Машины ставит сервер (game/showcase.rs).
    fn pavilion(&self, reg: &mut Region, p: &Pavilion) {
        let y0 = self.y0;
        for v in -2..=2 {
            for u in p.hall.u_min()..=3 {
                let (dx, dz) = p.off(u, v);
                let (x, z) = (self.x + dx, self.z + dz);
                for y in y0 + 1..H {
                    set(reg, x, y, z, AIR);
                }
                set(reg, x, y0 - 1, z, STONE);
                set(reg, x, y0, z, if u <= -4 { COBBLE } else { SPRUCE_PLANKS });
                let pillar = u.abs() == 3 && v.abs() == 2;
                if pillar {
                    for k in 1..=4 {
                        set(reg, x, y0 + k, z, SPRUCE_LOG);
                    }
                    set(reg, x, y0 + 6, z, TORCH);
                } else if v == -2 && u.abs() <= 2 {
                    for k in 1..=4 {
                        set(reg, x, y0 + k, z, if k == 1 { STONE } else { SPRUCE_PLANKS });
                    }
                }
                if u >= -3 {
                    let edge = u.abs() == 3 || v.abs() == 2;
                    set(reg, x, y0 + 5, z, if edge { SPRUCE_LOG } else { SPRUCE_PLANKS });
                }
            }
        }
    }

    /// Общая крыша галереи над западной дорогой (дорога и ворота проходят под ней, пол — дорога как была).
    fn bridge(&self, reg: &mut Region) {
        let p = &PAVILIONS[0];
        for dz in BRIDGE.0..=BRIDGE.1 {
            for v in -2..=2 {
                let (dx, _) = p.off(0, v);
                let (x, z) = (self.x + dx, self.z + dz);
                for y in self.y0 + 1..self.y0 + 5 {
                    set(reg, x, y, z, AIR);
                }
                set(reg, x, self.y0 + 5, z, if v.abs() == 2 { SPRUCE_LOG } else { SPRUCE_PLANKS });
            }
        }
    }

    /// Перголы со скамьёй в садах (по диагонали кварталов, где нет павильона и Врат): площадка-клумба +1 из камня,
    /// четыре столба, крыша из листвы, скамья из еловых досок спиной к ограде, факел над крышей.
    fn pergolas(&self, reg: &mut Region) {
        let y = self.y0 + 1;
        for (cx, cz) in [(-16, 16), (16, 16), (-16, -16), (16, -16)] {
            if near_pavilion(cx, cz, 4) || near_gate(cx, cz, 4) {
                continue;
            }
            let back = if cz > 0 { 1 } else { -1 }; // скамья — у внешней стороны, лицом к площади
            for dz in -1..=1 {
                for dx in -2..=2 {
                    let (x, z) = (self.x + cx + dx, self.z + cz + dz);
                    for k in 1..=6 {
                        set(reg, x, y + k, z, AIR);
                    }
                    set(reg, x, y, z, STONE);
                    if dx.abs() == 2 && dz.abs() == 1 {
                        for k in 1..=3 {
                            set(reg, x, y + k, z, SPRUCE_LOG);
                        }
                    } else if dz == back && dx.abs() <= 1 {
                        set(reg, x, y + 1, z, SPRUCE_PLANKS);
                    }
                    set(reg, x, y + 4, z, LEAVES);
                }
            }
            set(reg, self.x + cx, y + 5, self.z + cz, TORCH);
        }
    }

    /// Фонари площади: столбы на концах диагоналей розы ветров (r ≈ 18) — бревно, бревно, факел.
    fn lamps(&self, reg: &mut Region) {
        for (sx, sz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let (x, z) = (self.x + 13 * sx, self.z + 13 * sz);
            set(reg, x, self.y0 + 1, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 2, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 3, z, TORCH);
        }
    }

    /// Доски витрины (render/boards.js): на каждом павильоне — спереди «что это», сзади — «из чего крафтится».
    pub(crate) fn boards(&self) -> Vec<Board> {
        let mut out = Vec::new();
        if !self.active {
            return out;
        }
        for p in &PAVILIONS {
            let (dx, dz) = p.off(0, -2);
            let (fx, fz) = face_vec(p.face);
            let (cx, cz) = ((self.x + dx) as f32 + 0.5, (self.z + dz) as f32 + 0.5);
            let d = 0.5 + 0.04;
            let y = self.y0 as f32 + 3.5;
            let side = |f: u8| match f {
                F_NORTH => 'N',
                F_SOUTH => 'S',
                F_WEST => 'W',
                _ => 'E',
            };
            let back = match p.face {
                F_NORTH => F_SOUTH,
                F_SOUTH => F_NORTH,
                F_WEST => F_EAST,
                _ => F_WEST,
            };
            let (front, craft) = hall_sheets(p.hall);
            out.push(Board { at: [cx + fx as f32 * d, y, cz + fz as f32 * d], side: side(p.face), w: 5.0, h: 3.0, sheet: front });
            out.push(Board { at: [cx - fx as f32 * d, y, cz - fz as f32 * d], side: side(back), w: 5.0, h: 3.0, sheet: craft });
        }
        // Стенд неба: кнопки — накладки чуть меньше грани (видна шерсть по краю), подпись — на досках сверху.
        let (x0, zf, y0) = ((self.x + SKY_X) as f32, (self.z + SKY_Z) as f32 + 1.04, self.y0 as f32);
        out.push(Board { at: [x0 + 0.5, y0 + 2.5, zf], side: 'S', w: 0.875, h: 0.875, sheet: Sheet::Sky(true) });
        out.push(Board { at: [x0 + 2.5, y0 + 2.5, zf], side: 'S', w: 0.875, h: 0.875, sheet: Sheet::Sky(false) });
        out.push(Board { at: [x0 + 1.5, y0 + 3.5, zf], side: 'S', w: 3.0, h: 1.0, sheet: Sheet::SkySign });
        out
    }

    /// Пол клетки (верхний блок на y0).
    fn floor(&self, dx: i32, dz: i32, r: f32) -> BlockId {
        let (ax, az) = (dx.abs(), dz.abs());
        let axis = ax <= 1 || az <= 1; // луч или дорога по оси шириной 3
        if ax <= 3 && az <= 3 {
            return STONE; // пол Маяка
        }
        if r <= 8.5 && axis {
            return PLANKS; // мостики
        }
        if r <= 7.5 {
            return WATER;
        }
        if r <= 8.5 {
            return COBBLE; // бортик
        }
        if r <= 18.5 {
            // Роза ветров: север — острие из железа, стороны света — песчаник, диагонали — булыжник; отметки часов.
            if dz < 0 && ax <= 1 && r > 14.5 {
                return IRON_BLOCK;
            }
            if axis {
                return SANDSTONE;
            }
            if ax == az {
                return COBBLE;
            }
            if sundial_mark(dx, dz, r) {
                return COAL_BLOCK;
            }
            return STONE;
        }
        if r <= 19.5 {
            return COBBLE;
        }
        if r <= 28.5 {
            if axis {
                return SANDSTONE; // дороги
            }
            return GRASS;
        }
        if r <= 29.5 || r > 30.5 {
            return COBBLE; // бордюры дорожки
        }
        GRAVEL
    }

    fn hedges(&self, reg: &mut Region) {
        // Изгородь листвой вдоль дорог (с садовой стороны), с проходом в середине каждого сада.
        for d in 21..=27 {
            if d == 24 {
                continue;
            }
            for (sx, sz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
                for (hx, hz) in [(2 * sx, d * sz), (d * sx, 2 * sz)] {
                    if near_pavilion(hx, hz, 0) {
                        continue;
                    }
                    // Два блока: над клумбой (+1) изгородь видна на ладонь.
                    set(reg, self.x + hx, self.y0 + 1, self.z + hz, LEAVES);
                    set(reg, self.x + hx, self.y0 + 2, self.z + hz, LEAVES);
                }
            }
        }
    }

    fn trees(&self, seed: u32, reg: &mut Region) {
        // Деревья растут из клумб (+1); у павильонов (их ветряку нужно небо) и Врат — нет.
        let (cx, cz, h) = (self.x, self.z, self.y0 + 1);
        let ok = |dx: i32, dz: i32| !near_pavilion(dx, dz, 3) && !near_gate(dx, dz, 3);
        // ГСЧ — у каждого дерева свой (по клетке): соседние области видят одно и то же дерево.
        let rng = |x: i32, z: i32| Rng(hash(seed, 0x7EE5, x, z));
        // Северо-запад — еловая роща, юго-запад — березняк, юго-восток — дубы над лугом; северо-восток — Врата.
        for (dx, dz, t) in [(-9, -22, 9), (-15, -16, 11), (-21, -9, 8), (-22, -16, 7), (-16, -23, 7)] {
            if !ok(dx, dz) {
                continue;
            }
            spruce(reg, cx + dx, h, cz + dz, t);
        }
        for (dx, dz, t) in [(-9, 22, 7), (-15, 17, 8), (-22, 10, 6), (-20, 18, 7)] {
            if !ok(dx, dz) {
                continue;
            }
            birch(reg, cx + dx, h, cz + dz, t, &mut rng(cx + dx, cz + dz));
        }
        for (dx, dz, t) in [(13, 20, 5), (21, 12, 6), (21, -10, 6), (10, -22, 5)] {
            if !ok(dx, dz) {
                continue;
            }
            oak(reg, cx + dx, h, cz + dz, t, &mut rng(cx + dx, cz + dz));
        }
    }

    /// Ворота на осях (r = 28): проём 5 × 7, столбы — еловые брёвна, перемычка — доски, навершие — песчаник.
    /// Врата странствий (северо-восток) — пока рама той же арки, повёрнутая к центру.
    fn gates(&self, reg: &mut Region) {
        for (ux, uz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
            let (px, pz) = (-uz, ux); // поперёк дороги
            for p in -3..=3 {
                let (x, z) = (self.x + ux * 28 + px * p, self.z + uz * 28 + pz * p);
                if p.abs() == 3 {
                    for k in 1..=8 {
                        set(reg, x, self.y0 + k, z, SPRUCE_LOG);
                    }
                    set(reg, x, self.y0 + 9, z, SANDSTONE);
                    set(reg, x, self.y0 + 10, z, TORCH);
                } else {
                    set(reg, x, self.y0 + 8, z, SPRUCE_PLANKS);
                    if p.abs() <= 1 {
                        set(reg, x, self.y0 + 9, z, SANDSTONE);
                    }
                }
            }
        }
        // Врата странствий (северо-восточный сад): рама из угольных блоков, как портал, порог — тоже уголь; к ним от
        // площади — гравийная дорожка.
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        for p in -3..=3 {
            let x = gx + p;
            if p.abs() == 3 {
                for k in 1..=8 {
                    set(reg, x, self.y0 + k, gz, COAL_BLOCK);
                }
                set(reg, x, self.y0 + 9, gz, IRON_BLOCK);
            } else {
                set(reg, x, self.y0 + 8, gz, COAL_BLOCK);
                set(reg, x, self.y0, gz, COAL_BLOCK);
                for k in 1..=7 {
                    set(reg, x, self.y0 + k, gz, AIR);
                }
                if p == 0 {
                    set(reg, x, self.y0 + 9, gz, IRON_BLOCK);
                }
            }
        }
        let step = if GATE_DZ < 0 { 1 } else { -1 }; // от Врат к центру
        let mut z = gz + step;
        while z != self.z {
            for x in gx - 1..=gx + 1 {
                let (dx, dz) = (x - self.x, z - self.z);
                if is_garden(dx, dz, ((dx * dx + dz * dz) as f32).sqrt()) {
                    set(reg, x, self.y0, z, GRAVEL);
                    set(reg, x, self.y0 + 1, z, AIR);
                    set(reg, x, self.y0 + 2, z, AIR);
                }
            }
            z += step;
        }
    }

    /// Столбы на краю зоны через 30° (кроме осей — там дороги).
    fn posts(&self, reg: &mut Region) {
        for k in 0..12 {
            if k % 3 == 0 {
                continue;
            }
            let a = k as f32 * std::f32::consts::TAU / 12.0;
            let (dx, dz) = ((31.0 * libm::cosf(a)).round() as i32, (31.0 * libm::sinf(a)).round() as i32);
            let (x, z) = (self.x + dx, self.z + dz);
            set(reg, x, self.y0 + 1, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 2, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 3, z, TORCH);
        }
    }

    fn tower(&self, reg: &mut Region) {
        let (cx, cz, y0) = (self.x, self.z, self.y0);
        // Сторона, к которой приходит паркур (проём в перилах смотровой).
        let &(lx, _, lz) = self.parkour.last().unwrap();
        for dz in -3..=3i32 {
            for dx in -3..=3i32 {
                let (ax, az) = (dx.abs(), dz.abs());
                let wall = ax == 3 || az == 3;
                let corner = ax == 3 && az == 3;
                let perp = if ax == 3 { az } else { ax };
                let (x, z) = (cx + dx, cz + dz);
                for k in 1..=ROOF + 1 {
                    let b = if k < DECK {
                        if !wall {
                            AIR
                        } else if corner {
                            SPRUCE_LOG
                        } else if k <= 4 && perp <= 1 {
                            AIR // проёмы к мостикам
                        } else if k % 6 == 0 {
                            SANDSTONE // пояса
                        } else if (13..=16).contains(&k) && perp <= 1 {
                            AIR // арки колокола
                        } else if k > 6 && perp == 0 && k % 6 >= 2 && !(dz == 3 && k <= 9) {
                            GLASS // окна-бойницы (на юге над проёмом — доска «Как играть», render/boards.js)
                        } else {
                            STONE
                        }
                    } else if k == DECK {
                        if corner { SPRUCE_LOG } else { SPRUCE_PLANKS }
                    } else if k < ROOF {
                        if corner {
                            SPRUCE_LOG
                        } else if k == DECK + 1 && wall {
                            // Перила; проём к последней площадке паркура.
                            let gap = (dx - lx).abs().max((dz - lz).abs()) <= 2 && perp <= 1;
                            if gap { AIR } else { GLASS }
                        } else {
                            AIR
                        }
                    } else if k == ROOF {
                        if ax <= 1 && az <= 1 { GLASS } else { SANDSTONE }
                    } else if ax <= 1 && az <= 1 {
                        if ax == 0 && az == 0 { IRON_BLOCK } else { SANDSTONE }
                    } else {
                        AIR
                    };
                    set(reg, x, y0 + k, z, b);
                }
            }
        }
        // Водосливы и водопады в бассейн.
        for (dx, dz) in FALLS {
            let (x, z) = (cx + dx, cz + dz);
            set(reg, x, y0 + SPOUT, z, SANDSTONE);
            for k in 1..SPOUT {
                set(reg, x, y0 + k, z, WATER);
            }
        }
    }

    fn parkour_blocks(&self, reg: &mut Region) {
        for (i, &(dx, k, dz)) in self.parkour.iter().enumerate() {
            // Отметки (каждая седьмая и последняя) — песчаник, остальные — берёзовые доски (светлые на фоне камня).
            let b = if (i + 1) % 7 == 0 || i + 1 == self.parkour.len() { SANDSTONE } else { BIRCH_PLANKS };
            set(reg, self.x + dx, self.y0 + k, self.z + dz, b);
        }
    }

    /// Дальняя клетка (lod_mesh): (высота верха, слой верха, слой бока, вода). x, z — середина клетки размером cell.
    /// Маяк — столбцом, пока клетка не больше 8 блоков (дальше его заменит луч); площадь — камень, сады — трава без
    /// полога крон.
    pub(crate) fn lod(&self, x: i32, z: i32, cell: i32) -> Option<(i32, u32, u32, bool)> {
        if !self.active {
            return None;
        }
        let (dx, dz) = (x - self.x, z - self.z);
        let half = cell / 2;
        if cell <= 8 && dx.abs() <= 3 + half && dz.abs() <= 3 + half {
            return Some((self.y0 + ROOF, L_SANDSTONE_TOP, L_STONE, false));
        }
        let d2 = dx * dx + dz * dz;
        if d2 <= 19 * 19 {
            return Some((self.y0, L_STONE, L_STONE, false));
        }
        if d2 <= (R_FLAT as i32) * (R_FLAT as i32) {
            return Some((self.y0, L_GRASS_TOP, L_GRASS_FULL, false));
        }
        None
    }
}

/// Клумба сада (+1): сад без дорожки к Вратам и без газона у павильонов (к ним подходят по ровному).
fn is_bed(dx: i32, dz: i32, r: f32) -> bool {
    is_garden(dx, dz, r) && !near_pavilion(dx, dz, 2) && !((dx - GATE_DX).abs() <= 1 && (dz.signum() == GATE_DZ.signum() && dz.abs() < GATE_DZ.abs()))
}

/// Доски павильона: (лицевая — что это: цепочка машин иконками, задняя — рецепты машин линии).
fn hall_sheets(h: Hall) -> (Sheet, Sheet) {
    const RAW: ItemId = I_RAW_IRON;
    const INGOT: ItemId = I_IRON_INGOT;
    match h {
        Hall::Wind => (
            Sheet::Show {
                title: "Ветер: руда в слитки",
                chain: &[(RAW, 3), (ENRICHMENT_CHAMBER as ItemId, 1), (ENERGIZED_SMELTER as ItemId, 1), (INGOT, 4)],
                extra: &[(WIND_GENERATOR as ItemId, "энергия ветра: выше — больше"), (UNIVERSAL_CABLE as ItemId, "несёт её к машинам")],
                why: "3 сырого железа — 4 слитка",
            },
            Sheet::Craft {
                recipes: &[("wind_generator", "Ветряк"), ("enrichment_chamber", "Камера"), ("energized_smelter", "Плавильня"), ("basic_universal_cable", "Кабель")],
            },
        ),
        Hall::Steel => (
            Sheet::Show {
                title: "Сталь",
                chain: &[(INGOT, 1), (METALLURGIC_INFUSER as ItemId, 1), (METALLURGIC_INFUSER as ItemId, 1), (ENERGIZED_SMELTER as ItemId, 1), (I_STEEL_INGOT, 1)],
                extra: &[(HEAT_GENERATOR as ItemId, "энергия: жжёт уголь"), (I_COAL, "уголь в оба инфузора")],
                why: "Сталь — на корпуса машин и кабели",
            },
            Sheet::Craft {
                recipes: &[("heat_generator", "Генератор"), ("metallurgic_infuser", "Инфузор"), ("steel_casing", "Корпус"), ("energy_tablet", "Планшет")],
            },
        ),
        Hall::Double => (
            Sheet::Show {
                title: "Удвоение руды",
                chain: &[(RAW, 1), (PURIFICATION_CHAMBER as ItemId, 1), (CRUSHER as ItemId, 1), (ENRICHMENT_CHAMBER as ItemId, 1), (ENERGIZED_SMELTER as ItemId, 1), (INGOT, 2)],
                extra: &[(ELECTROLYTIC_SEPARATOR as ItemId, "кислород из воды"), (LOGISTICAL_TRANSPORTER as ItemId, "руда из сундука")],
                why: "Из одной руды — два слитка",
            },
            Sheet::Craft {
                recipes: &[("electrolytic_separator", "Сепаратор"), ("purification_chamber", "Очиститель"), ("crusher", "Дробилка"), ("basic_logistical_transporter", "Транспортёр")],
            },
        ),
    }
}

/// Рецепт верстака по имени (game/crafting.rs path_recipes) для доски: сетка 3 × 3 (узор по середине, тег — первый
/// предмет набора, бесформенный — по порядку), результат и число. Нет такого — None.
pub(crate) fn recipe_cells(name: &str) -> Option<([ItemId; 9], ItemId, i32)> {
    use crate::game::crafting::{path_recipes, Ing, RShape};
    let first = |i: &Ing| match *i {
        Ing::Item(i) => i,
        Ing::Set(v) => v[0],
    };
    let d = path_recipes().into_iter().find(|d| d.name == name)?;
    let mut g = [0 as ItemId; 9];
    match &d.shape {
        RShape::Shaped(rows, key) => {
            let (h, w) = (rows.len(), rows[0].chars().count());
            let (ox, oy) = ((3 - w) / 2, (3 - h) / 2);
            for (y, r) in rows.iter().enumerate() {
                for (x, c) in r.chars().enumerate() {
                    if let Some((_, i)) = key.iter().find(|(k, _)| *k == c) {
                        g[ox + x + (oy + y) * 3] = first(i);
                    }
                }
            }
        }
        RShape::Shapeless(v) => {
            for (k, i) in v.iter().enumerate().take(9) {
                g[k] = first(i);
            }
        }
    }
    Some((g, d.out, d.count))
}

/// Доски одной строкой JSON (render/boards.js): [{at, side, w, h, kind, …}] — kind «show» (title, chain [[id, n]],
/// extra [[id, подпись]], why), «craft» (recipes [{grid [9 id], out, n, cap}]), «sky» (day), «skysign».
/// Тексты — свои, без кавычек и обратных косых (экранировать нечего).
pub(crate) fn boards_json(site: &SpawnSite) -> String {
    let s = |t: &str| format!("\"{t}\"");
    let mut out = Vec::new();
    for b in site.boards() {
        let head = format!("\"at\":[{},{},{}],\"side\":\"{}\",\"w\":{},\"h\":{}", b.at[0], b.at[1], b.at[2], b.side, b.w, b.h);
        let body = match &b.sheet {
            Sheet::Show { title, chain, extra, why } => {
                let chain: Vec<String> = chain.iter().map(|(i, n)| format!("[{i},{n}]")).collect();
                let extra: Vec<String> = extra.iter().map(|(i, t)| format!("[{i},{}]", s(t))).collect();
                format!("\"kind\":\"show\",\"title\":{},\"chain\":[{}],\"extra\":[{}],\"why\":{}", s(title), chain.join(","), extra.join(","), s(why))
            }
            Sheet::Craft { recipes } => {
                let list: Vec<String> = recipes
                    .iter()
                    .filter_map(|(name, cap)| {
                        let (g, o, n) = recipe_cells(name)?;
                        let g: Vec<String> = g.iter().map(|i| i.to_string()).collect();
                        Some(format!("{{\"grid\":[{}],\"out\":{o},\"n\":{n},\"cap\":{}}}", g.join(","), s(cap)))
                    })
                    .collect();
                format!("\"kind\":\"craft\",\"recipes\":[{}]", list.join(","))
            }
            Sheet::Sky(day) => format!("\"kind\":\"sky\",\"day\":{day}"),
            Sheet::SkySign => "\"kind\":\"skysign\"".to_string(),
        };
        out.push(format!("{{{head},{body}}}"));
    }
    format!("[{}]", out.join(","))
}

/// Сад: между дорогами, кольцом 19.5…28.5.
fn is_garden(dx: i32, dz: i32, r: f32) -> bool {
    r > 19.5 && r <= 27.5 && dx.abs() > 2 && dz.abs() > 2
}

/// Отметки солнечных часов: куда падает тень Маяка в 6, 9, 12, 15 и 18 часов — направление −(горизонталь солнца)
/// по пути солнца atmosphere.js (skyState: ось AX_E = norm(0.55, 0, 0.4), AX_U — «вверх» с наклоном к югу; угол
/// 2π·t − π/2, t = час / 24). Углы (0° — восток, 90° — юг): 6 ч −144.0°, 9 ч −132.8°, 12 ч −54.0°, 15 ч 24.8°,
/// 18 ч 36.0°. Отметка — клетки r 13…17 в пределах ±0.75 блока от линии; полдень длиннее (с r 10.5).
fn sundial_mark(dx: i32, dz: i32, r: f32) -> bool {
    if !(10.5..=17.0).contains(&r) {
        return false;
    }
    let a = libm::atan2f(dz as f32, dx as f32).to_degrees();
    [(-144.0f32, 13.0f32), (-132.8, 13.0), (-54.0, 10.5), (24.8, 13.0), (36.0, 13.0)].iter().any(|&(m, r0)| {
        let d = (a - m + 540.0) % 360.0 - 180.0;
        r >= r0 && d.abs().to_radians() * r < 0.75
    })
}

#[inline]
fn set(reg: &mut Region, x: i32, y: i32, z: i32, b: BlockId) {
    if let Some(i) = reg.index_of(x, y, z) {
        reg.blocks[i] = b as u8;
    }
}

