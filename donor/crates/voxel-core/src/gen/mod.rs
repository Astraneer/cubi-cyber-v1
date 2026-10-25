//! Генерация мира: столбцы, биомы, заполнение области (Region — region.rs), деревья, руды, вид дальних клеток.
//!
//! Рельеф — по образцу Minecraft 1.18: континентальность (суша и океаны), эрозия (горы ↔ равнины), гребни (пики и
//! долины), реки, редкие озёра, болота. Высота — сплайн континентальности + холмы + горы, местами террасами
//! (полки и уступы — и красиво, и меньше граней). Климат — крупный шум; варианты биомов (луг, редколесье, старый
//! лес, болото, каменистый берег) — без новых блоков и без новых номеров биомов: формой и растительностью.

pub(crate) mod noise;
pub(crate) mod region;
pub(crate) mod spawn;

use crate::cell::{BlockId, CS, H, SEA};
use crate::registry::blocks::is_grass;
use crate::registry::ids::*;
use noise::{hash, lerp, sm, Noise, Rng};
pub(crate) use region::{Region, PAD, W};

/// Версия генератора (design.md §2.7): мир — сид плюс правки поверх генератора, поэтому живой мир держится за свой
/// генератор. Любая правка, меняющая блоки мира при том же сиде (рельеф, биомы, руды, деревья, растения), — +1:
/// сохранение с другой версией не загрузится (новый генератор = новый мир, решение пользователя).
/// 2 — MP-0: sin/cos жил руд и ветвей дуба — libm (одни и те же биты в WASM и нативно), эталоны — probe_reference.
/// 3 — Mekanism M1: руды золота, редстоуна, меди (ваниль), осмия и олова (MEK-05), подземные лавовые озёра — новый мир.
/// 4 — G1: Перекрёсток у (0, 0) (gen/spawn.rs): терраса, площадь, Маяк; деревьев ближе 46 от центра нет.
pub(crate) const GEN_VERSION: u32 = 4;

/// Проба генератора при входе (design.md §2.7): клиент шлёт хеши этих кусков (Hello.probe), сервер сверяет со своими —
/// расхождение реализаций (libm, порядок HashMap) видно в первый же день. Сид пробы постоянный: важна реализация, а не
/// мир. Кусок появления и три дальних (руды, деревья, горы и вода).
pub(crate) const PROBE_SEED: u32 = 1337;
pub(crate) const PROBE_CHUNKS: [(i32, i32); 4] = [(0, 0), (7, -3), (-21, 12), (55, 38)];

/// Хеши кусков пробы (FNV-1a по номерам блоков и оттенкам области с рамкой).
pub(crate) fn probe() -> [u64; 4] {
    let g = Gen::new(PROBE_SEED);
    PROBE_CHUNKS.map(|(cx, cz)| region_hash(&generate(&g, cx, cz)))
}

/// Отпечаток области генератора: номера блоков (оба байта) и оттенки столбцов.
pub(crate) fn region_hash(r: &Region) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |b: u8| h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    r.blocks.iter().for_each(|&b| eat(b));
    if let Some(hi) = &r.blocks_hi {
        hi.iter().for_each(|&b| eat(b));
    }
    r.tint.iter().for_each(|&t| {
        eat(t as u8);
        eat((t >> 8) as u8);
    });
    h
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Biome {
    Ocean,
    Beach,
    River,
    Plains,
    Forest,
    Birch,
    Taiga,
    Snowy,
    Savanna,
    Desert,
    Mountain,
    Peaks,
}

/// Вариант биома: другой облик без нового номера биома.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Var {
    None,
    Meadow,    // цветочный луг (равнины)
    Sparse,    // редколесье: поляны, крупные деревья поодиночке
    OldGrowth, // старый лес: крупные деревья
    Swamp,     // болотистая низина: заводи, тёмная трава
    Stony,     // каменистый берег
}

pub(crate) struct Column {
    pub(crate) h: i32,
    pub(crate) biome: Biome,
    pub(crate) var: Var,
    pub(crate) det: f32,
    pub(crate) tt: f32,   // температура с поправкой на высоту
    pub(crate) hum: f32,  // влажность
    pub(crate) vary: f32,  // шум разнообразия (σ ≈ 0.37): поляны, опушки, луга
    pub(crate) swamp: f32, // 0…1 — болотистость (тёмная трава плавно)
}

pub(crate) struct Gen {
    pub(crate) seed: u32,
    pub(crate) cont: Noise,
    pub(crate) ero: Noise,
    pub(crate) ridge: Noise,
    pub(crate) hill: Noise,
    pub(crate) temp: Noise,
    pub(crate) hum: Noise,
    pub(crate) river: Noise,
    pub(crate) warp: Noise,
    pub(crate) det: Noise,
    pub(crate) vary: Noise,
    pub(crate) roff: (f32, f32), // сдвиг поля рек и озёр: подобран по сиду так, чтобы у точки появления их не было
    /// Перекрёсток — общая площадь появления (spawn.rs): место по сиду, выравнивает рельеф и кладёт блоки.
    pub(crate) site: spawn::SpawnSite,
}

/// Основа высоты над морем по континентальности (σ ≈ 0.35): глубокий океан, шельф, берег, равнины вглубь материка.
pub(crate) const SPLINE: [(f32, f32); 12] = [
    (-1.2, -28.0),
    (-0.55, -24.0),
    (-0.35, -17.0),
    (-0.2, -9.0),
    (-0.13, -4.0),
    (-0.1, -1.5),
    (-0.07, 1.5),
    (0.0, 4.5),
    (0.15, 8.0),
    (0.4, 11.0),
    (0.8, 14.0),
    (1.2, 16.0),
];

pub(crate) fn spline(x: f32) -> f32 {
    if x <= SPLINE[0].0 {
        return SPLINE[0].1;
    }
    for i in 1..SPLINE.len() {
        if x <= SPLINE[i].0 {
            let (x0, y0) = SPLINE[i - 1];
            let (x1, y1) = SPLINE[i];
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    SPLINE[SPLINE.len() - 1].1
}

/// Террасы: ровные полки высотой step и крутые уступы (sharp — доля шага, занятая уступом).
pub(crate) fn terrace(v: f32, step: f32, sharp: f32) -> f32 {
    let k = v / step;
    let fl = k.floor();
    let t = ((k - fl - (1.0 - sharp)) / sharp).clamp(0.0, 1.0);
    (fl + t * t * (3.0 - 2.0 * t)) * step
}

impl Gen {
    pub(crate) fn new(seed: u32) -> Self {
        let mut g = Gen {
            seed,
            cont: Noise::new(seed ^ 0x11),
            ero: Noise::new(seed ^ 0x22),
            ridge: Noise::new(seed ^ 0x33),
            hill: Noise::new(seed ^ 0x44),
            temp: Noise::new(seed ^ 0x55),
            hum: Noise::new(seed ^ 0x66),
            river: Noise::new(seed ^ 0x77),
            warp: Noise::new(seed ^ 0x88),
            det: Noise::new(seed ^ 0x99),
            vary: Noise::new(seed ^ 0xAA),
            roff: (0.0, 0.0),
            site: spawn::SpawnSite::placeholder(),
        };
        // Река или озеро прямо в точке появления — некрасиво (и игрок в воде): сдвигаем их поле, пока рядом с (0, 0)
        // не станет суша. Детерминированно по сиду.
        for k in 0..48 {
            g.roff = (k as f32 * 0.37, k as f32 * -0.23);
            let dry = [(0, 0), (64, 0), (-64, 0), (0, 64), (0, -64), (45, 45), (-45, -45), (45, -45), (-45, 45)]
                .iter()
                .all(|&(x, z)| {
                    let (wx, wz) = g.warp(x as f32, z as f32);
                    g.river_t(wx, wz) > 0.035 && g.lake_n(wx, wz) < 0.45
                });
            if dry {
                break;
            }
        }
        g.site = spawn::SpawnSite::find(&g);
        g
    }

    /// Извилистость рек и озёр: сдвиг координат.
    #[inline]
    pub(crate) fn warp(&self, fx: f32, fz: f32) -> (f32, f32) {
        (fx + 70.0 * self.warp.perlin(fx / 280.0, fz / 280.0), fz + 70.0 * self.warp.perlin(fx / 280.0 + 50.0, fz / 280.0))
    }
    /// Расстояние до линии реки в единицах шума (0 — середина русла).
    #[inline]
    pub(crate) fn river_t(&self, wx: f32, wz: f32) -> f32 {
        self.river.fbm(wx / 1150.0 + self.roff.0, wz / 1150.0 + self.roff.1, 2).abs()
    }
    /// Шум озёр (σ ≈ 0.37): котловина — выше 0.55, вода — выше 0.88. Там, где первая октава заведомо не даёт
    /// котловины, вторая не считается (возвращается оценка сверху, она ниже порога).
    #[inline]
    pub(crate) fn lake_n(&self, wx: f32, wz: f32) -> f32 {
        let (x, z) = (wx / 420.0 - 400.0 + self.roff.1, wz / 420.0 + 170.0 + self.roff.0);
        let p1 = self.vary.perlin(x, z);
        if p1 < -0.07 {
            return (p1 + 0.35) / 1.5 * 2.7;
        }
        let p2 = self.vary.perlin(x * 2.03 + 17.13, z * 2.03 - 9.71);
        (p1 + 0.5 * p2) / 1.5 * 2.7
    }

    /// Столбец мира: естественный рельеф, под Перекрёстком — выровненный (его одинаково видят куски, физика, воркеры и
    /// дальние плитки).
    #[inline]
    pub(crate) fn column(&self, x: i32, z: i32) -> Column {
        let mut c = self.column_raw(x, z);
        self.site.shape(x, z, &mut c);
        c
    }

    /// Столбец до выравнивания под Перекрёсток (поиск его места).
    pub(crate) fn column_raw(&self, x: i32, z: i32) -> Column {
        let (fx, fz) = (x as f32, z as f32);
        let s = SEA as f32;
        // Точка появления (0, 0) — на материке; высоких гор у самого центра нет (они видны вдали).
        let d0 = (fx * fx + fz * fz).sqrt();
        let center = sm(1.0 - d0 / 1600.0);
        let near0 = sm(1.0 - d0 / 400.0);
        // Крупные шумы, σ ≈ 0.35: континентальность, эрозия, климат.
        let c = self.cont.fbm(fx / 2400.0, fz / 2400.0, 5) * 3.2 + 0.02 + 0.45 * center;
        let e = self.ero.fbm(fx / 1100.0, fz / 1100.0, 2) * 2.6;
        let temp = self.temp.fbm(fx / 1700.0, fz / 1700.0, 2) * 2.6 * (1.0 - 0.5 * center);
        let hum = self.hum.fbm(fx / 1500.0, fz / 1500.0, 2) * 2.6;
        let det = self.det.perlin(fx / 23.0, fz / 23.0) + 0.5 * self.det.perlin(fx / 9.0, fz / 9.0);

        let mut h = s + spline(c);
        // Дно глубже шельфа — полками по 4 блока: вдали меньше боковых граней, вблизи — уступы под водой.
        if h < s - 5.0 {
            h = s - 5.0 + terrace(h - s + 5.0, 4.0, 0.35);
        }
        let (mut vary, mut mtn, mut cliffy, mut swamp, mut river_k) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut lake = false;
        if c > -0.2 {
            vary = self.vary.perlin(fx / 260.0, fz / 260.0) * 2.0;
            // Берег: при низкой эрозии рельеф обрывается к морю круто (скалы), иначе полого (пляж).
            cliffy = sm((-0.05 - e) / 0.3);
            let land_k = sm((c + 0.1) / lerp(0.14, 0.015, cliffy));
            let inland = sm((c + 0.05) / 0.4);
            // Холмы — от подошвы вверх (без ям ниже моря): чем ниже эрозия, тем выше; вершины и низины пологие.
            let hilly = sm((0.4 - e) / 0.75);
            let hn = self.hill.fbm(fx / 210.0, fz / 210.0, 2) * 2.6;
            let rel = (2.0 + 22.0 * hilly * hilly) * sm(hn * 0.5 + 0.5);
            // Горные цепи — вдоль линий нуля крупного шума (хребты тянутся на километры), где эрозия невысокая;
            // при очень низкой эрозии — и отдельные массивы.
            let chain = if e < 0.3 {
                sm(1.0 - self.ridge.perlin(fx / 2200.0 + 77.0, fz / 2200.0 - 41.0).abs() / 0.15)
            } else {
                0.0
            };
            mtn = (sm((chain - 0.15) / 0.5) * sm((0.3 - e) / 0.45)).max(sm((-0.4 - e) / 0.3))
                * land_k * (0.45 + 0.55 * inland) * (1.0 - 0.85 * near0);
            let mut mh = 0.0;
            if mtn > 0.01 {
                // Пики и долины: гребни двух масштабов, выше всего — на оси цепи.
                let n1 = self.ridge.perlin(fx / 420.0, fz / 420.0);
                let n2 = self.ridge.perlin(fx / 170.0 + 31.0, fz / 170.0 - 17.0);
                let r1 = (1.0 - n1.abs() * 2.4).max(0.0);
                let r2 = (1.0 - n2.abs() * 2.4).max(0.0);
                let peaks = r1 * r1 * (0.6 + 0.4 * r2 * r2);
                mh = mtn * (14.0 + 66.0 * peaks * (0.55 + 0.45 * chain) + 3.0 * det);
            }
            let mut v = (rel * (1.0 - 0.6 * mtn) + mh) * land_k;
            // Плато и террасы: полки с уступами — в тёплом климате (саванна, пустыня), местами в предгорьях.
            if v > 2.0 {
                let pl = self.hill.perlin(fx / 640.0 + 91.0, fz / 640.0 - 33.0) * 2.0;
                let warm = sm((temp - 0.05) / 0.25);
                let tk = sm((pl - 0.05) / 0.3) * (0.3 + 0.7 * warm) * (0.25 + 0.75 * hilly) * (1.0 - 0.5 * mtn);
                if tk > 0.0 {
                    v = lerp(v, terrace(v + det * 1.5, 6.5 + 2.0 * pl, 0.3), tk);
                }
            }
            h += v;
            // Дюны в пустыне.
            let desert = sm((temp - 0.35) / 0.1) * sm((-0.1 - hum) / 0.1) * land_k * (1.0 - mtn);
            if desert > 0.0 {
                h += desert * (1.0 - self.det.perlin(fx / 34.0, fz / 46.0).abs()).powi(2) * 5.0;
            }
            // Болотистые низины: ровно у воды, заводи пятнами.
            swamp = sm((e - 0.12) / 0.2) * sm((hum - 0.05) / 0.15) * sm((temp + 0.1) / 0.2) * sm((0.35 - c) / 0.2)
                * land_k * (1.0 - mtn);
            if swamp > 0.0 {
                h = lerp(h, s + 0.4 + det * 2.2, swamp);
            }
            // Извилистость рек и озёр.
            let (wx, wz) = self.warp(fx, fz);
            // Озёра: редкие котловины на равнинах, неровной формы и разной величины.
            if mtn < 0.3 {
                let ln = self.lake_n(wx, wz) + det * 0.03;
                let basin = sm((ln - 0.55) / 0.3) * land_k * (1.0 - mtn / 0.3);
                if basin > 0.0 {
                    h = lerp(h, s + 1.0, basin);
                    let core = sm((ln - 0.88) / 0.08);
                    if core > 0.0 {
                        h = lerp(h, s - 1.0 - 5.0 * core, core.min(0.999));
                        lake = h < s;
                    }
                }
            }
            // Реки: линия нуля извилистого шума; в горах — ущелья, к морю — шире.
            if land_k > 0.0 {
                let t = self.river_t(wx, wz);
                let core = 0.0045 * (1.0 + 0.8 * (1.0 - inland));
                let valley = core * 3.5 + 0.012 * mtn;
                if t < valley {
                    river_k = sm((valley - t) / (valley - core));
                    let bank = s + 1.5 + (h - s - 1.5).max(0.0) * 0.1 * (1.0 - river_k);
                    h = lerp(h, bank, river_k * land_k);
                    let core_k = sm((core - t) / core * 2.0) * land_k;
                    h = lerp(h, s - 4.0 + 4.0 * (t / core), core_k);
                    let mid = core * 0.75;
                    if t < mid && land_k > 0.3 {
                        h = h.min(s - 1.0 - 3.0 * (1.0 - t / mid));
                    }
                }
            }
        }
        // Вершины мягко прижаты к потолку мира.
        if h > 108.0 {
            h = 108.0 + (h - 108.0) * 0.5;
        }
        let hi = (h.clamp(4.0, (H - 3) as f32)) as i32;
        let tt = temp - (hi - SEA - 12).max(0) as f32 / 75.0;
        // Границы биомов — рваные: пороги климата чуть сдвигает мелкий шум (±несколько блоков).
        let (tj, hj) = (tt + det * 0.035, hum - det * 0.035);
        let mut var = Var::None;
        let biome = if hi < SEA {
            if lake || river_k > 0.7 {
                Biome::River
            } else if swamp > 0.5 {
                var = Var::Swamp;
                Biome::Forest
            } else {
                Biome::Ocean
            }
        } else if hi <= SEA + 2 && c + det * 0.03 < -0.04 && mtn < 0.3 && swamp < 0.5 {
            if cliffy > 0.5 {
                var = Var::Stony;
            }
            Biome::Beach
        } else if hi > SEA + 48 + (det * 5.0) as i32 - (temp * 14.0) as i32 {
            Biome::Peaks
        } else if mtn > 0.4 && hi > SEA + 26 {
            Biome::Mountain
        } else if tj > 0.38 && hj < -0.12 {
            Biome::Desert
        } else if tj > 0.22 && hj < 0.12 {
            Biome::Savanna
        } else if tj < -0.5 {
            Biome::Snowy
        } else if tj < -0.2 {
            if vary > 0.35 {
                var = Var::OldGrowth;
            } else if vary < -0.3 {
                var = Var::Sparse;
            }
            Biome::Taiga
        } else if swamp > 0.5 {
            var = Var::Swamp;
            Biome::Forest
        } else if tj < 0.02 && hj > -0.05 {
            if vary > 0.4 {
                var = Var::OldGrowth;
            }
            Biome::Birch
        } else if hj > 0.02 {
            if vary > 0.35 {
                var = Var::OldGrowth;
            } else if vary < -0.25 {
                var = Var::Sparse;
            }
            Biome::Forest
        } else {
            if vary > 0.4 {
                var = Var::Meadow;
            }
            Biome::Plains
        };
        Column { h: hi, biome, var, det, tt, hum, vary, swamp }
    }
}

/// Имя биома с вариантом (для подсказки на экране).
#[allow(dead_code)]
pub(crate) fn biome_name(c: &Column) -> &'static str {
    match (c.biome, c.var) {
        (Biome::Plains, Var::Meadow) => "Flower meadow",
        (Biome::Forest, Var::Swamp) => "Swamp",
        (Biome::Forest, Var::Sparse) => "Sparse woodland",
        (Biome::Forest, Var::OldGrowth) => "Old growth forest",
        (Biome::Birch, Var::OldGrowth) => "Old growth birch forest",
        (Biome::Taiga, Var::OldGrowth) => "Old growth taiga",
        (Biome::Taiga, Var::Sparse) => "Sparse taiga",
        (Biome::Beach, Var::Stony) => "Stony shore",
        (Biome::Ocean, _) => "Ocean",
        (Biome::Beach, _) => "Beach",
        (Biome::River, _) => "River",
        (Biome::Plains, _) => "Plains",
        (Biome::Forest, _) => "Forest",
        (Biome::Birch, _) => "Birch forest",
        (Biome::Taiga, _) => "Taiga",
        (Biome::Snowy, _) => "Snowy plains",
        (Biome::Savanna, _) => "Savanna",
        (Biome::Desert, _) => "Desert",
        (Biome::Mountain, _) => "Mountains",
        (Biome::Peaks, _) => "Snowy peaks",
    }
}

// ---------------------------------------------------------------- цвет травы и листвы (biome blend)

// Оттенки палитры (tools/atlas.py печатает их же; там же запечены кайма бока и горная трава).
pub(crate) const T_PLAINS: [f32; 3] = [0.4516, 0.6667, 0.2581];
pub(crate) const T_FOREST: [f32; 3] = [0.3548, 0.5873, 0.2258];
pub(crate) const T_BIRCH: [f32; 3] = [0.4516, 0.6667, 0.2903];
pub(crate) const T_TAIGA: [f32; 3] = [0.3548, 0.5397, 0.3226];
pub(crate) const T_SNOWY: [f32; 3] = [0.4516, 0.6190, 0.4839];
pub(crate) const T_SAVANNA: [f32; 3] = [0.7419, 0.6825, 0.3226];
pub(crate) const T_MOUNTAIN: [f32; 3] = [0.4194, 0.5714, 0.3871];
pub(crate) const FRINGE_TINTS: [[f32; 3]; 6] = [[0.4516, 0.6667, 0.2581], [0.3548, 0.5873, 0.2258], [0.3548, 0.5397, 0.3226], [0.7419, 0.6825, 0.3226], [0.5806, 0.6667, 0.2903], [0.4194, 0.5714, 0.3871]];

pub(crate) fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

/// Оттенок травы/листвы столбца — плавная функция климата (как карта цветов Minecraft): у порогов биомов
/// цвет перетекает за десятки блоков, поэтому граница саванны и равнины не видна линией.
pub(crate) fn grass_tint(c: &Column) -> [f32; 3] {
    let (tt, hum) = (c.tt, c.hum);
    let lush = sm((hum + 0.22) / 0.3);
    let mut t = mix3(T_PLAINS, T_FOREST, lush);
    let birch = sm((tt + 0.38) / 0.1) * sm((0.02 - tt) / 0.1) * sm((hum + 0.18) / 0.12);
    t = mix3(t, T_BIRCH, birch * 0.8);
    t = mix3(t, T_TAIGA, sm((-0.2 - tt) / 0.16));
    t = mix3(t, T_SNOWY, sm((-0.56 - tt) / 0.14));
    t = mix3(t, T_SAVANNA, sm((tt - 0.15) / 0.16) * sm((0.3 - hum) / 0.16));
    t = mix3(t, T_MOUNTAIN, sm((c.h - SEA - 22) as f32 / 18.0));
    // Болото — темнее и оливковее (из той же палитры).
    let sw = c.swamp * 0.85;
    t = mix3(t, [T_TAIGA[0] * 0.9, T_FOREST[1] * 0.82, T_TAIGA[2] * 0.62], sw);
    // Лёгкая пятнистость.
    let k = 1.0 + 0.035 * c.det;
    [t[0] * k, t[1] * k, t[2] * k]
}

pub(crate) fn rgb565(t: [f32; 3]) -> u16 {
    let r = (t[0].clamp(0.0, 1.0) * 31.0 + 0.5) as u16;
    let g = (t[1].clamp(0.0, 1.0) * 63.0 + 0.5) as u16;
    let b = (t[2].clamp(0.0, 1.0) * 31.0 + 0.5) as u16;
    (r << 11) | (g << 5) | b
}

/// Ближайший запечённый вариант каймы бока травы.
pub(crate) fn fringe_variant(t: [f32; 3]) -> u8 {
    let mut best = (f32::MAX, 0u8);
    for (i, f) in FRINGE_TINTS.iter().enumerate() {
        let d = (t[0] - f[0]).powi(2) + (t[1] - f[1]).powi(2) + (t[2] - f[2]).powi(2);
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}

/// Густота деревьев: опушки по влажности, поляны и редколесье по шуму разнообразия, луга почти без деревьев.
/// Всё плавно — без резкой стены леса. Не больше 0.6 (порог в generate).
pub(crate) fn tree_density(c: &Column) -> f32 {
    let open = sm((c.vary + 0.55) / 0.4); // 0 — поляна, 1 — лес
    match c.biome {
        Biome::Forest if c.var == Var::Swamp => 0.2,
        Biome::Forest | Biome::Plains => {
            let wood = 0.02 + 0.53 * sm((c.hum + 0.2) / 0.22);
            let meadow = if c.biome == Biome::Plains { sm((c.vary - 0.28) / 0.15) } else { 0.0 };
            wood * (0.2 + 0.8 * open) * (1.0 - 0.97 * meadow)
        }
        Biome::Birch => 0.06 + 0.44 * open,
        Biome::Taiga => 0.05 + 0.5 * open,
        Biome::Snowy => 0.1,
        Biome::Savanna => 0.06,
        Biome::Mountain => 0.07,
        Biome::Desert => 0.1,
        _ => 0.0,
    }
}

/// Дальше всего дерево выступает от ствола (крона крупного дуба, ель 2×2): столько клеток решётки деревьев
/// за краем области ещё просматривается.
pub(crate) const TREE_REACH: i32 = 6;
/// Предел густоты деревьев с пятнистостью (tree_density × множитель): выше — попытку можно не считать.
pub(crate) const TREE_MAX: f32 = 0.72;

pub(crate) fn generate(gen: &Gen, cx: i32, cz: i32) -> Region {
    let x0 = cx * CS - PAD;
    let z0 = cz * CS - PAD;
    // Блоки генератора — меньше 256: пишутся младшим байтом напрямую (gen_tests: старшего байта у области нет).
    let mut reg = Region { x0, z0, blocks: vec![AIR as u8; (W * W * H) as usize], blocks_hi: None, tint: vec![0xFFFF; (W * W) as usize], states: None };
    // Столбцы области с запасом в 1 для наклона.
    let cw = W + 2;
    let mut cols: Vec<Column> = Vec::with_capacity((cw * cw) as usize);
    for lz in -1..W + 1 {
        for lx in -1..W + 1 {
            cols.push(gen.column(x0 + lx, z0 + lz));
        }
    }
    let col = |lx: i32, lz: i32| -> &Column { &cols[((lz + 1) * cw + lx + 1) as usize] };

    for lz in 0..W {
        for lx in 0..W {
            let c = col(lx, lz);
            let h = c.h;
            let low = [col(lx - 1, lz).h, col(lx + 1, lz).h, col(lx, lz - 1).h, col(lx, lz + 1).h]
                .iter()
                .fold(h, |a, &b| a.min(b));
            let drop = h - low;
            let (top, fill, depth) = match c.biome {
                Biome::Ocean => (if SEA - h > 9 { GRAVEL } else if c.det > 0.35 { CLAY } else { SAND }, SAND, 3),
                Biome::River => (if c.det > 0.2 { GRAVEL } else if c.det < -0.35 { CLAY } else { SAND }, SAND, 2),
                Biome::Beach if c.var == Var::Stony => (if c.det > 0.15 { GRAVEL } else { STONE }, STONE, 1),
                Biome::Beach => (SAND, SAND, 4),
                // Уступы пустыни — песчаник.
                Biome::Desert => (if drop >= 3 { SANDSTONE } else { SAND }, SANDSTONE, 5),
                // Снег держится на полках; крутые стены — камень.
                Biome::Peaks => (if drop >= 5 { STONE } else { SNOW }, STONE, 2),
                Biome::Mountain => {
                    if drop >= 3 {
                        (STONE, STONE, 0)
                    } else if c.det > 0.35 {
                        (GRAVEL, STONE, 1)
                    } else {
                        (GRASS, DIRT, 2)
                    }
                }
                Biome::Savanna | Biome::Taiga => (GRASS, DIRT, 3),
                Biome::Snowy => (SNOWY_GRASS, DIRT, 3),
                _ => (GRASS, DIRT, 3),
            };
            let t = grass_tint(c);
            reg.tint[(lz * W + lx) as usize] = rgb565(t);
            let mut top = top;
            if top == GRASS {
                if h <= SEA + 1 && low <= SEA && c.det > -0.15 && c.swamp < 0.5 {
                    // Берег у воды — песок пятнами.
                    top = SAND;
                } else {
                    // Ступень в 1 блок — бок с глубокой каймой (земля узкой тёмной полосой в углу), иначе обычная кайма.
                    top = if drop == 1 { GRASS_FULL } else { GRASS_EDGE + fringe_variant(t) as BlockId };
                }
            }
            let base = Region::idx(lx, 0, lz);
            reg.blocks[base] = BEDROCK as u8;
            for y in 1..=h {
                let d = h - y;
                let b = if d == 0 {
                    if y <= SEA && (is_grass(top) || top == SNOWY_GRASS) {
                        // Дно под водой: в болоте — земля и глина, иначе песок и земля.
                        if c.swamp >= 0.5 {
                            if c.det > 0.3 { CLAY } else { DIRT }
                        } else if c.det > 0.0 {
                            SAND
                        } else {
                            DIRT
                        }
                    } else {
                        top
                    }
                } else if d <= depth {
                    if drop >= 4 && y > low { STONE } else if top == SAND && c.biome == Biome::Desert && d > 3 { SANDSTONE } else { fill }
                } else {
                    STONE
                };
                reg.blocks[base + y as usize] = b as u8;
            }
            for y in h + 1..=SEA {
                reg.blocks[base + y as usize] = WATER as u8;
            }
        }
    }

    // Руды — до деревьев, как шаг UNDERGROUND_ORES перед VEGETAL_DECORATION в Minecraft.
    let mut tops = vec![0i32; (W * W) as usize];
    for lz in 0..W {
        for lx in 0..W {
            tops[(lz * W + lx) as usize] = col(lx, lz).h;
        }
    }
    #[cfg(test)]
    let skip = ORES_OFF.with(|o| o.get());
    #[cfg(not(test))]
    let skip = false;
    if !skip {
        place_lava_lake(&mut reg, gen.seed, cx, cz, &tops);
        place_ores(&mut reg, gen.seed, cx, cz, &tops);
    }

    // Деревья: одна попытка на клетку 5×5; решение — только по столбцам генератора, поэтому соседи получают то же.
    let seed = gen.seed;
    let reach = TREE_REACH;
    let tg = 5;
    for gz in (z0 - reach).div_euclid(tg)..=(z0 + W + reach).div_euclid(tg) {
        for gx in (x0 - reach).div_euclid(tg)..=(x0 + W + reach).div_euclid(tg) {
            let mut r = Rng(hash(seed, 1, gx, gz));
            let roll = r.next();
            let tx = gx * tg + (r.next() * tg as f32) as i32;
            let tz = gz * tg + (r.next() * tg as f32) as i32;
            if roll >= TREE_MAX || tx < x0 - reach || tx > x0 + W + reach || tz < z0 - reach || tz > z0 + W + reach {
                continue;
            }
            let (clx, clz) = (tx - x0, tz - z0);
            let own;
            let c = if clx >= -1 && clz >= -1 && clx <= W && clz <= W {
                col(clx, clz)
            } else {
                own = gen.column(tx, tz);
                &own
            };
            let dens = tree_density(c) * (0.6 + c.det * 1.2).clamp(0.15, 1.3);
            if roll >= dens || c.h <= SEA || c.h + 24 >= H || gen.site.no_trees(tx, tz) {
                continue;
            }
            // В горах — только на полках (на крутом камне деревьев нет).
            if c.biome == Biome::Mountain {
                let hat = |x: i32, z: i32| {
                    let (lx, lz) = (x - x0, z - z0);
                    if lx >= -1 && lz >= -1 && lx <= W && lz <= W { col(lx, lz).h } else { gen.column(x, z).h }
                };
                let low = hat(tx + 1, tz).min(hat(tx - 1, tz)).min(hat(tx, tz + 1)).min(hat(tx, tz - 1));
                if c.h - low >= 3 {
                    continue;
                }
            }
            let h = c.h;
            let size = r.next();
            let kind = r.next();
            match (c.biome, c.var) {
                (Biome::Desert, _) => {
                    for y in 1..=1 + (size * 3.0) as i32 {
                        reg.put(tx, h + y, tz, CACTUS, 0);
                    }
                }
                (Biome::Taiga, Var::OldGrowth) if kind < 0.45 => mega_spruce(&mut reg, tx, h, tz, 14 + (size * 7.0) as i32),
                (Biome::Taiga | Biome::Snowy | Biome::Mountain, _) => spruce(&mut reg, tx, h, tz, 6 + (size * 6.0) as i32),
                (Biome::Birch, Var::OldGrowth) => birch(&mut reg, tx, h, tz, 8 + (size * 4.0) as i32, &mut r),
                (Biome::Birch, _) => birch(&mut reg, tx, h, tz, 5 + (size * 3.0) as i32, &mut r),
                (Biome::Forest, Var::Swamp) => swamp_oak(&mut reg, tx, h, tz, 4 + (size * 3.0) as i32, &mut r),
                (Biome::Savanna, _) => {
                    if kind < 0.45 {
                        bush(&mut reg, tx, h, tz, &mut r)
                    } else {
                        savanna_oak(&mut reg, tx, h, tz, 4 + (size * 3.0) as i32, &mut r)
                    }
                }
                _ => {
                    // Крупный дуб: на равнинах и в редколесье поодиночке, в старом лесу — часто.
                    let big = match c.var {
                        Var::OldGrowth => 0.5,
                        Var::Sparse => 0.3,
                        Var::Meadow => 0.4,
                        _ if c.biome == Biome::Plains => 0.22,
                        _ => 0.07,
                    };
                    let shrub = if c.biome == Biome::Plains || c.var == Var::Sparse { 0.25 } else { 0.06 };
                    if kind < big {
                        big_oak(&mut reg, tx, h, tz, 7 + (size * 5.0) as i32, &mut r)
                    } else if kind < big + shrub {
                        bush(&mut reg, tx, h, tz, &mut r)
                    } else if c.biome == Biome::Forest && kind > 0.75 {
                        birch(&mut reg, tx, h, tz, 5 + (size * 3.0) as i32, &mut r)
                    } else {
                        oak(&mut reg, tx, h, tz, 4 + (size * size * 3.0) as i32, &mut r)
                    }
                }
            }
        }
    }

    // Мелочь в своих столбцах: трава куртинами, цветы (на лугах — коврами двух цветов), папоротник, сухие кусты.
    for lz in 0..W {
        for lx in 0..W {
            let (x, z) = (x0 + lx, z0 + lz);
            let c = col(lx, lz);
            let h = c.h;
            if h <= SEA || h + 1 >= H {
                continue;
            }
            let below = reg.get(lx, h, lz);
            if reg.get(lx, h + 1, lz) != AIR {
                continue;
            }
            let roll = hash(seed, 2, x, z) as f32 / 4294967296.0;
            // Трава куртинами: густо в пятнах, редко между ними (меньше ровного «ковра» крестиков).
            let patch = sm(0.45 + c.det * 1.1);
            // Цвет цветов — пятнами в несколько блоков.
            let j = (c.det * 6.0) as i32;
            let yellow = hash(seed, 3, (x + j).div_euclid(7), (z - j).div_euclid(7)) & 1 == 0;
            let flower = if yellow { DANDELION } else { POPPY };
            let plant = match c.biome {
                Biome::Desert if below == SAND && roll < 0.008 => DEAD_BUSH,
                _ if !is_grass(below) => AIR,
                Biome::Plains => {
                    // Луг — плавно по шуму разнообразия: цветы куртинами, трава между.
                    let m = sm((c.vary - 0.25) / 0.2);
                    let fl = 0.008 + m * 0.34 * sm((c.det + 0.3) / 0.5);
                    if roll < fl {
                        flower
                    } else if roll < fl + 0.02 + 0.2 * patch * (1.0 - 0.5 * m) {
                        TALL_GRASS
                    } else {
                        AIR
                    }
                }
                Biome::Forest if c.var == Var::Swamp => {
                    if roll < 0.06 + 0.3 * patch {
                        if c.det > 0.25 { FERN } else { TALL_GRASS }
                    } else {
                        AIR
                    }
                }
                Biome::Forest | Biome::Birch => {
                    // На полянах — цветы, в чаще — редкая трава.
                    let glade = 1.0 - sm((c.vary + 0.55) / 0.4);
                    if roll < 0.02 + 0.12 * patch + 0.1 * glade {
                        TALL_GRASS
                    } else if roll > 0.99 - 0.06 * glade {
                        flower
                    } else {
                        AIR
                    }
                }
                Biome::Taiga => {
                    let k = if c.var == Var::OldGrowth { 0.25 } else { 0.14 };
                    if roll < 0.02 + k * patch { FERN } else if roll > 0.985 { TALL_GRASS } else { AIR }
                }
                Biome::Savanna if roll < 0.03 + 0.24 * patch => TALL_GRASS,
                Biome::Mountain if roll < 0.05 * patch => TALL_GRASS,
                _ => AIR,
            };
            if plant != AIR {
                reg.blocks[Region::idx(lx, h + 1, lz)] = plant as u8;
            }
        }
    }
    if gen.site.touches(&reg) {
        gen.site.stamp(seed, &mut reg);
    }
    reg
}

// ---------------------------------------------------------------- руды (ASP-06 §2.9, решение: 1.18+ под 0–127 и море 48)
//
// Ванильные фичи ore_coal_upper / lower, ore_iron_upper / middle / small: жила OreFeature заданного размера, попытки на
// кусок, высота — равномерно или треугольником (TrapezoidHeightProvider без плато), замена только камня. Высоты
// переведены кусочно-линейно от моря: ниже моря Java 63 → 48, дно −64 → 0 (×48/127); от моря до 80 (равнины и холмы)
// — 63…80 → 48…68; выше 80 (горы до 320) — 80…320 → 68…127 (×59/240). Число попыток уменьшено в той же мере, что и
// диапазон высот, — плотность руды в камне на каждой высоте как в ванили (дробная часть — попытка с этой вероятностью).
//
// | фича        | руда   | Java: высота, попыток       | у нас: высота         | попыток | жила | на воздухе |
// | coal_upper  | уголь  | 136…320 равн., 30           | 82…127 равн.          | 7.3     | 17   | —          |
// | coal_lower  | уголь  | 0…192 треуг., 20            | 24…96 треуг. (пик 60) | 7.5     | 17   | 50 % нет   |
// | iron_upper  | железо | 80…384 треуг., 90           | 68…143 треуг. (пик 105) | 22.2  | 9    | —          |
// | iron_middle | железо | −24…56 треуг., 10           | 15…45 треуг. (пик 30) | 3.75    | 9    | —          |
// | iron_small  | железо | −64…72 равн., 10            | 0…58 равн.            | 4.26    | 4    | —          |
// Mekanism M1 (MEK-05 §2.2, решение после MEK-05/07/16 — строки той же таблицы; олово — один основной слой):
// | gold        | золото | −64…32 треуг., 4            | 0…36 треуг.           | 1.5     | 9    | 50 % нет   |
// | gold_lower  | золото | −64…−48 равн., 0…1          | 0…6 равн.             | 0.19    | 9    | 50 % нет   |
// | redstone    | редст. | −64…15 равн., 4             | 0…30 равн.            | 1.52    | 8    | —          |
// | redst_lower | редст. | −96…−32 треуг., 8           | −12…12 треуг.         | 3.0     | 8    | —          |
// | copper      | медь   | −16…112 треуг., 16          | 18…76 треуг.          | 7.25    | 10   | —          |
// | osmium_up   | осмий  | 72…343 трапеция (плато 8), 65 | 59…133 (плато 2)    | 17.75   | 7    | —          |
// | osmium_mid  | осмий  | −32…56 треуг., 6            | 12…45 треуг.          | 2.25    | 9    | —          |
// | osmium_small| осмий  | −64…64 равн., 8             | 0…49 равн.            | 3.06    | 4    | —          |
// | tin_large   | олово  | −32…72 треуг., 12           | 12…59 треуг.          | 5.42    | 9    | —          |
// Новые фичи — после старых (номер фичи входит в хеш жилы: уголь и железо остались на месте); ванильные — раньше
// Mekanism (как модификаторы биома NeoForge): руда, легшая раньше, позже не перекрывается.
//
// Жила может выйти за край куска (до ~5 блоков): область считает жилы своего куска и восьми соседей, и каждая жила
// детерминирована по (кусок, фича, номер) — соседние области кладут одни и те же клетки. Случайность «на воздухе» — хеш
// клетки, а не порядок обхода (область видит только свою часть жилы).

pub(crate) struct OreFeature {
    pub(crate) block: BlockId,
    pub(crate) count: f32,
    pub(crate) size: i32,
    pub(crate) lo: i32,
    pub(crate) hi: i32,
    pub(crate) tri: bool,
    pub(crate) discard: f32,
    /// Плато трапеции (TrapezoidHeightProvider.plateau; 0 — треугольник).
    pub(crate) plat: i32,
}

const fn ore_f(block: BlockId, count: f32, size: i32, lo: i32, hi: i32, tri: bool, discard: f32) -> OreFeature {
    OreFeature { block, count, size, lo, hi, tri, discard, plat: 0 }
}

pub(crate) const ORES: [OreFeature; 14] = [
    ore_f(COAL_ORE, 7.3, 17, 82, 127, false, 0.0),
    ore_f(COAL_ORE, 7.5, 17, 24, 96, true, 0.5),
    ore_f(IRON_ORE, 22.2, 9, 68, 143, true, 0.0),
    ore_f(IRON_ORE, 3.75, 9, 15, 45, true, 0.0),
    ore_f(IRON_ORE, 4.26, 4, 0, 58, false, 0.0),
    ore_f(GOLD_ORE, 1.5, 9, 0, 36, true, 0.5),
    ore_f(GOLD_ORE, 0.19, 9, 0, 6, false, 0.5),
    ore_f(REDSTONE_ORE, 1.52, 8, 0, 30, false, 0.0),
    ore_f(REDSTONE_ORE, 3.0, 8, -12, 12, true, 0.0),
    ore_f(COPPER_ORE, 7.25, 10, 18, 76, true, 0.0),
    OreFeature { block: OSMIUM_ORE, count: 17.75, size: 7, lo: 59, hi: 133, tri: true, discard: 0.0, plat: 2 },
    ore_f(OSMIUM_ORE, 2.25, 9, 12, 45, true, 0.0),
    ore_f(OSMIUM_ORE, 3.06, 4, 0, 49, false, 0.0),
    ore_f(TIN_ORE, 5.42, 9, 12, 59, true, 0.0),
];

/// Соль хешей руд (отдельно от деревьев и травы).
pub(crate) const ORE_SALT: u32 = 0x0E5E_0000;

/// Высота попытки (UniformHeightProvider / TrapezoidHeightProvider с плато 0): два равномерных числа из хеша.
pub(crate) fn ore_height(f: &OreFeature, h: u32) -> i32 {
    let span = f.hi - f.lo;
    if !f.tri {
        return f.lo + ((h & 0xFFFF) as i64 * (span as i64 + 1) >> 16) as i32;
    }
    let l = (span - f.plat).max(0) / 2;
    let m = span - l;
    f.lo + ((h & 0xFFFF) as i64 * (m as i64 + 1) >> 16) as i32 + ((h >> 16) as i64 * (l as i64 + 1) >> 16) as i32
}

/// Форм жил на размер: жила попытки — одна из них (номер — из хеша попытки). Форма — клетки шаров OreFeature
/// относительно точки попытки; считаются один раз на поток (воркер) — генерация куска только раскладывает их.
pub(crate) const VEIN_SHAPES: usize = 512;

thread_local! {
    /// Формы жил по размерам (порядок ORE_SIZES).
    static VEINS: Vec<Vec<Box<[u32]>>> = ORE_SIZES.iter().map(|&s| (0..VEIN_SHAPES as u32).map(|k| vein_shape(s, hash(0x0E5E_5A9E, k, s, 0)).into_boxed_slice()).collect()).collect();
}
#[cfg(test)]
thread_local! {
    /// Замер цены руд (ore_tests::ore_cost): генерация без них.
    static ORES_OFF: std::cell::Cell<bool> = std::cell::Cell::new(false);
}
pub(crate) const ORE_SIZES: [i32; 6] = [17, 9, 4, 8, 10, 7];

/// Руды в куске (cx, cz): жилы своего куска и восьми соседей (жила выходит за край на 2–4 блока), по фичам в порядке
/// Minecraft. tops — верх столбцов области (W × W): выше него камня нет. Руда кладётся только в сам кусок: рамка
/// области нужна мешу лишь как «непрозрачно», а камень и руда там одинаковы.
pub(crate) fn place_ores(reg: &mut Region, seed: u32, cx: i32, cz: i32, tops: &[i32]) {
    let (kx0, kz0) = (cx * CS, cz * CS);
    let (kx1, kz1) = (kx0 + CS - 1, kz0 + CS - 1);
    // Верх рельефа квадратами 6 × 6 (сетка 3 × 3 по области 18 × 18): жила над своим местом рельефа не раскладывается.
    let mut grid = [0i32; 9];
    for lz in 0..W {
        for lx in 0..W {
            let g = &mut grid[(lz / 6 * 3 + lx / 6) as usize];
            *g = (*g).max(tops[(lz * W + lx) as usize]);
        }
    }
    let (rx0, rz0) = (reg.x0, reg.z0);
    let local_max = |x0: i32, x1: i32, z0: i32, z1: i32| -> i32 {
        let (a, b) = ((x0 - rx0).clamp(0, W - 1) / 6, (x1 - rx0).clamp(0, W - 1) / 6);
        let (c, d) = ((z0 - rz0).clamp(0, W - 1) / 6, (z1 - rz0).clamp(0, W - 1) / 6);
        let mut m = 0;
        for gz in c..=d {
            for gx in a..=b {
                m = m.max(grid[(gz * 3 + gx) as usize]);
            }
        }
        m
    };
    VEINS.with(|shapes| {
        for (fi, f) in ORES.iter().enumerate() {
            let si = ORE_SIZES.iter().position(|&s| s == f.size).unwrap_or(0);
            // Жила не дальше g + r_max от точки (отрезок ±size/8 и шар до (size/8 + 1)/2).
            let reach = (f.size as f64 / 8.0 + (2.0 * f.size as f64 / 16.0 + 1.0) / 2.0).ceil() as i32;
            let n = f.count.ceil() as u32;
            let frac = f.count - f.count.floor();
            for ccz in cz - 1..=cz + 1 {
                for ccx in cx - 1..=cx + 1 {
                    for k in 0..n {
                        let id = (fi as u32) << 8 | k;
                        let h = hash(seed ^ ORE_SALT, id, ccx, ccz);
                        if k as f32 + 1.0 > f.count && (h >> 24) as f32 / 256.0 >= frac {
                            continue;
                        }
                        let (ox, oz) = (ccx * CS + (h & 15) as i32, ccz * CS + (h >> 4 & 15) as i32);
                        if ox + reach < kx0 || ox - reach > kx1 || oz + reach < kz0 || oz - reach > kz1 {
                            continue;
                        }
                        let hy = hash(seed ^ ORE_SALT ^ 0x1111, id, ccx, ccz);
                        let y = ore_height(f, hy);
                        // Жила — от 2 + r ниже точки до r выше: целиком над рельефом своего места или под дном — камня нет.
                        if y + reach < 1 || y - 2 - reach >= H || y - 2 - reach > local_max(ox - reach, ox + reach, oz - reach, oz + reach) {
                            continue;
                        }
                        let vs = hash(seed ^ ORE_SALT ^ 0x2222, id, ccx, ccz);
                        for &c in shapes[si][(vs as usize) % VEIN_SHAPES].iter() {
                            let (x, z) = (ox + (c & 31) as i32 - 16, oz + (c >> 5 & 31) as i32 - 16);
                            let yy = y + (c >> 10 & 31) as i32 - 16;
                            if x < kx0 || x > kx1 || z < kz0 || z > kz1 || yy < 1 || yy >= H {
                                continue;
                            }
                            let (lx, lz) = (x - reg.x0, z - reg.z0);
                            if yy > tops[(lz * W + lx) as usize] {
                                continue;
                            }
                            let at = Region::idx(lx, yy, lz);
                            if reg.blocks[at] != STONE as u8 {
                                continue;
                            }
                            // «На воздухе» (discardOnAirChance): доля — по хешу клетки, не по порядку обхода.
                            if f.discard > 0.0 && (hash(vs, yy as u32, x, z) >> 8) as f32 / 16777216.0 < f.discard && ore_exposed(reg, lx, yy, lz) {
                                continue;
                            }
                            reg.blocks[at] = f.block as u8;
                        }
                    }
                }
            }
        }
    });
}

/// Форма жилы (OreFeature.generate + generateVeinPart) относительно точки попытки: отрезок длиной size/4 под
/// случайным углом, вдоль него size шаров с радиусом по синусу (толще в середине). Клетка — dx + 16 (5 бит), dz + 16
/// (5 бит), dy + 16 (5 бит), без повторов. Отсев вложенных шаров Minecraft не нужен: объединение шаров от него не
/// меняется, это лишь ускорение Java.
pub(crate) fn vein_shape(size: i32, vs: u32) -> Vec<u32> {
    let mut r = Rng(vs);
    let ang = r.next() * std::f32::consts::PI;
    let g = size as f32 / 8.0;
    let (sn, cs) = (libm::sinf(ang) as f64 * g as f64, libm::cosf(ang) as f64 * g as f64);
    let y0 = ((r.next() * 3.0) as i32 - 2) as f64;
    let y1 = ((r.next() * 3.0) as i32 - 2) as f64;
    let mut out = Vec::with_capacity(64);
    for k in 0..size {
        let t = k as f64 / size as f64;
        let hh = r.next() as f64 * size as f64 / 16.0;
        let sin = libm::sinf(std::f32::consts::PI * (k as f32 / size as f32)) as f64;
        let d = ((sin + 1.0) * hh + 1.0) / 2.0;
        let (cx, cy, cz) = (sn - t * 2.0 * sn, y0 + t * (y1 - y0), cs - t * 2.0 * cs);
        let d2 = d * d;
        for x in ifloor(cx - d)..=ifloor(cx + d) {
            let dx = x as f64 + 0.5 - cx;
            for z in ifloor(cz - d)..=ifloor(cz + d) {
                let dz = z as f64 + 0.5 - cz;
                for y in ifloor(cy - d)..=ifloor(cy + d) {
                    let dy = y as f64 + 0.5 - cy;
                    if dx * dx + dy * dy + dz * dz < d2 {
                        out.push((x + 16) as u32 | ((z + 16) as u32) << 5 | ((y + 16) as u32) << 10);
                    }
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// ⌊x⌋ без вызова floor (в нативной сборке без SSE4.1 это вызов функции; в WASM — команда).
#[inline]
pub(crate) fn ifloor(x: f64) -> i32 {
    let i = x as i32;
    if (i as f64) > x {
        i - 1
    } else {
        i
    }
}

// ---------------------------------------------------------------- подземные лавовые озёра (Mekanism M1: тепловой генератор)
//
// Ванильный lake_lava_underground (LakeFeature): в среднем 1 кусок из 9, коробка 16 × 8 × 16 из 4…7 эллипсоидов —
// нижние 4 слоя лавой, верхние — воздухом (пещерка над озером). У нас коробка целиком внутри своего куска (отступ 1 от
// краёв — рамке соседних областей ничего не надо знать), высота озера 6…39 и только под рельефом (до верха столбцов
// коробки не меньше 4 блоков камня). Лава — неподвижный источник (течение — ASP-15).

/// Соль хешей озёр.
pub(crate) const LAKE_SALT: u32 = 0x1A4E_0000;

pub(crate) fn place_lava_lake(reg: &mut Region, seed: u32, cx: i32, cz: i32, tops: &[i32]) {
    let h = hash(seed ^ LAKE_SALT, 0, cx, cz);
    if h % 9 != 0 {
        return;
    }
    let mut r = Rng(hash(seed ^ LAKE_SALT, 1, cx, cz));
    let y0 = 6 + (r.next() * 34.0) as i32;
    // Коробка 14 × 8 × 14 в клетках куска 1…14 (+PAD в области).
    let mut top = i32::MAX;
    for lz in 1..15 {
        for lx in 1..15 {
            top = top.min(tops[((lz + PAD) * W + lx + PAD) as usize]);
        }
    }
    if y0 + 8 + 4 > top {
        return;
    }
    let mut mark = [false; 14 * 14 * 8];
    let n = 4 + (r.next() * 4.0) as i32;
    for _ in 0..n {
        let d = r.next() as f64 * 5.0 + 3.0;
        let e = r.next() as f64 * 4.0 + 2.0;
        let f = r.next() as f64 * 5.0 + 3.0;
        let g = r.next() as f64 * (14.0 - d - 2.0) + 1.0 + d / 2.0;
        let hh = r.next() as f64 * (8.0 - e - 4.0) + 2.0 + e / 2.0;
        let k = r.next() as f64 * (14.0 - f - 2.0) + 1.0 + f / 2.0;
        for x in 1..13 {
            for z in 1..13 {
                for y in 1..7 {
                    let (a, b, c) = ((x as f64 - g) / (d / 2.0), (y as f64 - hh) / (e / 2.0), (z as f64 - k) / (f / 2.0));
                    if a * a + b * b + c * c < 1.0 {
                        mark[((x * 14 + z) * 8 + y) as usize] = true;
                    }
                }
            }
        }
    }
    for x in 0..14 {
        for z in 0..14 {
            for y in 0..8 {
                if mark[((x * 14 + z) * 8 + y) as usize] {
                    let b = if y < 4 { LAVA } else { AIR };
                    reg.blocks[Region::idx(x + 1 + PAD, y0 + y, z + 1 + PAD)] = b as u8;
                }
            }
        }
    }
}

/// Клетка у воздуха (Feature.isExposedToAir); за краем области — не воздух.
pub(crate) fn ore_exposed(reg: &Region, lx: i32, y: i32, lz: i32) -> bool {
    [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)].iter().any(|&(dx, dy, dz)| {
        let (x, yy, z) = (lx + dx, y + dy, lz + dz);
        x >= 0 && z >= 0 && x < W && z < W && yy >= 0 && yy < H && reg.blocks[Region::idx(x, yy, z)] == AIR as u8
    })
}

pub(crate) fn trunk(reg: &mut Region, x: i32, y0: i32, y1: i32, z: i32, b: BlockId) {
    for y in y0..=y1 {
        reg.put(x, y, z, b, 1);
    }
}

pub(crate) fn oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    for dy in t - 2..=t + 1 {
        let rad: i32 = if dy < t { 2 } else { 1 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner && (dy == t + 1 || r.next() < 0.5) {
                    continue;
                }
                reg.put(x + dx, h + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

/// Шар листвы (сплюснутый по высоте), край — с рваными углами.
pub(crate) fn blob(reg: &mut Region, x: i32, y: i32, z: i32, rad: f32, leaf: BlockId, r: &mut Rng) {
    let salt = (r.next() * 16777216.0) as u32;
    let ri = rad.ceil() as i32;
    let r2 = rad * rad;
    let (ax, bx, az, bz) = reg.span(x, z, -ri, ri);
    for dy in -ri + 1..=ri - 1 {
        for dx in ax..=bx {
            for dz in az..=bz {
                let d = (dx * dx + dz * dz) as f32 + (dy * dy) as f32 * 2.2;
                if d <= r2 - 0.6 || (d <= r2 + 0.6 && hash(salt, dy as u32, dx, dz) & 1 == 0) {
                    reg.put(x + dx, y + dy, z + dz, leaf, 0);
                }
            }
        }
    }
}

/// Крупный дуб: высокий ствол, ветви наискось и крона из нескольких шаров.
pub(crate) fn big_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 7) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    blob(reg, x, h + t + 1, z, 3.2, LEAVES, r);
    let n = 2 + (r.next() * 2.0) as i32;
    for i in 0..n {
        // Ветвь: от ствола наискось вверх (2–3 бревна), на конце — шар листвы.
        let a = (i as f32 + r.next() * 0.6) * std::f32::consts::TAU / n as f32;
        let (dx, dz) = (libm::cosf(a), libm::sinf(a));
        let y0 = h + t - 3 + (r.next() * 2.0) as i32;
        let len = 2 + (r.next() * 2.0) as i32;
        let (mut bx, mut bz) = (x, z);
        for k in 1..=len {
            bx = x + (dx * k as f32).round() as i32;
            bz = z + (dz * k as f32).round() as i32;
            reg.put(bx, y0 + k, bz, LOG, 1);
        }
        blob(reg, bx, y0 + len + 1, bz, 2.4 + r.next() * 0.6, LEAVES, r);
    }
}

/// Куст: бревно и шапка листвы у земли.
pub(crate) fn bush(reg: &mut Region, x: i32, h: i32, z: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    reg.put(x, h + 1, z, LOG, 1);
    let rad: i32 = if r.next() < 0.5 { 1 } else { 2 };
    for dy in 1..=2 {
        let rr = if dy == 2 { rad - 1 } else { rad };
        for dx in -rr..=rr {
            for dz in -rr..=rr {
                if rr > 0 && dx.abs() == rr && dz.abs() == rr && r.next() < 0.7 {
                    continue;
                }
                reg.put(x + dx, h + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

/// Дерево саванны: ствол с изломом и плоская широкая крона.
pub(crate) fn savanna_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    let (sx, sz) = if r.next() < 0.5 { (1, 0) } else { (0, 1) };
    let s = if r.next() < 0.5 { 1 } else { -1 };
    let bend = t - 2;
    trunk(reg, x, h + 1, h + bend, z, LOG);
    let (tx, tz) = (x + sx * s, z + sz * s);
    trunk(reg, tx, h + bend + 1, h + t, tz, LOG);
    for (dy, rad) in [(1i32, 3i32), (2, 2)] {
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner || (dx.abs() + dz.abs() == 2 * rad - 1 && r.next() < 0.4) {
                    continue;
                }
                reg.put(tx + dx, h + t + dy - 1, tz + dz, LEAVES, 0);
            }
        }
    }
}

/// Болотный дуб: невысокий, крона широкая и плоская.
pub(crate) fn swamp_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    for (dy, rad) in [(-1i32, 3i32), (0, 3), (1, 2)] {
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner && (dy != 0 || r.next() < 0.6) {
                    continue;
                }
                reg.put(x + dx, h + t + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

pub(crate) fn birch(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, _r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, BIRCH_LOG);
    for dy in t - 3..=t + 1 {
        let rad: i32 = if dy <= t - 1 && dy > t - 3 { 2 } else { 1 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                if !(dx.abs() == rad && dz.abs() == rad) || (rad == 1 && dy < t) {
                    reg.put(x + dx, h + dy, z + dz, BIRCH_LEAVES, 0);
                }
            }
        }
    }
    reg.put(x, h + t + 1, z, BIRCH_LEAVES, 0);
}

pub(crate) fn spruce(reg: &mut Region, x: i32, h: i32, z: i32, t: i32) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, SPRUCE_LOG);
    reg.put(x, h + t + 1, z, SPRUCE_LEAVES, 0);
    reg.put(x, h + t + 2, z, SPRUCE_LEAVES, 0);
    for dy in (3..=t).rev() {
        let k = t - dy;
        let rad = (1 + k / 2).min(3) - if k % 2 == 0 && k > 0 { 1 } else { 0 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                if !(rad >= 2 && dx.abs() == rad && dz.abs() == rad) && !(k == 0 && dx != 0 && dz != 0) {
                    reg.put(x + dx, h + dy, z + dz, SPRUCE_LEAVES, 0);
                }
            }
        }
    }
}

/// Большая ель старого леса: ствол 2×2 (уходит в землю — на склоне не висит), высокий узкий конус.
pub(crate) fn mega_spruce(reg: &mut Region, x: i32, h: i32, z: i32, t: i32) {
    if !reg.touches(x, z, 6) {
        return;
    }
    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        trunk(reg, x + dx, h - 2, h + t, z + dz, SPRUCE_LOG);
    }
    for dy in t / 3..=t + 2 {
        let k = t + 2 - dy;
        let rad = ((k as f32 * 0.32 + 1.0).min(4.2) - if k % 3 == 1 { 0.9 } else { 0.0 }).max(0.8);
        let ri = rad.ceil() as i32;
        let (ax, bx, az, bz) = reg.span(x, z, -ri, ri + 1);
        for dx in ax..=bx {
            for dz in az..=bz {
                let (fx, fz) = (dx as f32 - 0.5, dz as f32 - 0.5);
                if fx * fx + fz * fz <= rad * rad + 0.3 {
                    reg.put(x + dx, h + dy, z + dz, SPRUCE_LEAVES, 0);
                }
            }
        }
    }
}

/// Что лежит сверху клетки дальней плитки: (высота верхнего блока, слой верха, слой бока, вода ли).
/// Те же правила поверхности, что у кусков (slope — перепад к соседней клетке на блок).
pub(crate) fn lod_top(c: &Column, slope: i32) -> (i32, u32, u32, bool) {
    let water = c.h < SEA;
    let (top, side) = match c.biome {
        Biome::Ocean => (if SEA - c.h > 9 { L_GRAVEL } else { L_SAND }, L_SAND),
        Biome::River => (L_SAND, L_SAND),
        Biome::Beach if c.var == Var::Stony => (if c.det > 0.15 { L_GRAVEL } else { L_STONE }, L_STONE),
        Biome::Desert if slope >= 3 => (L_SANDSTONE_TOP, L_SANDSTONE),
        Biome::Beach | Biome::Desert => (L_SAND, L_SAND),
        // Снег издали: бока невысоких ступенек — тем же снегом (как трава ниже), камень — только на обрывах; иначе
        // каждая ступенька в 1 блок рисовалась серой (среднее «снег на земле»/камня) пунктирной горизонталью.
        Biome::Peaks => (if slope >= 5 { L_STONE } else { L_SNOW }, if slope >= 4 { L_STONE } else { L_SNOW }),
        Biome::Mountain => {
            if slope >= 3 { (L_STONE, L_STONE) } else { (L_MTN_TOP, L_STONE) }
        }
        Biome::Snowy => (L_SNOW, if slope >= 4 { L_STONE } else { L_SNOW }),
        // Дно болотной заводи — земля.
        _ if water => (L_DIRT, L_DIRT),
        // Трава (с оттенком lod_tint): верх как у кусков, бока — травой (склон издали), обрыв — камнем.
        _ => (L_GRASS_TOP, if slope >= 4 { L_STONE } else { L_GRASS_FULL }),
    };
    // Лес издали — полог крон над землёй, пятнами.
    if !water {
        if let Some((up, leaf)) = lod_canopy(c) {
            return (c.h + up, leaf, leaf, false);
        }
    }
    (c.h, top, side, water)
}

/// Полог крон дальней клетки: (высота над землёй, слой листвы). Доля клеток под кроной — по густоте деревьев.
pub(crate) fn lod_canopy(c: &Column) -> Option<(i32, u32)> {
    let (up, leaf) = match (c.biome, c.var) {
        (Biome::Forest, Var::OldGrowth) => (8, L_LEAVES),
        (Biome::Forest | Biome::Plains, _) => (5, L_LEAVES),
        (Biome::Birch, Var::OldGrowth) => (9, L_BIRCH_LEAVES),
        (Biome::Birch, _) => (6, L_BIRCH_LEAVES),
        (Biome::Taiga, Var::OldGrowth) => (11, L_SPRUCE_LEAVES),
        (Biome::Taiga, _) => (7, L_SPRUCE_LEAVES),
        _ => return None,
    };
    // Густота 0.55 → порог −0.3 (почти сплошь), реже — меньше клеток с кроной; редколесье издали — без полога.
    let d = tree_density(c);
    if d >= 0.2 && c.det > 0.5 - d * 1.45 {
        Some((up, leaf))
    } else {
        None
    }
}

/// Оттенок дальней клетки (rgb565), 0xFFFF — без оттенка. Тот же, что у столбца куска (трава и листва).
pub(crate) fn lod_tint(c: &Column) -> u32 {
    match c.biome {
        Biome::Plains | Biome::Forest | Biome::Birch | Biome::Taiga | Biome::Savanna if c.h >= SEA => {
            rgb565(grass_tint(c)) as u32
        }
        _ => 0xFFFF,
    }
}

// ---------------------------------------------------------------- проверки генератора (карта, скорость)

#[cfg(test)]
mod gen_tools {
    use super::*;

    fn biome_rgb(b: Biome) -> [f32; 3] {
        match b {
            Biome::Ocean => [0.10, 0.25, 0.55],
            Biome::Beach => [0.86, 0.82, 0.55],
            Biome::River => [0.25, 0.50, 0.85],
            Biome::Plains => [0.55, 0.75, 0.35],
            Biome::Forest => [0.20, 0.48, 0.18],
            Biome::Birch => [0.40, 0.62, 0.30],
            Biome::Taiga => [0.22, 0.42, 0.35],
            Biome::Snowy => [0.85, 0.90, 0.92],
            Biome::Savanna => [0.70, 0.66, 0.30],
            Biome::Desert => [0.92, 0.80, 0.45],
            Biome::Mountain => [0.50, 0.50, 0.48],
            Biome::Peaks => [0.97, 0.97, 1.0],
        }
    }

    /// Карта: биом цветом, рельеф — отмывкой; вода — по глубине. Пишет PPM (P6) и печатает сводку.
    fn map(gen: &Gen, out: &str, cx: i32, cz: i32, step: i32, n: i32) {
        let mut hs = vec![0i32; (n * n) as usize];
        let mut bs = vec![Biome::Ocean; (n * n) as usize];
        let mut vs = vec![Var::None; (n * n) as usize];
        for j in 0..n {
            for i in 0..n {
                let c = gen.column(cx + (i - n / 2) * step, cz + (j - n / 2) * step);
                hs[(j * n + i) as usize] = c.h;
                bs[(j * n + i) as usize] = c.biome;
                vs[(j * n + i) as usize] = c.var;
            }
        }
        let mut img = Vec::with_capacity((n * n * 3) as usize);
        let mut hist = [0u32; 16];
        let mut vars = [0u32; 6];
        let mut counts = std::collections::BTreeMap::new();
        for j in 0..n {
            for i in 0..n {
                let k = (j * n + i) as usize;
                let h = hs[k];
                let hx = hs[(j * n + (i + 1).min(n - 1)) as usize] - hs[(j * n + (i - 1).max(0)) as usize];
                let hz = hs[((j + 1).min(n - 1) * n + i) as usize] - hs[((j - 1).max(0) * n + i) as usize];
                let shade = (1.0 - (hx + hz) as f32 * 0.6 / step as f32).clamp(0.45, 1.5);
                let mut c = match vs[k] {
                    Var::Meadow => [0.85, 0.75, 0.40],
                    Var::Swamp => [0.30, 0.38, 0.22],
                    Var::Stony => [0.45, 0.45, 0.45],
                    Var::OldGrowth => [0.10, 0.32, 0.12],
                    Var::Sparse => [0.45, 0.62, 0.25],
                    Var::None => biome_rgb(bs[k]),
                };
                if h < SEA {
                    let d = (SEA - h) as f32 / 30.0;
                    c = [c[0] * (1.0 - 0.5 * d), c[1] * (1.0 - 0.5 * d), c[2] * (1.0 - 0.3 * d)];
                } else {
                    let a = 0.75 + 0.35 * ((h - SEA) as f32 / 70.0);
                    c = [c[0] * a * shade, c[1] * a * shade, c[2] * a * shade];
                }
                for v in c {
                    img.push((v.clamp(0.0, 1.0) * 255.0) as u8);
                }
                hist[(h / 8) as usize] += 1;
                vars[vs[k] as usize] += 1;
                *counts.entry(bs[k] as u8).or_insert(0u32) += 1;
            }
        }
        // Озёра: связные области воды, не касающиеся края карты.
        let mut seen = vec![false; (n * n) as usize];
        let (mut lakes, mut lake_px) = (0, 0);
        for s in 0..(n * n) as usize {
            if seen[s] || hs[s] >= SEA {
                continue;
            }
            let mut st = vec![s];
            seen[s] = true;
            let (mut area, mut border) = (0, false);
            while let Some(p) = st.pop() {
                area += 1;
                let (i, j) = (p as i32 % n, p as i32 / n);
                if i == 0 || j == 0 || i == n - 1 || j == n - 1 {
                    border = true;
                }
                for (di, dj) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let (a, b) = (i + di, j + dj);
                    if a < 0 || b < 0 || a >= n || b >= n {
                        continue;
                    }
                    let q = (b * n + a) as usize;
                    if !seen[q] && hs[q] < SEA {
                        seen[q] = true;
                        st.push(q);
                    }
                }
            }
            if !border && area * step * step < 400_000 {
                lakes += 1;
                lake_px += area;
            }
        }
        let tot = (n * n) as f32;
        let names = ["Ocean", "Beach", "River", "Plains", "Forest", "Birch", "Taiga", "Snowy", "Savanna", "Desert", "Mountain", "Peaks"];
        let mut s = format!("{out}: lakes {lakes} ({:.1}% area); ", lake_px as f32 / tot * 100.0);
        for (b, c) in &counts {
            s += &format!("{} {:.1}% ", names[*b as usize], *c as f32 / tot * 100.0);
        }
        s += &format!("\n  max h {}; meadow {:.1}% sparse {:.1}% old {:.1}% swamp {:.1}% stony {:.1}%", hs.iter().max().unwrap(),
            vars[1] as f32 / tot * 100.0, vars[2] as f32 / tot * 100.0, vars[3] as f32 / tot * 100.0, vars[4] as f32 / tot * 100.0, vars[5] as f32 / tot * 100.0);
        s += "\n  h/8:";
        for (i, v) in hist.iter().enumerate() {
            s += &format!(" {}:{:.1}", i * 8, *v as f32 / tot * 100.0);
        }
        println!("{s}");
        let mut f = format!("P6 {n} {n} 255\n").into_bytes();
        f.extend(img);
        std::fs::write(out, f).unwrap();
    }

    #[test]
    #[ignore]
    fn gen_map() {
        let dir = std::env::var("GEN_MAP").unwrap_or_else(|_| ".".into());
        let gen = Gen::new(1337);
        map(&gen, &format!("{dir}/map_world.ppm"), 0, 0, 24, 512);
        map(&gen, &format!("{dir}/map_mid.ppm"), 0, 0, 6, 512);
        map(&gen, &format!("{dir}/map_near.ppm"), 0, 0, 1, 384);
    }
}

#[cfg(test)]
mod gen_bench {
    use super::*;
    use crate::world::World;
    use std::hint::black_box;
    use std::time::Instant;

    fn best<F: FnMut()>(reps: usize, n: usize, mut f: F) -> f64 {
        (0..reps)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1000.0 / n as f64
            })
            .fold(f64::MAX, f64::min)
    }

    /// Скорость генератора и число квадов (замена треугольникам кадра): лучшее из нескольких повторов.
    #[test]
    #[ignore]
    fn gen_bench() {
        let w = World::new(1337);
        let gen = Gen::new(1337);
        let build = best(5, 400, || {
            for i in 0..400 {
                black_box(w.build(i % 20 - 10, i / 20 - 10));
            }
        });
        let gen_near = best(5, 400, || {
            for i in 0..400 {
                black_box(generate(&gen, i % 20 - 10, i / 20 - 10));
            }
        });
        let gen_wide = best(5, 300, || {
            for i in 0..300 {
                black_box(generate(&gen, (i % 20) * 37 - 370, (i / 20) * 41 - 300));
            }
        });
        let col = best(5, 200_000, || {
            let mut s = 0;
            for i in 0..200_000 {
                s += gen.column((i % 631) * 13 - 4000, (i / 631) * 11 - 2000).h;
            }
            black_box(s);
        });
        let col_land = best(5, 200_000, || {
            let mut s = 0;
            for i in 0..200_000 {
                s += gen.column((i % 500) * 2 - 500, (i / 500) * 2 - 400).h;
            }
            black_box(s);
        });
        let lod = best(5, 200, || {
            for i in 0..200 {
                black_box(w.build_lod(i % 10, i / 10, 3));
            }
        });
        println!("TIME build {build:.3} ms/chunk; generate near {gen_near:.3} wide {gen_wide:.3} ms; column {:.0} ns, land {:.0} ns; lod L3 {lod:.3} ms/tile",
            col * 1e6, col_land * 1e6);
        // Квады: куски на большой площади и плитки каждого уровня.
        let (mut sq, mut cq, mut wq) = (0usize, 0usize, 0usize);
        let nc = 600i32;
        for i in 0..nc {
            let m = w.build((i % 30) * 23 - 340, (i / 30) * 29 - 290);
            sq += m.solid.len() / 8;
            cq += m.cutout.len() / 8;
            wq += m.water.len() / 8;
        }
        let near = {
            let (mut a, mut b) = (0usize, 0usize);
            for i in 0..400 {
                let m = w.build(i % 20 - 10, i / 20 - 10);
                a += m.solid.len() / 8;
                b += m.cutout.len() / 8;
            }
            (a / 400, b / 400)
        };
        // Путь бенчмарка (от (8, 8) на восток 800 блоков) и только суша на большой площади.
        let path = {
            let (mut a, mut b, mut n) = (0usize, 0usize, 0usize);
            for cz in -6..=6 {
                for cx in -2..=52 {
                    let m = w.build(cx, cz);
                    a += m.solid.len() / 8;
                    b += m.cutout.len() / 8;
                    n += 1;
                }
            }
            (a / n, b / n)
        };
        let land = {
            let (mut a, mut b, mut n) = (0usize, 0usize, 0usize);
            for i in 0..nc {
                let (cx, cz) = ((i % 30) * 23 - 340, (i / 30) * 29 - 290);
                if gen.column(cx * 16 + 8, cz * 16 + 8).h >= SEA {
                    let m = w.build(cx, cz);
                    a += m.solid.len() / 8;
                    b += m.cutout.len() / 8;
                    n += 1;
                }
            }
            (a / n.max(1), b / n.max(1), n)
        };
        let mut s = format!("QUADS chunk wide solid {} cutout {} water {}; land solid {} cutout {} ({} chunks); near spawn solid {} cutout {}; path solid {} cutout {}; lod per tile:",
            sq / nc as usize, cq / nc as usize, wq / nc as usize, land.0, land.1, land.2, near.0, near.1, path.0, path.1);
        for level in 1..=5u32 {
            let span = 2400 / (16 << level) as i32; // ~±2400 блоков
            let mut q = 0usize;
            let n = 144i32;
            for i in 0..n {
                let m = w.build_lod((i % 12) * span / 6 - span, (i / 12) * span / 6 - span, level);
                q += m.solid.len() / 8;
            }
            s += &format!(" L{level} {}", q / n as usize);
        }
        println!("{s}");
    }
}

#[cfg(test)]
mod ore_tests {
    use super::*;

    /// Руды по высотам на n × n кусках: [y] → (уголь, железо).
    fn census(gen: &Gen, c0: (i32, i32), n: i32) -> Vec<(u32, u32)> {
        let mut by_y = vec![(0u32, 0u32); H as usize];
        for cz in c0.1..c0.1 + n {
            for cx in c0.0..c0.0 + n {
                let reg = generate(gen, cx, cz);
                for lz in PAD..PAD + CS {
                    for lx in PAD..PAD + CS {
                        for y in 0..H {
                            match reg.get(lx, y, lz) {
                                COAL_ORE => by_y[y as usize].0 += 1,
                                IRON_ORE => by_y[y as usize].1 += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        by_y
    }

    #[test]
    fn probe_reference() {
        spawn::site_on(); // проба — как у сборок: с Перекрёстком
        // Эталон генерации (GEN_VERSION 4: руды и лава M1 + Перекрёсток в куске (0, 0); libm): те же биты дают сборка wasm32 (tools: прогон pkg в node — отчёт MP-0)
        // и нативный сервер. Расхождение — генератор изменился (поднять GEN_VERSION и эталон осознанно) или
        // реализация разошлась между платформами (искать встроенные sin/cos, порядок HashMap).
        let p = probe();
        println!("probe {:#018x?}", p);
        assert_eq!(p, PROBE_REF);
    }
    const PROBE_REF: [u64; 4] = [0x433b_5b0e_f285_6c41, 0x0e00_9b2a_a4b1_3193, 0xf9b2_1370_b7f5_0024, 0x45da_32de_09dd_965a];

    #[test]
    fn ores_deterministic_and_seamless() {
        // Тот же сид — те же руды; жилы переходят через край куска (своя область считает и жилы соседей).
        let a = Gen::new(1337);
        let b = Gen::new(1337);
        let mut cross = 0;
        for (cx, cz) in [(0, 0), (3, -2), (-7, 5), (20, 11), (-30, 40)] {
            let ra = generate(&a, cx, cz);
            assert_eq!(ra.blocks, generate(&b, cx, cz).blocks);
            // Восточный сосед: руда в последнем столбце нашего куска и в первом столбце соседа рядом — жила через шов.
            let rn = generate(&a, cx + 1, cz);
            for lz in PAD..PAD + CS {
                for y in 0..H {
                    let ours = ra.get(PAD + CS - 1, y, lz);
                    let theirs = rn.get(PAD, y, lz);
                    if (ours == IRON_ORE || ours == COAL_ORE) && ours == theirs {
                        cross += 1;
                    }
                }
            }
        }
        assert!(cross > 0, "жилы через край куска есть");
        // Другой сид — другие руды.
        assert_ne!(generate(&Gen::new(7), 0, 0).blocks, generate(&a, 0, 0).blocks);
    }

    #[test]
    fn ore_frequencies_by_height() {
        // 12 × 12 кусков у точки появления (равнины и леса, море 48): железо ниже ~58 с пиком у 30 (iron_middle) и
        // ровным слоем 0…58 (iron_small), уголь — 24…96 с пиком у 60; выше рельефа руд нет; только вместо камня.
        let gen = Gen::new(1337);
        let by_y = census(&gen, (-6, -6), 12);
        let (coal, iron): (u32, u32) = by_y.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        println!("руды на 144 кусках: уголь {coal} ({:.1} на кусок), железо {iron} ({:.1} на кусок)", coal as f64 / 144.0, iron as f64 / 144.0);
        let band = |lo: usize, hi: usize, k: usize| -> u32 { by_y[lo..hi].iter().map(|v| if k == 0 { v.0 } else { v.1 }).sum() };
        for y in (0..H as usize).step_by(8) {
            println!("y {:3}…{:3}: уголь {:5}, железо {:5}", y, y + 7, band(y, y + 8, 0), band(y, y + 8, 1));
        }
        assert!(coal > 1000 && iron > 1000, "уголь {coal}, железо {iron}");
        assert_eq!(band(0, 20, 0), 0, "угля ниже 20 нет (coal_lower с 24; жила — до 2 + r ниже точки)");
        assert!(band(20, 41, 1) > band(41, 62, 1), "железа у пика 30 больше, чем у поверхности");
        assert!(band(52, 68, 0) > band(24, 40, 0), "уголь гуще к 60, чем у дна");
        assert_eq!(band(100, 128, 0) + band(100, 128, 1), 0, "над рельефом равнин руд нет");
    }

    #[test]
    #[ignore]
    fn ore_cost() {
        // Цена руд в генерации: те же куски с рудами и без, вперемешку (лучшее из 7); рядом (как группа воркера) и
        // вразброс (кэш жил не помогает).
        let gen = Gen::new(1337);
        let near: Vec<(i32, i32)> = (0..400).map(|i| (i % 20 - 10, i / 20 - 10)).collect();
        let wide: Vec<(i32, i32)> = (0..300).map(|i| ((i % 20) * 37 - 370, (i / 20) * 41 - 300)).collect();
        for (name, list) in [("рядом", &near), ("вразброс", &wide)] {
            let mut t = [f64::MAX; 2];
            for _ in 0..7 {
                for off in [false, true] {
                    ORES_OFF.with(|o| o.set(off));
                    let s = std::time::Instant::now();
                    for &(cx, cz) in list.iter() {
                        std::hint::black_box(generate(&gen, cx, cz));
                    }
                    let k = off as usize;
                    t[k] = t[k].min(s.elapsed().as_secs_f64() * 1e3 / list.len() as f64);
                }
            }
            ORES_OFF.with(|o| o.set(false));
            println!("ORES {name}: с рудами {:.3} мс/кусок, без {:.3} — {:+.1} %", t[0], t[1], (t[0] / t[1] - 1.0) * 100.0);
        }
    }
}
